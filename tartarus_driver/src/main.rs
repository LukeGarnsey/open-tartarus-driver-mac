use hidapi::HidApi;
use std::env;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant};

mod config;
mod configui;
#[cfg(windows)]
mod dpad;
mod emulate;
mod hypershift;
mod key;
mod lighting;
mod platform;
mod remap;
#[cfg(windows)]
mod tray;
#[cfg(target_os = "macos")]
mod tray_macos;
use key::Key;
// Every synthetic keystroke the crate emits goes through the OS backend
// selected in platform/mod.rs; `crate::send_key` stays the crate-wide name
// for it.
pub(crate) use platform::send_key;

// Public repo's Releases page — where a newer version, if any, would be
// published. This project has no auto-update check of its own (no telemetry,
// no background network calls by design); the tray menu item just saves the
// user from having to remember the URL.
pub const RELEASES_URL: &str = "https://github.com/ultramonaka/open-tartarus-driver/releases";
// Where `configui`'s local web server listens (see configui.rs).
pub const CONFIGUI_URL: &str = "http://127.0.0.1:7878/";

// ===========================================================================
// Locating config.toml / logs/run.log relative to where the binary is
// actually running from
// ===========================================================================
//
// A prior version resolved these paths at COMPILE time via
// `env!("CARGO_MANIFEST_DIR")`. That bakes in the absolute path of whichever
// machine built the binary — harmless for a local `cargo build`, but it means
// a binary built on a CI runner (e.g. `D:\a\open-tartarus-driver\...`) ships
// with that CI path hardcoded, so config.toml/run.log can never be found on
// a user's machine no matter where the exe is placed (confirmed in the wild
// via the GitHub Actions release build). Resolved at runtime instead:
//   - Distributed build: the exe sits next to config.toml/logs/ (the
//     packaged release layout), so its own directory is the right base.
//   - Dev build (`cargo run`/`cargo build`, debug or release): the exe lives
//     under `tartarus_driver/target/<profile>/`, so walk back up past
//     `target/<profile>` and the crate dir to the repo root, matching where
//     config.toml has always lived for development.
pub fn app_root() -> std::path::PathBuf {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    // macOS .app bundle: writing next to the exe would break the bundle's
    // code signature, so use ~/Library/Application Support instead (see
    // platform/macos.rs::bundle_app_root).
    #[cfg(target_os = "macos")]
    if let Some(root) = platform::bundle_app_root(&exe_dir) {
        return root;
    }
    let looks_like_build_profile_dir = exe_dir
        .file_name()
        .is_some_and(|n| n == "debug" || n == "release");
    if looks_like_build_profile_dir
        && let Some(target_dir) = exe_dir.parent()
        && target_dir.file_name().is_some_and(|n| n == "target")
        && let Some(repo_root) = target_dir.parent().and_then(|p| p.parent())
    {
        return repo_root.to_path_buf();
    }
    exe_dir
}

pub fn config_path() -> std::path::PathBuf {
    app_root().join("config.toml")
}

fn log_path() -> std::path::PathBuf {
    app_root().join("logs").join("run.log")
}

// v1.0.6 hot-reload: config.toml's current mtime, or None if it can't be
// stat'd (missing, or some other I/O error) — a plain sentinel rather than
// panicking/crashing, since "no file" is an ordinary, already-handled state
// (config::load()/try_reload() both fall back gracefully). Comparing two
// `Option<SystemTime>` values with `!=` also means a config.toml that's
// created for the first time WHILE the driver is already running (None ->
// Some(...)) is correctly detected as a change, not just edits to an
// existing file.
fn config_mtime_now() -> Option<std::time::SystemTime> {
    std::fs::metadata(config_path()).and_then(|m| m.modified()).ok()
}

// ===========================================================================
// Always-on file logging
// ===========================================================================
//
// Piping stdout through `Tee-Object` from PowerShell has repeatedly failed in
// practice (wrong shell cwd, mangled multi-line pastes, etc.), losing test
// output. So the program writes its own log directly, independent of however
// it's invoked.
//
// The actual disk write happens on its own background thread, off of
// whichever thread called println!/eprintln!. This matters because the
// D-pad/wheel/Hypershift path (dpad.rs) and the analog key-read loop below
// both log on every single keystroke/wheel-notch transition, on the same
// thread that's also responsible for actually forwarding/remapping that
// event as fast as possible — a synchronous file write (a syscall, however
// fast) sitting in that path directly adds to input latency, which runs
// against this project's whole point. println!/eprintln! now only do a
// cheap in-memory channel send; the writer thread does the real I/O
// whenever it gets to it, however far behind that ends up being.
static LOG_SENDER: OnceLock<std::sync::mpsc::Sender<String>> = OnceLock::new();

fn init_log_file() {
    let path = log_path();
    // `logs/` is gitignored and not part of a fresh clone/release download
    // (see .gitignore), so it may not exist yet; File::create alone would
    // fail since it never creates missing parent directories.
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match std::fs::File::create(&path) {
        Ok(mut file) => {
            let (tx, rx) = std::sync::mpsc::channel::<String>();
            let _ = LOG_SENDER.set(tx);
            std::thread::spawn(move || {
                use std::io::{Seek, Write};
                // `tray` mode is meant to run for days at a time, and every
                // keystroke/wheel-notch transition logs a line — cap the
                // file at ~5 MiB instead of growing it unbounded for the
                // life of the process. Restarting the file (rather than
                // deleting/renaming) keeps this thread's single open handle
                // valid throughout.
                const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;
                let mut written: u64 = 0;
                for line in rx {
                    if written >= MAX_LOG_BYTES {
                        let _ = file.set_len(0);
                        let _ = file.rewind();
                        let _ = file.write_all(b"[log truncated at ~5 MiB; continuing]\n");
                        written = 0;
                    }
                    written += line.len() as u64 + 1;
                    let _ = file.write_all(line.as_bytes());
                    let _ = file.write_all(b"\n");
                }
            });
            std::println!("Logging to {}", path.display());
        }
        Err(e) => std::eprintln!(
            "WARNING: could not open log file {}: {e} (console-only)",
            path.display()
        ),
    }
}

// Shadow println!/eprintln! everywhere below this point so every existing
// call site gets file logging for free, with zero other changes needed.
// #[macro_export] (rather than relying on textual scoping) so config/'s
// submodules / configui.rs can use them too via `crate::{println, eprintln}`
// regardless of where `mod config;` etc. appear relative to these definitions.
#[macro_export]
macro_rules! println {
    () => {{ std::println!(); }};
    ($($arg:tt)*) => {{
        let s = format!($($arg)*);
        std::println!("{s}");
        if let Some(tx) = $crate::LOG_SENDER.get() {
            let _ = tx.send(s);
        }
    }};
}
#[macro_export]
macro_rules! eprintln {
    () => {{ std::eprintln!(); }};
    ($($arg:tt)*) => {{
        let s = format!($($arg)*);
        std::eprintln!("{s}");
        if let Some(tx) = $crate::LOG_SENDER.get() {
            let _ = tx.send(s);
        }
    }};
}

// Phase 4: loaded at startup (config.toml if present, else built-in
// placeholder defaults — see config/load.rs). v1.0.6: an `RwLock<Option<Arc<_>>>`
// rather than the earlier `OnceLock` specifically so `run_driver`'s loop can
// hot-swap it when config.toml changes on disk (see its ~1s mtime-poll
// block) without needing a reference threaded through every function —
// the Interception thread and the main analog-read loop both just call
// `cfg()` fresh whenever they need it. Reads (`cfg()`) are a cheap
// refcount-bump `Arc` clone; writes (reloads) happen at most ~once/sec, so
// there's no meaningful lock contention either way.
static CONFIG: RwLock<Option<Arc<config::DriverConfig>>> = RwLock::new(None);
fn cfg() -> Arc<config::DriverConfig> {
    CONFIG
        .read()
        .unwrap()
        .clone()
        .expect("CONFIG must be set at the start of main() before anything reads it")
}
fn set_cfg(new_cfg: config::DriverConfig) {
    *CONFIG.write().unwrap() = Some(Arc::new(new_cfg));
}

pub(crate) const VID: u16 = 0x1532;
pub(crate) const PID: u16 = 0x0244;

// Interface 1 / endpoint 0x82 emits this report ID for the 20 analog keys.
// Reverse-engineered via USBPcap capture on 2026-07-18 (docs/reference/logs/capture.pcap):
// byte[0] = report ID, byte[1..=20] = one 0-255 depth value per physical key.
// Confirmed 2026-07-18 (docs/reference/logs/keymap_log.txt): byte offset N == the number
// printed on keycap N (identity mapping, no permutation).
const ANALOG_REPORT_ID: u8 = 0x06;
const NUM_KEYS: usize = 20;

// Phase v1.0.5 (Hyper Shift redesign): the analog keymap now holds up to 3
// layers (index 0 = Default, 1 = Layer1, 2 = Layer2) instead of two fixed
// fields. Toggle-style Hypershift can cycle through 2 or 3 of them
// (config.toml's [hypershift] layer_count); momentary style always uses
// exactly layers 0/1 regardless of layer_count (see hypershift.rs).
pub const MAX_LAYERS: usize = 3;

// Hysteresis thresholds per docs/DESIGN.md §6.1 (recommended values). Phase 4:
// these are now specifically the BUILT-IN DEFAULT used by the config module
// whenever config.toml has no [actuation] section (or an invalid t_on/t_off
// pair) — see config::DriverConfig::defaults(). The actual analog loop below
// always reads the live values via cfg().actuation, never these consts
// directly.
const T_ON: u8 = 100;
const T_OFF: u8 = 80;

// TEST/PLACEHOLDER keymap — not a real layout, just enough to prove the
// hysteresis + SendInput pipeline end to end. key01..key20 -> '1'..'9','0','A'..'J'.
// Phase 4: this is now specifically the BUILT-IN DEFAULT used by the config module
// whenever config.toml doesn't override a given key (or doesn't exist at
// all) — see config::DriverConfig::defaults(). Editing this array changes
// what a machine with no config.toml (or an incomplete one) falls back to.
const TEST_KEYMAP: [Key; NUM_KEYS] = [
    Key::Digit1, // key01 -> '1'
    Key::Digit2, // key02 -> '2'
    Key::Digit3, // key03 -> '3'
    Key::Digit4, // key04 -> '4'
    Key::Digit5, // key05 -> '5'
    Key::Digit6, // key06 -> '6'
    Key::Digit7, // key07 -> '7'
    Key::Digit8, // key08 -> '8'
    Key::Digit9, // key09 -> '9'
    Key::Digit0, // key10 -> '0'
    Key::A,      // key11 -> 'A'
    Key::B,      // key12 -> 'B'
    Key::C,      // key13 -> 'C'
    Key::D,      // key14 -> 'D'
    Key::E,      // key15 -> 'E'
    Key::F,      // key16 -> 'F'
    Key::G,      // key17 -> 'G'
    Key::H,      // key18 -> 'H'
    Key::I,      // key19 -> 'I'
    Key::J,      // key20 -> 'J'
];

// TEST/PLACEHOLDER Layer1 (Hypershift) keymap — not a real layout, just enough
// to prove the layer switch end to end. key01..key20 -> F1..F20 so Layer1 hits
// are trivially distinguishable from the Default layer during testing.
const LAYER1_TEST_KEYMAP: [Key; NUM_KEYS] = [
    Key::F1,
    Key::F2,
    Key::F3,
    Key::F4,
    Key::F5,
    Key::F6,
    Key::F7,
    Key::F8,
    Key::F9,
    Key::F10,
    Key::F11,
    Key::F12,
    Key::F13,
    Key::F14,
    Key::F15,
    Key::F16,
    Key::F17,
    Key::F18,
    Key::F19,
    Key::F20,
];

// TEST/PLACEHOLDER Layer2 (Hypershift toggle, 3rd layer) keymap — same
// throwaway style as TEST_KEYMAP/LAYER1_TEST_KEYMAP, only reachable when
// config.toml sets [hypershift] switch_style="toggle", layer_count=3. Reuses
// key.rs's 20 named specials (LEFT..INSERT) so it's trivially
// distinguishable from both other layers during testing without needing any
// new key vocabulary.
const LAYER2_TEST_KEYMAP: [Key; NUM_KEYS] = [
    Key::Left,
    Key::Up,
    Key::Right,
    Key::Down,
    Key::Space,
    Key::Enter,
    Key::Tab,
    Key::Escape,
    Key::Backspace,
    Key::LShift,
    Key::RShift,
    Key::LCtrl,
    Key::RCtrl,
    Key::LAlt,
    Key::RAlt,
    Key::Home,
    Key::End,
    Key::PageUp,
    Key::PageDown,
    Key::Insert,
];

// Set by platform::install_shutdown_handler's OS hook (Windows: Ctrl+C,
// Ctrl+Break, console window closed, logoff, or shutdown; elsewhere:
// SIGINT/SIGTERM/SIGHUP) and by the tray menu's Quit, so the main
// analog-read loop can notice and exit its own way — running the existing
// "force-release any key still logically held" cleanup at the bottom of
// run_driver — instead of the OS just killing the process outright, which
// would skip that cleanup and could leave a key stuck down. The loop's
// sleep granularity (500us) means this is noticed almost immediately, well
// within the few seconds Windows grants a console handler to actually exit.
pub(crate) static SHUTDOWN_REQUESTED: AtomicBool = AtomicBool::new(false);

// docs/DESIGN.md §6② step 3: on the transition BACK TO Default (from any other
// layer — not on every transition, and specifically not on the Default ->
// Layer1/Layer2 press edge), force-send KeyUp for every key still logically
// down so nothing stays stuck sending a non-Default layer's key after
// returning to Default. (If the physical key is still past T_ON afterwards,
// the next report re-presses it fresh under Default.) A key already held
// when Hyper Shift engages (leaving Default) deliberately keeps sending
// whatever it was pressed with for the rest of that hold — this is the
// original, hardware-verified momentary design, and generalizing it to fire
// on every transition (tried briefly during v1.0.5 development) turned out
// to be a real regression: it force-released+re-pressed an already-held key
// the instant Hyper Shift engaged, sending both the Default and the new
// layer's key back-to-back instead of a clean switch. Shared by the real
// driver loop and `emulate` mode (see emulate.rs) so both exercise this edge
// exactly the same way.
pub(crate) fn force_keyup_on_layer_change(
    pressed_vk: &mut [Option<Key>; NUM_KEYS],
    start: Instant,
) {
    for (i, slot) in pressed_vk.iter_mut().enumerate() {
        if let Some(vk) = slot.take() {
            send_key(vk, true);
            println!(
                "[t={:>8.3}s] key{:02} UP   (forced: Hyper Shift layer changed)",
                start.elapsed().as_secs_f64(),
                i + 1
            );
        }
    }
}

fn layer_name(layer: usize) -> String {
    if layer == 0 {
        "Default".to_string()
    } else {
        format!("Layer{layer}")
    }
}

// Runs the hysteresis + keymap + SendInput decision for one already-parsed
// analog report: `depths[i]` is the 0-255 depth for key(i+1) (report ID 6,
// bytes[1..=NUM_KEYS]). `layer` is the active Hyper Shift layer index (0 =
// Default, per hypershift::CURRENT_LAYER — always < MAX_LAYERS). Shared by
// the real driver loop (fed from an actual HID read) and `emulate` mode (fed
// from a synthetic depth array typed at a terminal — see emulate.rs), so
// both exercise identical logic bit-for-bit.
pub(crate) fn process_key_depths(
    depths: &[u8; NUM_KEYS],
    layer: usize,
    pressed_vk: &mut [Option<Key>; NUM_KEYS],
    start: Instant,
) {
    for i in 0..NUM_KEYS {
        let depth = depths[i];
        let (t_on, t_off) = cfg().actuation.for_key(i);
        if pressed_vk[i].is_none() && depth > t_on {
            let vk = cfg().analog.layers[layer][i];
            pressed_vk[i] = Some(vk);
            send_key(vk, false);
            println!(
                "[t={:>8.3}s] key{:02} DOWN (depth={:#04x}, layer={})",
                start.elapsed().as_secs_f64(),
                i + 1,
                depth,
                layer_name(layer)
            );
        } else if depth < t_off
            && let Some(vk) = pressed_vk[i].take()
        {
            send_key(vk, true);
            println!(
                "[t={:>8.3}s] key{:02} UP   (depth={:#04x})",
                start.elapsed().as_secs_f64(),
                i + 1,
                depth
            );
        }
    }
}

// Filters hidapi's device list down to the Tartarus Pro's collections that
// are actually readable: matching VID/PID, minus the two boot collections
// (Usage Page 0x01, Usage 0x02 "Mouse" / 0x06 "Keyboard") that Windows' HID
// class driver claims exclusively (ReadFile on them always fails with
// ACCESS_DENIED). Shared by open_analog_devices below (which exits the
// process if this comes back empty — fine for the driver's fail-fast CLI
// startup) and configui's try_open_analog_devices (which must fail soft
// instead, since it runs inside the long-lived config web server).
pub(crate) fn analog_device_infos(api: &HidApi) -> Vec<hidapi::DeviceInfo> {
    let tartarus = api
        .device_list()
        .filter(|d| d.vendor_id() == VID && d.product_id() == PID);
    #[cfg(not(target_os = "macos"))]
    let infos = tartarus
        .filter(|d| !(d.usage_page() == 0x0001 && (d.usage() == 0x0002 || d.usage() == 0x0006)))
        .cloned();
    // macOS: hidapi reports one entry per (IOHIDDevice, usage pair) rather
    // than one per top-level collection, so an interface shows up several
    // times under the SAME path — and the usage filter above is the wrong
    // tool here: verified 2026-09-16 (examples/mac_probe.rs) that Interface
    // 1's FIRST entry is Keyboard usage (0x0001/0x0006) and that Interface
    // 2 also has a 0x0001/0x0001 entry the filter would let through. hidapi
    // does populate interface_number() on macOS, so select the analog
    // interface by number and open it once.
    #[cfg(target_os = "macos")]
    let infos = {
        let mut seen = std::collections::HashSet::new();
        tartarus
            .filter(|d| d.interface_number() == 1)
            .filter(move |d| seen.insert(d.path().to_owned()))
            .cloned()
    };
    infos.collect()
}

fn open_analog_devices(api: &HidApi) -> Vec<(i32, hidapi::HidDevice)> {
    let infos = analog_device_infos(api);

    if infos.is_empty() {
        eprintln!(
            "Tartarus Pro (VID {:#06x} / PID {:#06x}) not found. Is it plugged in?",
            VID, PID
        );
        std::process::exit(1);
    }

    let mut devices = Vec::new();
    for info in &infos {
        match info.open_device(api) {
            Ok(device) => {
                if let Err(e) = device.set_blocking_mode(false) {
                    eprintln!(
                        "[if{}] failed to set non-blocking mode: {e}",
                        info.interface_number()
                    );
                    continue;
                }
                devices.push((info.interface_number(), device));
            }
            Err(e) => {
                eprintln!(
                    "[if{}] failed to open (skipping): {e}",
                    info.interface_number()
                );
                if let Some(hint) = platform::hid_open_hint(&e) {
                    eprintln!("  {hint}");
                }
            }
        }
    }

    if devices.is_empty() {
        eprintln!("No interfaces could be opened.");
        std::process::exit(1);
    }

    devices
}

// Returned behind Arc<Mutex<..>> because on macOS this same handle is ALSO
// the wheel/middle-click reader: Interface 2 must be seized there so the
// OS stops scrolling on the Tartarus's wheel, and a seized IOHIDDevice
// refuses feature reports from any OTHER handle in the process
// (kIOReturnExclusiveAccess, verified 2026-09-16 with examples/mac_probe.rs
// `dualif2`) — so lighting and the capture thread have to share one. On
// Windows the mutex is simply never contended (Interception does the
// wheel work in dpad.rs, on its own device).
// `seize_for_capture`: macOS only — true from run_driver (the handle
// doubles as the wheel/middle-click reader), false from configui's
// best-effort unlock (a plain shared open; a calibration session must not
// take the wheel away from the OS, and if the driver already holds the
// seize, that unlock simply fails harmlessly). Ignored on other OSes.
#[cfg_attr(not(target_os = "macos"), allow(unused_variables))]
pub(crate) fn open_razer_control_device(api: &HidApi, seize_for_capture: bool) -> Option<Arc<Mutex<hidapi::HidDevice>>> {
    let info = api
        .device_list()
        .find(|d| d.vendor_id() == VID && d.product_id() == PID && d.usage_page() == 0x0001 && d.usage() == 0x0002)?
        .clone();
    // macOS: open Interface 2 exclusively (no root needed for a mouse-usage
    // interface) so its wheel/middle-click reports reach only us. The
    // exclusive flag is process-global in hidapi, hence flipped back
    // immediately; if the seize fails (some other process holds the
    // interface), fall back to a shared open so lighting still works and
    // let platform::spawn_input_capture warn that the wheel isn't remapped.
    #[cfg(target_os = "macos")]
    let opened = if seize_for_capture {
        api.set_open_exclusive(true);
        let seized = info.open_device(api);
        api.set_open_exclusive(false);
        match seized {
            Ok(d) => Ok(d),
            Err(e) => {
                eprintln!("[razer] Interface 2 could not be seized ({e}); opening shared instead.");
                info.open_device(api)
            }
        }
    } else {
        info.open_device(api)
    };
    #[cfg(not(target_os = "macos"))]
    let opened = info.open_device(api);
    match opened {
        Ok(d) => Some(Arc::new(Mutex::new(d))),
        Err(e) => {
            eprintln!("[razer] Interface 2 (Razer Control Device) open failed: {e}");
            if let Some(hint) = platform::hid_open_hint(&e) {
                eprintln!("  {hint}");
            }
            None
        }
    }
}

// Build an arbitrary razer_report (91 bytes incl. leading report-ID 0 byte).
// CRC = XOR of struct bytes 2..88 (i.e. buf[3..89] here, after the report-ID byte).
fn build_razer_cmd(txn: u8, class: u8, cmd: u8, args: &[u8]) -> [u8; 91] {
    let mut buf = [0u8; 91];
    buf[2] = txn;
    buf[6] = args.len() as u8; // data_size
    buf[7] = class;
    buf[8] = cmd;
    buf[9..9 + args.len()].copy_from_slice(args);
    let mut crc = 0u8;
    for b in &buf[3..89] {
        crc ^= *b;
    }
    buf[89] = crc;
    buf
}

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() {
    init_log_file();
    println!("tartarus_driver v{VERSION}");

    let subcommand = env::args().nth(1);
    // macOS: double-clicking "Tartarus Driver.app" in Finder launches the
    // binary with no arguments, and a .app has no console to run the
    // normal mode in — so a bundled launch without a subcommand means
    // `tray`. A bare CLI binary keeps the usual "no argument = run in the
    // terminal until Ctrl+C" behaviour.
    #[cfg(target_os = "macos")]
    let subcommand = subcommand.or_else(|| {
        let exe_dir = std::env::current_exe().ok()?;
        platform::bundle_app_root(exe_dir.parent()?).map(|_| "tray".to_string())
    });
    match subcommand.as_deref() {
        Some("configui") => {
            configui::run_configui_server();
            return;
        }
        Some("tray") => {
            run_tray_mode();
            return;
        }
        Some("emulate") => {
            emulate::run_emulator();
            return;
        }
        _ => {}
    }

    // Historical one-shot investigation subcommands (`razerheartbeat`,
    // `razermode`, `razerinit`/`razerburst`, `enumall`, and the earlier
    // `rawinputlog`) were removed 2026-07-20 once Phase 1-3 and the
    // Interception-based D-pad/wheel/middle-click remap were all fully
    // verified on real hardware and superseded them — their findings are
    // preserved in `docs/research_internal.md` and this file's other doc comments
    // (e.g. the device-mode-3 unlock sequence right below, and the
    // Interception module doc comment above `run_interception_thread`).
    // Interception's own per-event "[dpad] Interception device N hardware
    // id: ... -> TARTARUS/other" log line gives the same device-
    // classification observability during normal operation that
    // `rawinputlog`/`enumall` used to provide standalone.

    // No argument (the normal day-to-day invocation) -> run indefinitely,
    // stopped only by Ctrl+C/console close (see console_ctrl_handler below).
    // An explicit numeric argument still time-boxes the run, as before —
    // useful for scripted tests. 0 explicitly also means "forever".
    let duration_secs: u64 = subcommand.and_then(|s| s.parse().ok()).unwrap_or(0);
    let run_forever = duration_secs == 0;

    platform::install_shutdown_handler();

    run_driver(run_forever, duration_secs);
}

// `tray` subcommand: a background, console-window-free mode with a system
// tray icon instead (see tray.rs). Detaches the console (best-effort —
// there may not even be one, e.g. if launched from a shortcut) so this
// doesn't leave a window open, starts configui's web server so the tray
// menu's "設定を開く" always has something to point the browser at, spawns
// the tray icon itself, then runs the exact same driver loop as the normal
// path — indefinitely, stopped by the tray menu's "終了" (or Ctrl+C, on the
// off chance a console is still attached after all).
fn run_tray_mode() {
    platform::detach_console();

    std::thread::spawn(configui::run_configui_server);
    #[cfg(windows)]
    {
        tray::spawn_tray_icon_thread();
        run_driver(true, 0);
    }
    // macOS: AppKit wants the menu-bar icon on the main thread inside a
    // running NSApplication loop, so the roles flip — the driver loop moves
    // to a worker thread and the tray owns the main thread (see
    // tray_macos.rs). Whichever way shutdown is requested ("終了", Ctrl+C,
    // SIGTERM), the driver thread finishes its stuck-key cleanup and then
    // ends the process; `run_menu_bar` itself never returns.
    #[cfg(target_os = "macos")]
    {
        platform::install_shutdown_handler();
        std::thread::spawn(|| {
            run_driver(true, 0);
            std::process::exit(0);
        });
        tray_macos::run_menu_bar();
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        platform::install_shutdown_handler();
        println!("tray mode: no tray icon on this platform — settings page at {CONFIGUI_URL}");
        run_driver(true, 0);
    }
}

// The actual analog-key-read + hysteresis + SendInput driver loop, shared by
// the normal (console) invocation and `tray` mode. `duration_secs` is
// ignored when `run_forever` is true.
fn run_driver(run_forever: bool, duration_secs: u64) {
    // Before touching the device: on macOS this triggers the Accessibility
    // prompt and logs what to do if synthetic keys are being dropped.
    platform::check_input_permissions();

    let api = HidApi::new().expect("hidapi init failed");

    // Reverse-engineered 2026-07-18 (see try_razer_mode / docs/research_internal.md
    // §2): the device only streams analog reports on Interface 1
    // after Interface 2 (Razer Control Device) is told to enter "device mode 3"
    // via this class-0x00/cmd-0x04 feature report. Synapse sends this at
    // startup and mode 0 on exit; this is the *entire* lock/unlock mechanism —
    // no Synapse process needs to be running, we just need to send this once.
    let devices = open_analog_devices(&api);

    // Phase 4: load config.toml (or built-in placeholder defaults) once,
    // before either the Hypershift hook thread or the Interception thread
    // starts — both read it via cfg() fresh on every event, and v1.0.6's
    // hot-reload (below) is the only thing that ever calls set_cfg() again
    // after this. Loaded here (rather than after the unlock block below)
    // because the lighting command, if any is configured, is sent once at
    // startup right alongside the mode-3 unlock, using the same Interface 2
    // handle.
    set_cfg(config::load());
    let mut config_mtime = config_mtime_now();
    let mut last_reload_check = Instant::now();

    // Kept open (not just a local inside this block) for the lifetime of the
    // function: the layer-indicator LED (below) needs to send a command on
    // every Hypershift press/release, using this same Interface 2 handle.
    let ctrl = open_razer_control_device(&api, true);
    match &ctrl {
        Some(ctrl) => {
            let ctrl = ctrl.lock().unwrap();
            let cmd = build_razer_cmd(0x01, 0x00, 0x04, &[0x03, 0x00]);
            match ctrl.send_feature_report(&cmd) {
                Ok(()) => println!("Sent device-mode-3 unlock command to Interface 2."),
                Err(e) => eprintln!("WARNING: failed to send unlock command: {e} (analog data may not flow)"),
            }
            if let Some(lighting_cfg) = &cfg().lighting {
                lighting::apply(&ctrl, lighting_cfg);
            }
            if let Some(indicator) = &cfg().layer_indicator {
                // Start in the "off" (Default layer) state; the loop below
                // sends the "on" state the moment Hypershift is first held.
                lighting::set_layer_indicator(&ctrl, &indicator.color, false);
            }
        }
        None => eprintln!("WARNING: Interface 2 (Razer Control Device) not found; analog data may not flow."),
    }

    if run_forever {
        println!(
            "Opened {} HID interface(s). Running until Ctrl+C — press keys on the Tartarus Pro now.",
            devices.len()
        );
    } else {
        println!(
            "Opened {} HID interface(s). Running for {duration_secs}s — press keys on the Tartarus Pro now.",
            devices.len()
        );
    }

    // D-pad / wheel / middle-click remap via the Interception kernel driver
    // (see the module doc comment in dpad.rs for the full design, and
    // README.md "既知の制約" for driver install steps). Phase 3 (docs/DESIGN.md
    // §6②) Hypershift trigger detection now lives INSIDE this too (as of
    // 2026-07-21 — see handle_interception_keyboard in dpad.rs): the old
    // unconditional hook-based approach blocked Alt on every keyboard, not
    // just the Tartarus's, breaking real Alt+Tab while the driver ran.
    // dpad::run_interception_thread only falls back to
    // hypershift::spawn_hypershift_hook_thread() itself, internally, if
    // Interception isn't installed/running.
    platform::spawn_input_capture(&ctrl);

    // NOTE: reading these HidDevice handles from a *different* thread than the
    // one that opened them silently returned zero reports in testing on
    // Windows (2026-07-18) even though the exact same read loop works fine
    // on the opening thread. So for now this stays single-threaded: read +
    // hysteresis + SendInput all happen in the same loop. Revisit docs/DESIGN.md's
    // two-thread split later if this turns out to matter for latency.
    // Per-key "logically down" tracking. Some(vk) = down, storing the VK that
    // was actually sent at press time, so KeyUp (normal, forced-by-layer-exit,
    // or forced-at-shutdown) always releases under the keymap the key was
    // pressed with, even if the layer changed in between.
    let mut pressed_vk: [Option<Key>; NUM_KEYS] = [None; NUM_KEYS];
    let mut layer_prev: usize = 0;
    let start = Instant::now();
    let deadline = Duration::from_secs(duration_secs);
    let mut buf = [0u8; 64];

    while !SHUTDOWN_REQUESTED.load(Ordering::SeqCst) && (run_forever || start.elapsed() < deadline) {
        // v1.0.6 hot-reload: check config.toml's mtime at most once/sec (a
        // stat() syscall on every 500us tick would be wasteful; ~1s is
        // still plenty responsive for "saved via configui or a text
        // editor"). try_reload() either returns a fully-parsed new config
        // (swapped in below) or None (old config kept as-is) — see its doc
        // comment in config/load.rs for why a syntax error must never fall back
        // to hardcoded defaults on a live reload the way startup's load()
        // does.
        if last_reload_check.elapsed() >= Duration::from_secs(1) {
            last_reload_check = Instant::now();
            let mtime = config_mtime_now();
            if mtime != config_mtime {
                config_mtime = mtime;
                match config::try_reload() {
                    Some(new_cfg) => {
                        set_cfg(new_cfg);
                        println!("config.toml reloaded — new settings now active.");
                        // Any keymap/actuation/layer meaning a currently-held
                        // key had may no longer be valid under the new
                        // config: force a clean reset, same spirit as the
                        // "returned to Default" edge below.
                        force_keyup_on_layer_change(&mut pressed_vk, start);
                        hypershift::CURRENT_LAYER.store(0, Ordering::SeqCst);
                        if let Some(ctrl) = &ctrl {
                            let ctrl = ctrl.lock().unwrap();
                            if let Some(lighting_cfg) = &cfg().lighting {
                                lighting::apply(&ctrl, lighting_cfg);
                            }
                            if let Some(indicator) = &cfg().layer_indicator {
                                lighting::set_layer_indicator(&ctrl, &indicator.color, false);
                            }
                        }
                    }
                    None => eprintln!(
                        "WARNING: config.toml changed but could not be reloaded (missing or \
                         invalid TOML) — keeping the previous settings until this is fixed."
                    ),
                }
            }
        }

        let layer = hypershift::CURRENT_LAYER.load(Ordering::SeqCst) as usize;

        // On any Hyper Shift layer change: reflect it on the indicator LED
        // (if configured — on for any non-Default layer, off for Default;
        // TASK-009: unverified on real hardware whether this LED actually
        // lights, kept as a harmless opt-in regardless).
        if layer != layer_prev
            && let Some(ctrl) = &ctrl
            && let Some(indicator) = &cfg().layer_indicator
        {
            lighting::set_layer_indicator(&ctrl.lock().unwrap(), &indicator.color, layer != 0);
        }
        // Force-send KeyUp for every key still logically down, but ONLY on
        // the transition back to Default (from any other layer) — NOT on
        // every transition. This matches the original, hardware-verified
        // momentary design exactly (docs/DESIGN.md §6② step 3): a key already
        // held when Hyper Shift engages keeps sending whatever it was
        // pressed with for the rest of that hold, and only gets forcibly
        // reset when the layer returns to Default. v1.0.5 initially
        // generalized this to fire on EVERY transition (including the
        // Default->Layer1 press edge), which turned out to be a real
        // regression: an analog key already held under Default would get an
        // immediate KeyUp+KeyDown pair the instant Hyper Shift engaged,
        // visibly sending BOTH the Default and Layer1 key in quick
        // succession (e.g. "1" then "6") instead of a clean switch —
        // reverted back to this narrower, originally-verified condition.
        if layer_prev != 0 && layer == 0 {
            force_keyup_on_layer_change(&mut pressed_vk, start);
        }
        layer_prev = layer;

        for (_interface, device) in &devices {
            if let Ok(len) = device.read(&mut buf) {
                if len < 1 + NUM_KEYS || buf[0] != ANALOG_REPORT_ID {
                    continue;
                }
                let depths: [u8; NUM_KEYS] = buf[1..1 + NUM_KEYS].try_into().unwrap();
                process_key_depths(&depths, layer, &mut pressed_vk, start);
            }
        }
        std::thread::sleep(Duration::from_micros(500));
    }

    // Safety: force-release any key still held when the loop ends so we
    // never leave a stuck key pressed on the OS.
    for slot in pressed_vk.iter_mut() {
        if let Some(vk) = slot.take() {
            send_key(vk, true);
        }
    }
    remap::release_held();

    println!("Done.");
}
