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
- Config vocabulary: key names are a closed OS-neutral string set (`0-9`, `A-Z`, `F1-F24`, `LEFT/UP/RIGHT/DOWN`, `SPACE ENTER TAB ESCAPE BACKSPACE LSHIFT RSHIFT LCTRL RCTRL LALT RALT HOME END PAGEUP PAGEDOWN INSERT DELETE`, `MEDIA_*`, `VOLUME_*`; the port added `LCMD RCMD` and the punctuation keys `LBRACKET RBRACKET SEMICOLON QUOTE COMMA PERIOD SLASH BACKSLASH MINUS EQUALS GRAVE`). Users' `config.toml` files carry over.

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

- **hidapi on macOS opens every device in exclusive/seize mode by default.** The `macos-shared-device` Cargo feature flips the process-global default to non-exclusive at `HidApi::new()`. There is no per-open toggle, but hidapi-rs 2.6.6 exposes the global one **safely** as `HidApi::set_open_exclusive(bool)` / `get_open_exclusive()` plus `HidDevice::is_open_exclusive()` (only on macOS) — no `extern "C"` declaration needed. Verified 2026-09-16: flip to `true`, `open_device`, flip back to `false` seizes exactly that one handle.
- **Seizing a keyboard-usage device (primary usage page 1 / usage 6 or 7) needs root**; seizing mouse-usage or vendor-usage devices does not (Input Monitoring permission suffices).
- **hidapi macOS enumeration emits one `DeviceInfo` per (IOHIDDevice, usage pair), all sharing the same `path()`** — must dedupe by path or the same interface gets opened twice. `interface_number()` is populated (0/1/2) on macOS, so the backend can address interfaces by number instead of by usage. **IF1 (analog) enumerates with primary usage 0x0001/0x0006 (Keyboard)** — see Phase 0 results — so the usage-based boot-collection filter in `analog_device_infos()` is wrong on macOS; filter by `interface_number() == 1` there.
- macOS delivers input reports to every non-seized opener, so the separate-process `configui` calibration reader keeps working as long as the analog interface is *not* seized.
- Reading a `HidDevice` from another thread is fine on macOS (per-device CFRunLoop thread + queue); `HidDevice: Send`.
- Report-ID framing: Windows hidapi always prepends a report-ID byte; macOS only for numbered reports. Analog report `0x06` is numbered (`buf[0]==0x06` holds on both, 24-byte reports, `buf[1..=20]` = depths — the driver's offsets need no change). **IF0 and IF2 reports are unnumbered on macOS: 8 bytes, no ID byte** (verified 2026-09-16; layouts in the Phase 0 results table).
- Key emission: CoreGraphics `CGEventCreateKeyboardEvent` + `CGEventPost(kCGHIDEventTap)` with kVK codes; media/volume keys need `NSEvent otherEventWithType:NSEventTypeSystemDefined subtype:8`. Requires **Accessibility** TCC. `enigo` is *not* a fit (no L/R Alt or L/R Cmd distinction, `MEDIA_STOP`/`INSERT`/`F21-F24` cfg'd out on mac) — hand-roll (~150 lines).
- `tray-icon` 0.25 on macOS must be created on the main thread with an `NSApplication` run loop running.
- TCC grants are keyed to code identity: ad-hoc `codesign -s -` loses grants every rebuild; a self-signed cert from Keychain Access (or Developer ID) gives a stable identity. A CLI run from Terminal is attributed to Terminal.app — more precisely to the *responsible process*: observed 2026-09-16 that the same binary launched under Terminal.app vs. under the Claude Code app bundle needed separate Input Monitoring grants, and that the Accessibility grant for the app-bundle case did not take effect even when toggled on (Terminal.app's did). Document for users: grant the terminal you actually launch from.
- Razer Synapse for Mac / `razer-macos` don't support the Tartarus Pro; still document "quit them" since anything poking interface 2 can flip device mode.
- OpenRazer PR #2710: device-mode 3 (the unlock) caused firmware reset loops on some units — **not observed** on this unit (2026-09-16, `log show` over the IOUSBHostFamily subsystem showed zero events across repeated unlocks). Also: the unlock is idempotent; re-sending it while already in mode 3 produces no standby report and no side effects.

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

**Results (2026-09-16, Apple Silicon MacBook Pro, macOS 15 / Darwin 24.6, Tartarus Pro FW as shipped, Razer Synapse GUI not running but its DriverKit dexts `com.razer.appengine.driver` still loaded — they did not interfere).** `examples/mac_probe.rs` is kept in the tree (macOS-only; stub `main` elsewhere so CI's `cargo test` still compiles it) for re-verification on other units.

| if | hidapi usage pairs (primary first) | report | layout (macOS, no ID byte unless noted) |
|---|---|---|---|
| 0 | `0001/0006` Keyboard | 8 B, unnumbered | `[mods][00][k1..k6]`. D-pad: `k` = `0x52` up, `0x4f` right, `0x51` down, `0x50` left; **diagonals put two codes in the array** (`52 4f`), so diff all six slots. Hyper Response = `mods & 0x04` (Left Alt), no keycode. |
| 1 | `0001/0006` Keyboard, `000c/0001` Consumer, `0001/0080` SysCtl, `0001/0000` | 24 B, **numbered `0x06`** | `[06][d1..d20][00 00 00]`, depths 0-255, identity mapping, ~1 report/ms while a key moves. Same as Windows. |
| 2 | `0001/0002` Mouse, `0001/0001` Pointer | 8 B, unnumbered | `[buttons][x][y][wheel][..]`. Middle = `buttons & 0x04`; wheel = signed i8 in `buf[3]` (`+1` up, `-1` down). Also the feature-report control channel (unlock + lighting). |

| step | result |
|---|---|
| 2 analog, no root | PASS — IF2 and IF1 open non-exclusively after the Input Monitoring grant; unlock accepted; all-zero standby `0x06` arrives 2 ms later, then live data. No USB re-enumeration. |
| 4a seize IF0 as user | FAIL as predicted — `0xE00002C1 kIOReturnNotPrivileged`. |
| 4b seize IF2 as user | PASS — `is_open_exclusive()==true`, probe still receives wheel/middle, **OS stops scrolling**. Root not needed for IF2. |
| 4c seize IF0 as root | PASS — arrows/Alt delivered to the probe only. |
| 5 emission | PASS — `CGEventPost` letters; held LShift (flags `kCGEventFlagMaskShift \| NX_DEVICELSHIFTKEYMASK`) → `A`; LCmd+A / Right / F13 / PageUp / Enter; `NSEvent` SystemDefined subtype 8 volume-up (HUD shown) and play/pause (Music toggled). `CGEventSourceCreate(kCGEventSourceStatePrivate)` works. |
| 6 two shared readers | PASS — two processes on IF1 both received every `0x06` report. `configui` calibration design stands. |

Consequences for later phases: Phase 2's `analog_device_infos()` macOS branch must select `interface_number() == 1` (the current usage filter also keeps IF2 via its `0001/0001` entry); Phase 3 parses IF0 at `buf[0]` (mods) / `buf[2..8]` (keys) and IF2 at `buf[0]` (buttons) / `buf[3]` (wheel) with **no** report-ID offset, and can seize IF2 without root (only IF0 needs it — so a non-root run can still remap wheel/middle, and only D-pad/Hyper Shift degrade). Phase 3 uses `HidApi::set_open_exclusive` rather than the `extern "C"` flag.

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

**Done 2026-09-16 (commit after `3fe4518`).** Implemented as planned with two deviations: the event source is `kCGEventSourceStateCombinedSessionState` (not Private) so freshly created events inherit the *physical* keyboard's modifier flags and the driver only ORs in / clears its own (`HELD_FLAGS`, pure `next_flags()` with unit tests) — this is what makes a real-keyboard Shift combine with a Tartarus letter the way SendInput does; and the seam grew `check_input_permissions()` + `hid_open_hint()` (no-ops on Windows/stub). `analog_device_infos()` selects `interface_number() == 1` on macOS. Hardware-verified in TextEdit: `1234567890abcdefghij` on defaults; held LSHIFT/RSHIFT + letter → uppercase; LCMD+A selects all; RALT+A → `å`; F13/PageUp/LEFT/ENTER; VOLUME_UP/DOWN HUD; MEDIA_PLAY_PAUSE toggles Music; hot reload of config.toml; `[lighting] static 00FF00` turned the pad green; Ctrl+C with LSHIFT held released it (no stuck key). Not yet exercised: `emulate` subcommand (same `send_key` path). Driver log says `Opened 1 HID interface(s)`.

`src/platform/macos.rs`:
- kVK table lives in `key.rs` `KEY_DEFS` (letters are non-contiguous: A=0x00, S=0x01, D=0x02…; arrows 0x7B-0x7E; Backspace = kVK_Delete 0x33; DELETE = ForwardDelete 0x75; modifiers 0x37/0x36/0x38/0x3C/0x3A/0x3D/0x3B/0x3E).
- `send_key`: private `CGEventSource` tagged via `set_user_data(MAGIC)`; modifier keys post as key events and update `static HELD_FLAGS: AtomicU64` (NX_DEVICEL/R{SHIFT,CTL,ALT,CMD}KEYMASK); every posted event gets `set_flags(HELD_FLAGS)`; media keys via `NSEvent::otherEventWithType_(SystemDefined, subtype 8, data1 = (nx_code<<16) | ((down?0xA:0xB)<<8))` → `CGEvent` → post. Unsupported keys log once, no-op.
- `install_shutdown_handler` = `ctrlc` (SIGINT/SIGTERM/SIGHUP); `detach_console` no-op; `open_url` = `Command::new("open")`.
- Startup: `AXIsProcessTrustedWithOptions` prompt + clear log line about Accessibility / Input Monitoring.
- `app_root()` macOS branch: when running inside a `.app`, use `~/Library/Application Support/open-tartarus-driver/` for `config.toml` + `logs/` (writing inside a signed bundle breaks its signature and therefore its TCC grants); otherwise exe dir as today.

Verify on hardware: default map → `1..0`, `A..J` in TextEdit; hysteresis unchanged; held LSHIFT + letter → uppercase; LCMD+C copies; F13/arrows/PageUp/VOLUME_UP/MEDIA_PLAY_PAUSE; Ctrl+C releases all held keys (no stuck keys); hot reload; lighting effects; `emulate` subcommand.

### Phase 3 — D-pad / wheel / middle / Hyper Shift via seize (~2-3 days)

**Done 2026-09-16.** The "IF2 is both the seized reader and the control device" question was settled empirically (`mac_probe dualif2`): a seized IOHIDDevice rejects feature reports from any *other* handle in the process (`kIOReturnExclusiveAccess`), but accepts them on the seized handle itself. So `main.rs::open_razer_control_device(api, seize_for_capture)` opens IF2 **seized** on macOS (falling back to shared with a warning) and returns `Arc<Mutex<HidDevice>>`; the driver loop's lighting calls and `platform/macos.rs::if2_reader` share it (5 ms `read_timeout` under the lock, so lighting never waits long). `configui` passes `seize_for_capture = false` for its best-effort unlock. IF0 is opened seized in `spawn_input_capture` via a fresh `HidApi` when root; otherwise that half fails open with a warning that now correctly says wheel/middle still work. Parsing is pure and unit-tested (`keyboard_edges` diffs the 6-slot array so diagonals work; `mouse_edges`). Hardware-verified: D-pad → `k/w/l/s` with the caret never moving; Hyper Response momentary → Layer 1 while held (key01 → F1) with no Option leak; wheel → `o`/`p` per notch, middle → `m` held, page does not scroll; all of the wheel/middle part also without sudo. Not yet exercised: toggle / modifier_key Hyper Shift styles (OS-neutral code shared with Windows), unplug mid-run (reader threads log and exit; restart required), layer-indicator LED under the shared mutex.

- `platform/macos.rs::spawn_input_capture()`: if `geteuid() != 0`, log a clear warning ("run with sudo for D-pad/wheel/Hyper Shift remap") and return — fail-open exactly like the Windows Interception-missing path (analog keys keep working; the D-pad's arrows and Hyper Response's Option key pass through unmodified). Otherwise: worker thread declares `extern "C" fn hid_darwin_set_open_exclusive(c_int)`, flips to exclusive, opens IF0 and IF2, flips back; runs a `read_timeout` loop parsing boot-keyboard reports (modifier byte → Alt edges → `remap::on_hyper_response`; keycode array `0x4F-0x52` → `remap::on_dpad_arrow`) and mouse reports (wheel delta → `remap::on_wheel`; middle button bit → `remap::on_middle`).
- IF2 is both the seized wheel/middle reader **and** the control device for lighting/unlock. Open it once; either send feature reports from the capture thread via a channel, or open it on the main thread before spawning and move the handle into the capture thread with an `mpsc` for lighting commands. Decide in implementation; note the constraint in code.
- Since the analog interface (IF1) is opened non-exclusively, the separate `configui` calibration process keeps working (validated in Phase 0 step 6). If it doesn't, fall back to publishing depths from the driver loop into a shared `Mutex<[u8;20]>` served over HTTP.
- Shutdown: `remap::release_held()` where `dpad::release_held_dpad_test_keys()` is called (`main.rs:808`).
- Add a `sudo`-friendly LaunchDaemon plist sample later in Phase 4.

Verify on hardware: D-pad → K/W/L/S with zero arrow leakage (text editor + terminal); rapid alternating presses; Hyper Response momentary / toggle / modifier modes; wheel up/down taps; middle held; unplug/replug mid-run exits the capture thread cleanly (ideally re-attaches); non-root run degrades gracefully with the warning; layer-indicator LED still toggles; lighting still applies while IF2 is seized.

### Phase 4 — Tray, packaging, CI, docs (~2-4 days)

**Done 2026-09-16.** `src/tray_macos.rs` uses `tray-icon` 0.25 (+ its `muda` menu) with the same four items as Windows; the icon is a code-drawn template glyph. Threading is inverted vs. Windows: the tray owns the main thread (`[NSApp run]`, accessory activation policy) and `run_driver` runs on a worker thread that calls `process::exit` after its stuck-key cleanup — "終了", Ctrl+C and SIGTERM all go through `SHUTDOWN_REQUESTED`. A bundled launch with no argument defaults to `tray` (main.rs checks `bundle_app_root`). `scripts/macos/make-app.sh` builds a universal (`lipo`) `Tartarus Driver.app` with `LSUIElement`, ad-hoc or `SIGN_IDENTITY`-signed; `release.yml` gained a `macos-latest` job producing `tartarus_driver-<tag>-macos-universal.zip`; a LaunchAgent sample lives in `scripts/macos/`. Docs: README (requirements/download/quick start/known-limitation, EN+JA), USAGE section 9 (EN+JA), CHANGELOG (EN+JA). Verified: menu-bar icon, "設定を開く" opening configui in the browser, "終了" clean exit; Finder launch of the `.app` prompting for both TCC grants (Input Monitoring had to be added with **+** by hand — it was not auto-listed), then working after relaunch with config/logs under Application Support.

**Post-release fix (2026-09-18, v1.2.3):** a Tartarus-held modifier didn't apply to hardware events. `CGEventSourceFlagsState` for both the HID-system and combined-session ids *did* report the posted Shift as held (`mac_probe modstate`), but real mouseDown/keyDown events arriving during the hold carried `shift=false` (`mac_probe tapwatch`) — the kernel HID layer stamps hardware events from physical modifier state, which a posted event never reaches. Fixed with a session event tap in `platform/macos.rs` (`spawn_modifier_bridge`) that ORs `HELD_FLAGS` into passing events; modifier presses are also posted as `kCGEventFlagsChanged`. Verified: Photoshop Shift-drag and Shift+real-keyboard in TextEdit.

**Root + tray decision:** one process. `sudo ./tartarus_driver tray` from Terminal shows the menu-bar icon and gets D-pad/Hyper Shift; a Finder/LaunchAgent launch runs as the user (analog + wheel/middle only). A root LaunchDaemon for the D-pad half was **not** done — a daemon runs outside the GUI session and its TCC/CGEventPost behaviour is unverified; left as future work.

- `src/tray_macos.rs` (`cfg(target_os="macos")`): `tray-icon` + `muda` menu with the same items (open settings page, open releases page, quit). `run_tray_mode` on macOS: spawn configui thread, spawn driver thread (`run_driver(true, 0)`), build tray on main thread, `NSApplication::sharedApplication().run()`; Quit → `SHUTDOWN_REQUESTED`, join driver thread with a timeout (so stuck-key cleanup runs), then terminate. Note: tray mode needs root for D-pad, so document running via a root LaunchDaemon + user-session tray, or accept analog-only in tray mode without sudo.
- Packaging: `scripts/make-app.sh` → `Tartarus Driver.app` with `Info.plist` (`CFBundleIdentifier`, `LSUIElement=1`), universal binary (`lipo` of `aarch64-apple-darwin` + `x86_64-apple-darwin`), `codesign` with a stable self-signed identity; sample `LaunchDaemons/…tartarus-driver.plist`. Zip the bare CLI too.
- `release.yml`: add a `macos-latest` job producing `tartarus_driver-<tag>-macos-universal.zip`.
- Docs (README + USAGE, English and Japanese sections): macOS permissions walkthrough (Accessibility + Input Monitoring; Terminal attribution; re-grant after re-sign), `sudo` requirement and why, no Interception step, quit Synapse/razer-macos, `F21-F24`/`MEDIA_STOP` unavailable, `LCMD/RCMD` key names; CHANGELOG entry.

## Risks

- **hidapi exclusive toggle is process-global** — `HidApi::set_open_exclusive(true)` must be flipped back to `false` immediately after the IF0/IF2 opens, and no other thread may open a device in between (open the seized handles before spawning anything else). Resolved: it is a public hidapi-rs API, not an `extern "C"` hack.
- **Multiple `DeviceInfo` per device on macOS** — without path dedupe `open_analog_devices` opens IF1 twice. Confirmed; and IF1's *first* entry is a Keyboard usage, so the shared usage filter must be replaced by an `interface_number()` check on macOS (Phase 2).
- **TCC identity** — `cargo run` attributes to the responsible (terminal/host) app; shipped binary needs a stable signing identity or users re-grant after every update. Observed: Accessibility granted to a non-Terminal host app did not take effect for a child CLI, Terminal.app's did — test the shipped `.app` path explicitly in Phase 4.
- **`app_root()` inside a signed bundle** must be fixed before shipping an `.app`.
- **Root + tray** — tray needs the user session, D-pad needs root; may end up as two processes. Softened by Phase 0/3: wheel/middle remap (IF2) works without root, only D-pad/Hyper Shift (IF0) need it.
- **hidapi's exclusive flag is process-global** — `open_razer_control_device` and `open_if0_seized` each flip it around exactly one open; in `tray` mode the in-process configui thread could theoretically open IF1 during that window (only on an HTTP calibration request landing in those microseconds). Accepted; revisit if it ever bites.
- **OpenRazer #2710 reset loop** — same feature report as Windows; not reproduced on the test unit (Phase 0). Keep the `log stream` hint in USAGE for users with other firmware.
- **Modifier state tracking** — if `HELD_FLAGS` is wrong, Shift/Ctrl remaps silently do nothing; Phase 2 tests cover it.
- **Layout dependence** — kVK codes are physical ANSI positions; non-QWERTY layouts get different characters (same caveat as Windows VKs).

## Effort

Phase 0: 1-2 days · Phase 1: 2-3 · Phase 2: 2-3 · Phase 3: 2-3 · Phase 4: 2-4 → roughly 2-3 focused weeks. Phase 0's findings (interface usages, report framing) are the only thing that could still change Phase 3's details.

## Workflow

- All macOS work lives on the `macos-port` branch of this fork (`LukeGarnsey/open-tartarus-driver-mac`); `main` tracks upstream `ultramonaka/open-tartarus-driver`.
- Phase 1 (the portability refactor) can be done on any OS; Phases 0 and 2-4 must be built and tested on a Mac with the Tartarus Pro attached.
- Keep Windows behaviour byte-for-byte unchanged so the branch stays upstreamable.
