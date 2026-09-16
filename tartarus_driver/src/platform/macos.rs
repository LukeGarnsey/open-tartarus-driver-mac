// macOS backend — skeleton. Compiles and runs today, but every function is
// still a placeholder: this file exists so a fresh `cargo build` on a Mac
// succeeds before the real work lands, and so the phases in
// docs/MACOS_PORT_PLAN.md each replace one clearly-marked block:
//
//   Phase 2  send_key           CoreGraphics CGEventPost (kVK codes) +
//                               NSEvent SystemDefined events for media keys;
//                               Accessibility (TCC) permission prompt at
//                               startup
//   Phase 3  spawn_input_capture seize the Tartarus's boot-keyboard (IF0)
//                               and control/mouse (IF2) HID interfaces —
//                               needs root — and feed remap::* from the raw
//                               reports; fail open (warn, keep analog keys
//                               working) when not root
//   Phase 4  (tray_macos.rs)    menu-bar icon; open_url stays `open`
//
// What is already final here: the shutdown handler (SIGINT/SIGTERM/SIGHUP
// via ctrlc, same contract as the Windows console ctrl handler), `open`
// for URLs, and the root check that decides whether D-pad capture can run.

use crate::key::Key;
use crate::{eprintln, println, SHUTDOWN_REQUESTED};
use std::sync::atomic::{AtomicBool, Ordering};

// Phase 2 replaces this body. Logs once per process (not per keystroke) so
// a Mac user running the skeleton sees WHY nothing is typed, without the
// 500us analog loop flooding the log.
pub fn send_key(key: Key, key_up: bool) {
    static WARNED: AtomicBool = AtomicBool::new(false);
    if !WARNED.swap(true, Ordering::SeqCst) {
        eprintln!(
            "WARNING: macOS key emission is not implemented yet (docs/MACOS_PORT_PLAN.md Phase 2) — \
             keys are detected but nothing is typed. First event: {} {}",
            key.name(),
            if key_up { "UP" } else { "DOWN" }
        );
    }
}

pub fn install_shutdown_handler() {
    if let Err(e) = ctrlc::set_handler(|| SHUTDOWN_REQUESTED.store(true, Ordering::SeqCst)) {
        eprintln!(
            "WARNING: failed to install Ctrl+C handler ({e}) — stopping via Ctrl+C may leave a \
             key stuck down if one happens to be held at that exact moment."
        );
    }
}

// No console window to detach from on macOS; `tray` mode is started from a
// LaunchAgent/.app bundle or a terminal the user already owns.
pub fn detach_console() {}

#[allow(dead_code)] // tray menu (Phase 4) will call this
pub fn open_url(url: &str) {
    if let Err(e) = std::process::Command::new("open").arg(url).spawn() {
        eprintln!("WARNING: could not open {url}: {e}");
    }
}

// Seizing a keyboard-usage HID device (the Tartarus's boot keyboard
// interface, which carries the D-pad arrows and the Hyper Response Alt)
// requires root on macOS — IOHIDFamily returns kIOReturnNotPrivileged
// otherwise (see docs/MACOS_PORT_PLAN.md "Verified macOS facts"). Checked
// here rather than after a failed open so the log says exactly what to do.
pub fn is_root() -> bool {
    unsafe extern "C" {
        fn geteuid() -> u32;
    }
    unsafe { geteuid() == 0 }
}

// Phase 3 replaces the `else` branch with the real capture thread.
pub fn spawn_input_capture() {
    if !is_root() {
        eprintln!(
            "WARNING: not running as root — D-pad/wheel/middle-click remap and Hyper Shift are \
             disabled (the Tartarus's own arrow/Option/wheel events pass through unmodified). \
             Run with `sudo` to enable them. Analog keys keep working either way."
        );
    } else {
        println!(
            "Running as root, but the macOS D-pad/wheel/Hyper Shift capture is not implemented \
             yet (docs/MACOS_PORT_PLAN.md Phase 3)."
        );
    }
}
