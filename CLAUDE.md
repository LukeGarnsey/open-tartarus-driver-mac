# CLAUDE.md — open-tartarus-driver-mac

Fork of `ultramonaka/open-tartarus-driver` (GPL-3.0; keep the license and
author notices) whose purpose is a **macOS port with full feature parity**
of `tartarus_driver`, a Rust driver for the Razer Tartarus Pro keypad.

## Where things are

- **Branch `macos-port`** holds all port work. `main` tracks upstream; don't
  commit port work there.
- **`docs/MACOS_PORT_PLAN.md`** is the plan: phases, verified macOS facts,
  risks. Read it before starting a phase. Update it when reality differs.
- Crate: `tartarus_driver/` (edition 2024). Run cargo from there.
- User docs: `README.md`, `USAGE.md`, `CHANGELOG.md` (EN + JA sections —
  update both). `research.md` = protocol reverse-engineering notes.

## Current state (as of 2026-09-16)

- **Phase 1 done** (`d8c7ad4`): `src/key.rs` (`Key` enum + one table for
  name / Windows VK / macOS keycode), `src/platform/{windows,macos,stub}.rs`
  (the OS seam), `src/remap.rs` (OS-neutral D-pad/wheel/middle/Hyper
  Response logic). `dpad.rs`, `tray.rs`, and the hook in `hypershift.rs`
  are `#[cfg(windows)]`.
- **Phase 2 done (2026-09-16)**: `src/platform/macos.rs` emits for real —
  hand-rolled CoreGraphics `CGEventPost` with `HELD_FLAGS` modifier
  tracking (pure `next_flags()`, unit-tested) on a CombinedSessionState
  source, `NSEvent` SystemDefined events via `objc2` `msg_send!` for
  media keys, `check_input_permissions()` (Accessibility prompt),
  `hid_open_hint()` (Input Monitoring hint), `bundle_app_root()` for
  `.app` runs. `analog_device_infos()` picks `interface_number() == 1` on
  macOS. All hardware-verified (list in the plan's Phase 2 block).
- **Phase 3 done (2026-09-16)**: `spawn_input_capture(ctrl)` runs an IF2
  reader (wheel/middle, no root) on the seized control handle main.rs
  now opens (`open_razer_control_device(api, seize_for_capture)` →
  `Arc<Mutex<HidDevice>>`, shared with lighting because a seized device
  refuses feature reports from other handles) and, when root, seizes IF0
  for D-pad/Hyper Response. Pure `keyboard_edges`/`mouse_edges` parsers,
  unit-tested. Hardware-verified incl. diagonals and the non-root
  fail-open path.
- **Phase 0 done (2026-09-16)** on an Apple Silicon Mac with the keypad:
  `cargo build` + `cargo test` (41 pass) are green on macOS, and
  `examples/mac_probe.rs` verified every open question — results table and
  per-step pass/fail are in the plan under "Phase 0 … Results". Headlines:
  IF0/IF2 reports are 8-byte **unnumbered** (no ID byte); IF1's primary
  usage is Keyboard so `analog_device_infos()` must pick
  `interface_number() == 1` on macOS; IF2 can be seized **without** root
  (only IF0 needs it); diagonal D-pad = two codes in the key array;
  CGEventPost + NSEvent media emission works; two shared IF1 readers work.
- **Phase 4 done (2026-09-16)**: `src/tray_macos.rs` (tray-icon/muda,
  tray on the main thread, driver on a worker thread), no-arg bundled
  launch = `tray`, `scripts/macos/make-app.sh` (universal, signed .app),
  LaunchAgent sample, `release.yml` macOS job, README/USAGE §9/CHANGELOG
  in EN + JA. Verified from Finder with fresh TCC grants.
- **All four phases are complete.** Remaining ideas, none started: root
  LaunchDaemon for D-pad at login (TCC in a daemon context unverified),
  reattach after unplug (reader threads currently exit), `emulate`
  subcommand not exercised on macOS, toggle/modifier_key Hyper Shift
  styles not exercised on macOS (OS-neutral code).
- Windows regression check: `cargo check --tests --target x86_64-pc-windows-gnu`
  (done on Linux; no mingw on this Mac yet).

## Hard constraints

- **Windows behaviour must stay byte-for-byte unchanged** so the branch is
  upstreamable. Don't touch `platform/windows.rs`, `dpad.rs`, `tray.rs`
  logic; keep new deps target-gated in `Cargo.toml`. (So far the only
  Windows-side edits are two no-op fns in `platform/windows.rs`, an unused
  parameter, and `ctrl` becoming `Arc<Mutex<..>>` in main.rs.)
- **D-pad / wheel / Hyper Shift on macOS = seize the HID interfaces, run as
  root.** Decided by the user. Seizing a keyboard-usage IOHIDDevice (IF0)
  needs root (IOHIDFamily returns `kIOReturnNotPrivileged` otherwise); the
  mouse-usage IF2 seizes fine as a user. No CGEventTap-correlation approach.
- A seized IOHIDDevice rejects feature reports from any *other* handle in
  the process (`kIOReturnExclusiveAccess`), so IF2 is opened once, seized,
  and shared via `Arc<Mutex<HidDevice>>` between lighting and the reader.
- The analog interface must be opened **non-exclusively** (hidapi
  `macos-shared-device` is on) because `configui` runs as a separate process
  and opens its own handle for live calibration. Only IF0/IF2 get seized,
  by flipping the process-global `HidApi::set_open_exclusive(true)` around
  exactly those two opens (public hidapi-rs API on macOS; no per-open
  toggle, so do the seized opens before any other thread opens anything).
- Key emission: hand-rolled CoreGraphics (`CGEventPost`, kVK codes already in
  `key.rs`) + `NSEvent` SystemDefined subtype 8 for media keys. Don't adopt
  `enigo` (no L/R Alt or L/R Cmd distinction). `F21-F24` and `MEDIA_STOP`
  are `MacKey::Unsupported`.
- Tests must never emit real keystrokes. `cargo test` on Windows calls the
  real `SendInput` if a test reaches `send_key` — keep bookkeeping tests
  pure (see `remap::track_held`).

## macOS gotchas

- **TCC permissions**: Accessibility (for CGEventPost) and Input Monitoring
  (for HID reads). When run via `cargo run` / from a terminal, the grant is
  attributed to the **terminal app**, not the binary. A rebuilt unsigned
  binary loses grants; a stable self-signed code-signing identity fixes
  that (Phase 4).
- `sudo cargo run` and plain `cargo run` have different `target/` ownership
  headaches — prefer `cargo build --release` then `sudo ./target/release/tartarus_driver`.
- Hardware test recipe: `./target/release/tartarus_driver 60` (time-boxed)
  from Terminal.app with TextEdit focused; write a throwaway `config.toml`
  at the repo root mid-run to exercise hot reload (it's gitignored — delete
  it afterwards); read `logs/run.log` for the DOWN/UP edges.
- hidapi on macOS lists one `DeviceInfo` per (device, usage pair) with the
  same `path()`; `analog_device_infos()` already dedupes by path.
- Report-ID framing differs from Windows: macOS only prepends the ID byte
  for numbered reports. Analog report `0x06` is numbered (24 B, same
  offsets as Windows); IF0/IF2 reports are **unnumbered** 8 B — mods/buttons
  at `buf[0]`, keys at `buf[2..8]`, wheel at `buf[3]`.
- OpenRazer PR #2710: the device-mode-3 unlock caused firmware reset loops
  on some units. Watch `log stream --predicate 'subsystem == "com.apple.iokit.IOUSBHostFamily"'`
  during Phase 0.
- Quit Razer Synapse for Mac / razer-macos before testing (they don't
  support the Tartarus Pro, but anything poking IF2 can flip device mode).
  Synapse's DriverKit dexts (`com.razer.appengine.driver`) stay loaded
  even with the GUI quit; they did not interfere in Phase 0.
- TCC is attributed to the *responsible* app. Running from Claude Code's
  own app bundle needed its own Input Monitoring grant, and its
  Accessibility grant never took effect — use a plain Terminal.app window
  for `emit`/`sudo` tests. `sudo` can't prompt from Claude's Bash tool
  (no TTY); run root tests in Terminal.
- `app_root()` writes `config.toml`/`logs/` next to the exe — inside a
  signed `.app` that breaks the signature. Phase 2 adds an Application
  Support path for the bundle case.

## Verify

```
cd tartarus_driver
cargo build && cargo test            # any OS (48 tests)
cargo run --release -- emulate       # hardware-free harness for the key pipeline
scripts/macos/make-app.sh            # macOS: universal signed Tartarus Driver.app -> dist/
cargo check --tests --target x86_64-pc-windows-gnu   # Windows regression check from Linux/mac (needs mingw + target)
```
CI (`.github/workflows/ci.yml`) builds + tests on Windows/macOS/Linux on
every push.

## Style

- Match upstream's heavy explanatory comments ("why", dates, hardware
  findings). Upstream is not rustfmt-clean; don't reformat untouched files.
- Log lines are user-facing (USAGE.md tells users to grep for some of
  them); keep existing phrases stable.
- Commit messages: end with the Claude attribution lines the harness
  provides; don't push unless asked.
