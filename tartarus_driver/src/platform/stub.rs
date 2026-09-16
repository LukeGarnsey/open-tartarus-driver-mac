// Non-Windows backend used until a real one exists for the OS (Linux dev
// boxes / CI, and macOS before platform/macos.rs lands): every emission is
// logged instead of sent, so the whole analog/hysteresis/layer/config
// pipeline — and `cargo test` — runs unchanged without any OS input API.

use crate::key::Key;
use crate::{eprintln, println, SHUTDOWN_REQUESTED};
use std::sync::atomic::Ordering;

pub fn send_key(key: Key, key_up: bool) {
    println!(
        "[stub] send_key {} {} (no OS input backend on this platform)",
        key.name(),
        if key_up { "UP" } else { "DOWN" }
    );
}

// SIGINT/SIGTERM/SIGHUP -> SHUTDOWN_REQUESTED, the same contract as the
// Windows console ctrl handler: the driver loop notices and runs its
// stuck-key cleanup instead of the process just dying.
pub fn install_shutdown_handler() {
    if let Err(e) = ctrlc::set_handler(|| SHUTDOWN_REQUESTED.store(true, Ordering::SeqCst)) {
        eprintln!(
            "WARNING: failed to install Ctrl+C handler ({e}) — stopping via Ctrl+C may leave a \
             key stuck down if one happens to be held at that exact moment."
        );
    }
}

pub fn detach_console() {}

#[allow(dead_code)] // only the (Windows-only, for now) tray menu calls this
pub fn open_url(url: &str) {
    let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
    if let Err(e) = std::process::Command::new(opener).arg(url).spawn() {
        eprintln!("WARNING: could not open {url} via {opener}: {e}");
    }
}

pub fn check_input_permissions() {}

// Only macOS has the "launched via sudo but files belong to the user" case.
pub fn give_back_to_sudo_user(_path: &std::path::Path) {}

pub fn hid_open_hint(_err: &hidapi::HidError) -> Option<&'static str> {
    None
}

pub fn spawn_input_capture(_ctrl: &Option<std::sync::Arc<std::sync::Mutex<hidapi::HidDevice>>>) {
    eprintln!(
        "WARNING: D-pad/wheel/middle-click remap and Hyper Shift are not available on this \
         platform yet — the Tartarus's own arrow/Alt/wheel events pass through unmodified."
    );
}
