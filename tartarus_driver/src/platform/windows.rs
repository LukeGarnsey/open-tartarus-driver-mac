// Windows backend: SendInput-based key emission plus the console/shell
// helpers, moved verbatim out of main.rs / tray.rs for the macOS port.
// Behaviour is unchanged — every call site just goes through
// crate::platform now.

use crate::key::Key;
use crate::{eprintln, SHUTDOWN_REQUESTED};
use std::sync::atomic::Ordering;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::BOOL;
use windows::Win32::System::Console::{FreeConsole, SetConsoleCtrlHandler};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_EXTENDEDKEY,
    KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, VIRTUAL_KEY,
};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

// Left/Right pairs whose scan codes SendInput's own wVk-only path (wScan=0,
// no KEYEVENTF_SCANCODE — what every other key here relies on) does not
// reliably preserve. Confirmed on real hardware/Valorant (2026-07-27):
// LSHIFT arrived as RSHIFT, and RSHIFT arrived as nothing at all. Unlike
// Ctrl/Alt, Shift's two sides don't share a base scan code distinguished by
// the extended-key flag — they are genuinely different PC/AT Set 1 codes
// (0x2A vs 0x36) with no such flag to fall back on, so this pair is where
// Windows' own wVk->scancode derivation is known to fail. Ctrl/Alt use the
// same base code (Ctrl=0x1D, Alt=0x38) as their Left variant, extended for
// Right — included here too since they share the same theoretical risk
// (SendInput deriving the wrong one from wVk alone), even though only Shift
// has been confirmed broken in practice so far.
//
// Every other key (letters, digits, F-keys, media keys, etc.) keeps using
// the plain wVk-only path below unchanged, since that's already
// hardware-verified working — this table only overrides the handful of
// keys where it isn't.
fn explicit_scan_code(key: Key) -> Option<(u16, bool)> {
    // (scan code, is_extended)
    match key {
        Key::LShift => Some((0x2A, false)),
        Key::RShift => Some((0x36, false)),
        Key::LCtrl => Some((0x1D, false)),
        Key::RCtrl => Some((0x1D, true)),
        Key::LAlt => Some((0x38, false)),
        Key::RAlt => Some((0x38, true)),
        _ => None,
    }
}

pub fn send_key(key: Key, key_up: bool) {
    let key_up_flag = if key_up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) };
    let ki = match explicit_scan_code(key) {
        // Scan-code-based send: wVk is ignored by SendInput once
        // KEYEVENTF_SCANCODE is set, so it's left at 0 per the documented
        // contract for that flag.
        Some((scan, extended)) => {
            let mut flags = key_up_flag | KEYEVENTF_SCANCODE;
            if extended {
                flags |= KEYEVENTF_EXTENDEDKEY;
            }
            KEYBDINPUT {
                wVk: VIRTUAL_KEY(0),
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            }
        }
        // Unchanged, hardware-verified path for every other key.
        None => KEYBDINPUT {
            wVk: VIRTUAL_KEY(key.win_vk()),
            wScan: 0,
            dwFlags: key_up_flag,
            time: 0,
            dwExtraInfo: 0,
        },
    };
    let input = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 { ki },
    };
    unsafe {
        SendInput(&[input], std::mem::size_of::<INPUT>() as i32);
    }
}

// Ctrl+C, Ctrl+Break, console window closed, logoff, or shutdown: set the
// flag so the main analog-read loop can notice and exit its own way —
// running the existing "force-release any key still logically held"
// cleanup — instead of Windows just killing the process outright, which
// would skip that cleanup and could leave a key stuck down on the OS. The
// loop's sleep granularity (500us) means this is noticed almost
// immediately, well within the few seconds Windows grants a console
// handler to actually exit.
unsafe extern "system" fn console_ctrl_handler(_ctrl_type: u32) -> BOOL {
    SHUTDOWN_REQUESTED.store(true, Ordering::SeqCst);
    BOOL(1) // handled: don't run Windows' default action (immediate termination)
}

pub fn install_shutdown_handler() {
    unsafe {
        if SetConsoleCtrlHandler(Some(console_ctrl_handler), true).is_err() {
            eprintln!(
                "WARNING: failed to install Ctrl+C handler — stopping via Ctrl+C may leave a \
                 key stuck down if one happens to be held at that exact moment. Ctrl+C still \
                 works to end the process, just without the usual cleanup."
            );
        }
    }
}

// Best-effort — there may not even be a console (e.g. launched from a
// shortcut), and that's fine.
pub fn detach_console() {
    unsafe {
        let _ = FreeConsole();
    }
}

pub fn open_url(url: &str) {
    let wide: Vec<u16> = url.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        // Fire-and-forget: ShellExecuteW's return value here is an HINSTANCE
        // (legacy ABI quirk), not worth inspecting — worst case the browser
        // just doesn't open, which the user notices immediately.
        let _ = ShellExecuteW(None, w!("open"), PCWSTR(wide.as_ptr()), None, None, SW_SHOWNORMAL);
    }
}

// Windows needs no runtime permission for SendInput or HID reads.
pub fn check_input_permissions() {}

pub fn hid_open_hint(_err: &hidapi::HidError) -> Option<&'static str> {
    None
}

// D-pad / wheel / middle-click remap + device-aware Hypershift via the
// Interception kernel driver (see dpad.rs). Falls back internally to the
// WH_KEYBOARD_LL hook when Interception isn't installed.
// `_ctrl` (the Interface 2 handle) is only needed by the macOS backend;
// Interception observes the wheel/middle-click on its own.
pub fn spawn_input_capture(_ctrl: &Option<std::sync::Arc<std::sync::Mutex<hidapi::HidDevice>>>) {
    crate::dpad::spawn_interception_thread();
}
