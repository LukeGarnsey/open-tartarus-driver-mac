# macOS port of open-tartarus-driver (full feature parity)

## Context

`tartarus_driver` is a ~4.2k-line Rust (edition 2024) standalone driver for the Razer Tartarus Pro (VID `0x1532` / PID `0x0244`). It is Windows-only today: the `windows` and `interception` crates are unconditional dependencies, `windows::…::VIRTUAL_KEY` is the crate-wide key type, keystrokes go out through `SendInput`, and D-pad/wheel/middle-click suppression relies on the Interception kernel filter driver. Razer Synapse for macOS does not support the Tartarus Pro at all, so a macOS build is the only way to use the analog keys on a Mac.

Goal: full feature parity on macOS — analog keys + 3-layer remap, D-pad/wheel/middle-click remap, Hyper Shift, LED lighting, tray mode, browser config UI. The user has a Mac and a Tartarus Pro to test on.

Decisions already made:
- **D-pad suppression on macOS = seize the keyboard interface, run as root (`sudo`).** Seizing a keyboard-usage IOHIDDevice requires admin privileges (IOHIDFamily `IOHIDLibUserClient::open`, TN2187; Karabiner/kanata do the same). No CGEventTap correlation path.
- **Work happens in a fork of the upstream repo**, on the Mac. The upstream is GPL-3.0, so the fork stays GPL-3.0 and keeps the author/license notices. Aim to keep the diff upstreamable (cfg-gated backends, Windows behaviour unchanged).
- This Linux box can only do `cargo check --target x86_64-pc-windows-msvc` for the refactor; nothing macOS-specific compiles here (hidapi-sys builds `hid.c` against the Apple SDK).

## What is already portable (no change needed)

- `lighting.rs` — pure hidapi feature reports (`send_feature_report`) + `build_razer_cmd`.
- `configui.rs` — `tiny_http` on `127.0.0.1:7878`, `include_str!("../assets/configui.html")`.
- `config/payload.rs`, most of `config/load.rs`, `config/mod.rs` (except the `VIRTUAL_KEY` fields).
- `hypershift.rs` state machine `on_trigger_edge_with` (L63-99) and `CURRENT_LAYER`.
- `main.rs` logging, `app_root()` (needs a macOS branch, see Phase 2), config hot-reload, hysteresis `process_key_depths`, `build_razer_cmd`, unlock command.
- Config vocabulary: key names are a closed OS-neutral string set (`0-9`, `A-Z`, `F1-F24`, `LEFT/UP/RIGHT/DOWN`, `SPACE ENTER TAB ESCAPE BACKSPACE LSHIFT RSHIFT LCTRL RCTRL LALT RALT HOME END PAGEUP PAGEDOWN INSERT DELETE`, `MEDIA_*`, `VOLUME_*`). Users' `config.toml` files carry over.

## What is Windows-bound (must be abstracted or replaced)

| File | Coupling |
|---|---|
| `main.rs` | `send_key` L348-383 (`SendInput`, scan-code override L335-346); `VIRTUAL_KEY` in keymap tables L225-301 and `pressed_vk`; `SetConsoleCtrlHandler` L313-316/596-604; `FreeConsole` L619 |
| `vkname.rs` | every table entry is a `VK_*` constant; `vk_from_name` assumes `VK_A..Z == ASCII` |
| `dpad.rs` | entirely Interception (`Stroke`, `ScanCode`, HWID string match, `LoadLibraryW`) |
| `hypershift.rs` | `WH_KEYBOARD_LL` fallback hook L109-157 |
| `tray.rs` | hand-rolled Win32 tray + message pump |
| `config/mod.rs`, `config/load.rs`, `emulate.rs` | `VIRTUAL_KEY` type only |
| `Cargo.toml` | `windows`, `interception` unconditional |
| `build.rs` | MSVC `/DELAYLOAD` — already gated, fine |
| `.github/workflows/release.yml` | `windows-latest` only |

## Verified macOS facts that drive the design

- **hidapi on macOS opens every device in exclusive/seize mode by default.** hidapi-rs 2.6 does *not* expose a per-open toggle; the `macos-shared-device` Cargo feature flips the process-global default to non-exclusive. The C symbol `hid_darwin_set_open_exclusive(int)` is linked in and can be declared `extern "C"` to flip it around specific opens.
- **Seizing a keyboard-usage device (primary usage page 1 / usage 6 or 7) needs root**; seizing mouse-usage or vendor-usage devices does not (Input Monitoring permission suffices).
- **hidapi macOS enumeration emits one `DeviceInfo` per (IOHIDDevice, usage pair), all sharing the same `path()`** — must dedupe by path or the same interface gets opened twice.
- macOS delivers input reports to every non-seized opener, so the separate-process `configui` calibration reader keeps working as long as the analog interface is *not* seized.
- Reading a `HidDevice` from another thread is fine on macOS (per-device CFRunLoop thread + queue); `HidDevice: Send`.
- Report-ID framing: Windows hidapi always prepends a report-ID byte; macOS only for numbered reports. Analog report `0x06` is numbered (`buf[0]==0x06` holds on both). Boot-keyboard / mouse reports may be unnumbered on macOS — Phase 0 dumps raw bytes.
- Key emission: CoreGraphics `CGEventCreateKeyboardEvent` + `CGEventPost(kCGHIDEventTap)` with kVK codes; media/volume keys need `NSEvent otherEventWithType:NSEventTypeSystemDefined subtype:8`. Requires **Accessibility** TCC. `enigo` is *not* a fit (no L/R Alt or L/R Cmd distinction, `MEDIA_STOP`/`INSERT`/`F21-F24` cfg'd out on mac) — hand-roll (~150 lines).
- `tray-icon` 0.25 on macOS must be created on the main thread with an `NSApplication` run loop running.
- TCC grants are keyed to code identity: ad-hoc `codesign -s -` loses grants every rebuild; a self-signed cert from Keychain Access (or Developer ID) gives a stable identity. A CLI run from Terminal is attributed to Terminal.app.
- Razer Synapse for Mac / `razer-macos` don't support the Tartarus Pro; still document "quit them" since anything poking interface 2 can flip device mode.
- OpenRazer PR #2710: device-mode 3 (the unlock) caused firmware reset loops on some units — watch for re-enumeration in Phase 0.

## Phases

### Phase 0 — Hardware discovery spike on the Mac (~1-2 days)

Throwaway `tartarus_driver/examples/mac_probe.rs` (not shipped). Record:
1. hidapi enumerate dump with `macos-shared-device`: `path`, `interface_number`, `usage_page/usage`; dedupe by path. Identify which IOHIDDevice carries report `0x06`, the boot keyboard (IF0), and the control/mouse interface (IF2), and IF2's **primary** usage (decides whether IF2 seize needs root too).
2. Open IF2 non-exclusive, send `build_razer_cmd(0x01,0x00,0x04,&[0x03,0x00])`, read IF1 for `0x06` reports. Confirm Input Monitoring prompt and analog data without root. Watch `log stream --predicate 'subsystem == "com.apple.iokit.IOUSBHostFamily"'` for reset loops.
3. Open IF0 non-exclusive; log raw bytes while pressing D-pad and Hyper Response: report length, report-ID byte present?, modifier byte for Alt (L `0x04` vs R `0x40`), arrow usages `0x4F-0x52`. Same for IF2 wheel/middle reports.
4. Seize tests: IF2 as normal user (expect OK, OS wheel/middle go silent); IF0 as user (expect `kIOReturnNotPrivileged`); IF0 under `sudo` (expect OK, arrows/Alt silent).
5. Emission: `CGEventPost` letters, held LSHIFT+letter → uppercase, Cmd+C, F13, arrow, `NSEvent` SystemDefined volume-up / play-pause, with Accessibility granted to Terminal.
6. Two non-exclusive readers on IF1 in two processes both receive `0x06` (validates keeping calibration unchanged).

Exit: a table interface → usage → report format; pass/fail for 4-6.

### Phase 1 — Portability refactor, Windows stays green (~2-3 days)

New files:
- `src/key.rs` — `enum Key` (flat, closed: `Digit0..9`, `A..Z`, `F1..F24`, arrows, `LShift/RShift/LCtrl/RCtrl/LAlt/RAlt`, **new `LCmd/RCmd`** (→ `VK_LWIN/VK_RWIN` on Windows, kVK 0x37/0x36 on mac), navigation, media). Single source-of-truth table `KEY_DEFS: &[KeyDef { key, name, win_vk: u16, mac: MacKey }]` with `enum MacKey { Code(u16), Media(u8 /*NX_KEYTYPE_*/), Unsupported }`. Methods: `name()`, `from_name()` (case-insensitive), `supported_here()`, `all_key_names()`/`key_names_grouped()` (only advertise keys the current OS can emit). Move `vkname.rs` tests here. Unsupported on macOS: `F21-F24`, `MEDIA_STOP`. `INSERT` → kVK_Help `0x72`.
- `src/platform/mod.rs` — `send_key(Key, key_up: bool)`, `install_shutdown_handler()`, `open_url(&str)`, `detach_console()`, `spawn_input_capture()`; `#[cfg]` re-exports from the backend.
- `src/platform/windows.rs` — move `explicit_scan_code` + `send_key` from `main.rs:335-383`, the console ctrl handler (`main.rs:313-316, 596-604`), `FreeConsole` (`main.rs:619`), `ShellExecuteW` open from `tray.rs`.
- `src/platform/stub.rs` — log-only `send_key` for Linux so `cargo test` runs on any dev box.
- `src/remap.rs` — OS-neutral D-pad/wheel/middle/Hyper Response decision logic extracted from `dpad.rs`: `dpad_arrow_test_key_for` (L149-157), held-key tracker (L179-213), and the emit-decisions from `handle_interception_keyboard` (L289-334) / `handle_interception_mouse` (L340-391) as `on_dpad_arrow(dir, down)`, `on_hyper_response(down)`, `on_wheel(delta)`, `on_middle(down)`, `release_held()`.

Modify:
- `main.rs`: drop `windows` imports; `TEST_KEYMAP`/`LAYER1_TEST_KEYMAP`/`LAYER2_TEST_KEYMAP` → `[Key; 20]`; `pressed_vk: [Option<Key>; 20]`; all `send_key` calls → `platform::send_key`; `analog_device_infos` gets a macOS branch (dedupe by `path()`, filter per Phase 0 findings); `run_tray_mode` split per platform; call `platform::spawn_input_capture()` where `dpad::spawn_interception_thread()` is today (L699).
- `config/mod.rs`: `AnalogKeymap.layers: [[Key;20];3]`, `DpadKeymap` fields, `HypershiftConfig.modifier_key: Key`, default `Key::LAlt`. Fix tests in `config/load.rs` (L373+) and `config/payload.rs` (L290).
- `dpad.rs` → `#[cfg(windows)]`, thin Interception I/O shell calling `remap::*`. `hypershift.rs` hook fallback → `#[cfg(windows)]`. `tray.rs` → `#[cfg(windows)]`. `emulate.rs` → `Key`.
- `Cargo.toml`: `[target.'cfg(windows)'.dependencies] windows, interception`; `[target.'cfg(target_os = "macos")'.dependencies] core-graphics, core-foundation, objc2-app-kit, objc2-foundation` (tray-icon in Phase 4); `ctrlc = { version = "3", features = ["termination"] }`; `hidapi` with `features = ["macos-shared-device"]` on macOS.
- Add `.github/workflows/ci.yml`: matrix `windows-latest`, `macos-latest`, `ubuntu-latest` → `cargo build`, `cargo test`.

Verify: Windows — `cargo test`, driver + Interception + tray behave identically (analog keys, Hyper Shift all 3 modes, D-pad, lighting, configui round-trips every key name, hot reload). Linux — `cargo test`, `cargo check --target x86_64-pc-windows-msvc`. Mac — `cargo build` with stub `send_key`.

### Phase 2 — macOS analog keys + key emission (~2-3 days)

`src/platform/macos.rs`:
- kVK table lives in `key.rs` `KEY_DEFS` (letters are non-contiguous: A=0x00, S=0x01, D=0x02…; arrows 0x7B-0x7E; Backspace = kVK_Delete 0x33; DELETE = ForwardDelete 0x75; modifiers 0x37/0x36/0x38/0x3C/0x3A/0x3D/0x3B/0x3E).
- `send_key`: private `CGEventSource` tagged via `set_user_data(MAGIC)`; modifier keys post as key events and update `static HELD_FLAGS: AtomicU64` (NX_DEVICEL/R{SHIFT,CTL,ALT,CMD}KEYMASK); every posted event gets `set_flags(HELD_FLAGS)`; media keys via `NSEvent::otherEventWithType_(SystemDefined, subtype 8, data1 = (nx_code<<16) | ((down?0xA:0xB)<<8))` → `CGEvent` → post. Unsupported keys log once, no-op.
- `install_shutdown_handler` = `ctrlc` (SIGINT/SIGTERM/SIGHUP); `detach_console` no-op; `open_url` = `Command::new("open")`.
- Startup: `AXIsProcessTrustedWithOptions` prompt + clear log line about Accessibility / Input Monitoring.
- `app_root()` macOS branch: when running inside a `.app`, use `~/Library/Application Support/open-tartarus-driver/` for `config.toml` + `logs/` (writing inside a signed bundle breaks its signature and therefore its TCC grants); otherwise exe dir as today.

Verify on hardware: default map → `1..0`, `A..J` in TextEdit; hysteresis unchanged; held LSHIFT + letter → uppercase; LCMD+C copies; F13/arrows/PageUp/VOLUME_UP/MEDIA_PLAY_PAUSE; Ctrl+C releases all held keys (no stuck keys); hot reload; lighting effects; `emulate` subcommand.

### Phase 3 — D-pad / wheel / middle / Hyper Shift via seize (~2-3 days)

- `platform/macos.rs::spawn_input_capture()`: if `geteuid() != 0`, log a clear warning ("run with sudo for D-pad/wheel/Hyper Shift remap") and return — fail-open exactly like the Windows Interception-missing path (analog keys keep working; the D-pad's arrows and Hyper Response's Option key pass through unmodified). Otherwise: worker thread declares `extern "C" fn hid_darwin_set_open_exclusive(c_int)`, flips to exclusive, opens IF0 and IF2, flips back; runs a `read_timeout` loop parsing boot-keyboard reports (modifier byte → Alt edges → `remap::on_hyper_response`; keycode array `0x4F-0x52` → `remap::on_dpad_arrow`) and mouse reports (wheel delta → `remap::on_wheel`; middle button bit → `remap::on_middle`).
- IF2 is both the seized wheel/middle reader **and** the control device for lighting/unlock. Open it once; either send feature reports from the capture thread via a channel, or open it on the main thread before spawning and move the handle into the capture thread with an `mpsc` for lighting commands. Decide in implementation; note the constraint in code.
- Since the analog interface (IF1) is opened non-exclusively, the separate `configui` calibration process keeps working (validated in Phase 0 step 6). If it doesn't, fall back to publishing depths from the driver loop into a shared `Mutex<[u8;20]>` served over HTTP.
- Shutdown: `remap::release_held()` where `dpad::release_held_dpad_test_keys()` is called (`main.rs:808`).
- Add a `sudo`-friendly LaunchDaemon plist sample later in Phase 4.

Verify on hardware: D-pad → K/W/L/S with zero arrow leakage (text editor + terminal); rapid alternating presses; Hyper Response momentary / toggle / modifier modes; wheel up/down taps; middle held; unplug/replug mid-run exits the capture thread cleanly (ideally re-attaches); non-root run degrades gracefully with the warning; layer-indicator LED still toggles; lighting still applies while IF2 is seized.

### Phase 4 — Tray, packaging, CI, docs (~2-4 days)

- `src/tray_macos.rs` (`cfg(target_os="macos")`): `tray-icon` + `muda` menu with the same items (open settings page, open releases page, quit). `run_tray_mode` on macOS: spawn configui thread, spawn driver thread (`run_driver(true, 0)`), build tray on main thread, `NSApplication::sharedApplication().run()`; Quit → `SHUTDOWN_REQUESTED`, join driver thread with a timeout (so stuck-key cleanup runs), then terminate. Note: tray mode needs root for D-pad, so document running via a root LaunchDaemon + user-session tray, or accept analog-only in tray mode without sudo.
- Packaging: `scripts/make-app.sh` → `Tartarus Driver.app` with `Info.plist` (`CFBundleIdentifier`, `LSUIElement=1`), universal binary (`lipo` of `aarch64-apple-darwin` + `x86_64-apple-darwin`), `codesign` with a stable self-signed identity; sample `LaunchDaemons/…tartarus-driver.plist`. Zip the bare CLI too.
- `release.yml`: add a `macos-latest` job producing `tartarus_driver-<tag>-macos-universal.zip`.
- Docs (README + USAGE, English and Japanese sections): macOS permissions walkthrough (Accessibility + Input Monitoring; Terminal attribution; re-grant after re-sign), `sudo` requirement and why, no Interception step, quit Synapse/razer-macos, `F21-F24`/`MEDIA_STOP` unavailable, `LCMD/RCMD` key names; CHANGELOG entry.

## Risks

- **hidapi exclusive toggle is a global, non-public C flag** — the `extern "C"` declaration is sound today but fragile against hidapi-sys changes. Keep a test that the symbol links; fallback is opening IF2/IF0 via `io-kit-sys` directly.
- **Multiple `DeviceInfo` per device on macOS** — without path dedupe `open_analog_devices` opens IF1 twice.
- **TCC identity** — `cargo run` attributes to Terminal; shipped binary needs a stable signing identity or users re-grant after every update.
- **`app_root()` inside a signed bundle** must be fixed before shipping an `.app`.
- **Root + tray** — tray needs the user session, D-pad needs root; may end up as two processes.
- **OpenRazer #2710 reset loop** — same feature report as Windows, unobserved there, but Phase 0 watches for it.
- **Modifier state tracking** — if `HELD_FLAGS` is wrong, Shift/Ctrl remaps silently do nothing; Phase 2 tests cover it.
- **Layout dependence** — kVK codes are physical ANSI positions; non-QWERTY layouts get different characters (same caveat as Windows VKs).

## Effort

Phase 0: 1-2 days · Phase 1: 2-3 · Phase 2: 2-3 · Phase 3: 2-3 · Phase 4: 2-4 → roughly 2-3 focused weeks. Phase 0's findings (interface usages, report framing) are the only thing that could still change Phase 3's details.

## Workflow

- All macOS work lives on the `macos-port` branch of this fork (`LukeGarnsey/open-tartarus-driver-mac`); `main` tracks upstream `ultramonaka/open-tartarus-driver`.
- Phase 1 (the portability refactor) can be done on any OS; Phases 0 and 2-4 must be built and tested on a Mac with the Tartarus Pro attached.
- Keep Windows behaviour byte-for-byte unchanged so the branch stays upstreamable.
