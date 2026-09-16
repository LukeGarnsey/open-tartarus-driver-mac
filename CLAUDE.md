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
- **`src/platform/macos.rs` is a compiling skeleton**: `send_key` warns
  once and types nothing; `spawn_input_capture` only checks for root.
  Shutdown handler (ctrlc) and `open_url` are final.
- **Phase 0 done (2026-09-16)** on an Apple Silicon Mac with the keypad:
  `cargo build` + `cargo test` (41 pass) are green on macOS, and
  `examples/mac_probe.rs` verified every open question — results table and
  per-step pass/fail are in the plan under "Phase 0 … Results". Headlines:
  IF0/IF2 reports are 8-byte **unnumbered** (no ID byte); IF1's primary
  usage is Keyboard so `analog_device_infos()` must pick
  `interface_number() == 1` on macOS; IF2 can be seized **without** root
  (only IF0 needs it); diagonal D-pad = two codes in the key array;
  CGEventPost + NSEvent media emission works; two shared IF1 readers work.
- **Next: Phase 2** (`send_key` for real, `analog_device_infos` fix,
  Accessibility prompt, `app_root()` bundle path), then Phase 3.
- Windows regression check: `cargo check --tests --target x86_64-pc-windows-gnu`
  (done on Linux; no mingw on this Mac yet).

## Hard constraints

- **Windows behaviour must stay byte-for-byte unchanged** so the branch is
  upstreamable. Don't touch `platform/windows.rs`, `dpad.rs`, `tray.rs`
  logic; keep new deps target-gated in `Cargo.toml`.
- **D-pad / wheel / Hyper Shift on macOS = seize the HID interfaces, run as
  root.** Decided by the user. Seizing a keyboard-usage IOHIDDevice needs
  root (IOHIDFamily returns `kIOReturnNotPrivileged` otherwise). No
  CGEventTap-correlation approach.
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
cargo build && cargo test            # any OS
cargo run --release -- emulate       # hardware-free harness for the key pipeline
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
