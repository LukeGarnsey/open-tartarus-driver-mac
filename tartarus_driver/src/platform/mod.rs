// The OS-specific seam. Everything that touches an OS input/console/shell
// API lives in exactly one backend module below; the rest of the crate
// only ever calls the functions re-exported here, with `crate::key::Key`
// (not a Win32 VK or a macOS keycode) as the currency:
//
//   send_key(key, key_up)      emit one synthetic key press/release to the OS
//   install_shutdown_handler() route Ctrl+C / console close / SIGTERM into
//                              crate::SHUTDOWN_REQUESTED so run_driver's
//                              stuck-key cleanup still runs
//   detach_console()           `tray` mode: drop the console window, if any
//   open_url(url)              open a URL in the default browser (tray menu)
//   check_input_permissions()  log (and on macOS, prompt for) whatever OS
//                              permission synthetic input needs; no-op
//                              where none is needed
//   hid_open_hint(&err)        an extra, OS-specific sentence to log when a
//                              HID open fails (macOS: the Input Monitoring
//                              grant), or None
//   spawn_input_capture()      start whatever this OS uses to intercept the
//                              Tartarus's own D-pad/wheel/middle-click/
//                              Hyper Response events (Windows: the
//                              Interception kernel driver, see dpad.rs);
//                              must fail open — analog keys keep working
//                              when it can't
//
// Backends:
//   windows.rs  the original, hardware-verified SendInput / Win32 code
//   macos.rs    CoreGraphics/AppKit key emission (hardware-verified);
//               D-pad/wheel/Hyper Shift capture still pending (see
//               docs/MACOS_PORT_PLAN.md Phase 3)
//   stub.rs     everything else (Linux dev boxes / CI): logs instead of
//               emitting, so the pure logic + `cargo test` run anywhere

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use self::windows::*;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use self::macos::*;

#[cfg(not(any(windows, target_os = "macos")))]
mod stub;
#[cfg(not(any(windows, target_os = "macos")))]
pub use self::stub::*;
