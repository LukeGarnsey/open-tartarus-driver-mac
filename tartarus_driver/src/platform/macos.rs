// macOS backend. Phase 2 of docs/MACOS_PORT_PLAN.md: real key emission via
// CoreGraphics + AppKit, the Accessibility permission prompt, and the
// shutdown/URL helpers. Phase 3 (D-pad / wheel / Hyper Shift by seizing
// the Tartarus's own HID interfaces) replaces `spawn_input_capture`.
//
// Every fact below that says "verified" was checked on real hardware on
// 2026-09-16 with examples/mac_probe.rs (results table in the plan).
//
// Key emission design
// -------------------
// Windows' SendInput takes a VK and the OS works out the rest, including
// merging the keystroke with whatever modifiers are physically held.
// CoreGraphics is lower level: a keyboard event carries its OWN modifier
// flags, and a letter posted with flags=0 types lowercase even while the
// user holds Shift on a real keyboard. So this backend tracks the
// modifiers it has pressed itself (`HELD_FLAGS`) and stamps every event
// with (flags the event source reports for the physical keyboard) |
// HELD_FLAGS — the source is created with
// kCGEventSourceStateCombinedSessionState precisely so freshly created
// events start out carrying the real keyboard's modifier state, matching
// what SendInput does for free. Left/Right variants are distinguished by
// the NX_DEVICE*KEYMASK bits alongside the generic kCGEventFlagMask* bit,
// which is what lets a remapped RALT differ from LALT (and why `enigo`,
// which can't set those, was rejected in the plan).
//
// Media/volume keys are not keyboard events at all on macOS: they are
// NSEvent "system-defined" events (type 14, subtype 8) whose data1 packs
// the NX_KEYTYPE_* code and a key-down/up nibble. Built through AppKit's
// NSEvent, converted to a CGEvent and posted the same way. Verified:
// VOLUME_UP shows the HUD, MEDIA_PLAY_PAUSE toggles Music.
//
// Tests never reach send_key (same rule as Windows): the modifier
// bookkeeping is a pure function (`next_flags`) tested on its own.

use crate::key::{Key, MacKey};
use crate::{eprintln, println, SHUTDOWN_REQUESTED};
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::OnceLock;

// ---------------------------------------------------------------------------
// Raw FFI — deliberately hand-rolled (no core-graphics / objc2-app-kit
// crates) so the dependency surface stays tiny and every call is visible.
// ---------------------------------------------------------------------------

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn CGEventSourceCreate(state_id: i32) -> *mut c_void;
    fn CGEventCreateKeyboardEvent(source: *mut c_void, virtual_key: u16, key_down: bool) -> *mut c_void;
    fn CGEventGetFlags(event: *mut c_void) -> u64;
    fn CGEventSetFlags(event: *mut c_void, flags: u64);
    fn CGEventPost(tap: u32, event: *mut c_void);
    fn AXIsProcessTrustedWithOptions(options: *const c_void) -> bool;
    static kAXTrustedCheckOptionPrompt: *const c_void;
}
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(cf: *const c_void);
    fn CFDictionaryCreate(
        allocator: *const c_void,
        keys: *const *const c_void,
        values: *const *const c_void,
        num_values: isize,
        key_callbacks: *const c_void,
        value_callbacks: *const c_void,
    ) -> *const c_void;
    static kCFBooleanTrue: *const c_void;
    static kCFTypeDictionaryKeyCallBacks: c_void;
    static kCFTypeDictionaryValueCallBacks: c_void;
}
// AppKit has to be linked for `class!(NSEvent)` to resolve at runtime.
#[link(name = "AppKit", kind = "framework")]
unsafe extern "C" {}
unsafe extern "C" {
    fn geteuid() -> u32;
}

const CG_EVENT_SOURCE_STATE_COMBINED_SESSION: i32 = 0;
const CG_HID_EVENT_TAP: u32 = 0;

// CGEventFlags (CoreGraphics/CGEventTypes.h)
const CG_FLAG_SHIFT: u64 = 1 << 17;
const CG_FLAG_CONTROL: u64 = 1 << 18;
const CG_FLAG_ALTERNATE: u64 = 1 << 19;
const CG_FLAG_COMMAND: u64 = 1 << 20;
// NX_DEVICE*KEYMASK (IOKit/hidsystem/IOLLEvent.h): the left/right-specific
// companions to the generic bits above.
const NX_DEVICE_LCTL: u64 = 0x0000_0001;
const NX_DEVICE_LSHIFT: u64 = 0x0000_0002;
const NX_DEVICE_RSHIFT: u64 = 0x0000_0004;
const NX_DEVICE_LCMD: u64 = 0x0000_0008;
const NX_DEVICE_RCMD: u64 = 0x0000_0010;
const NX_DEVICE_LALT: u64 = 0x0000_0020;
const NX_DEVICE_RALT: u64 = 0x0000_0040;
const NX_DEVICE_RCTL: u64 = 0x0000_2000;

// ---------------------------------------------------------------------------
// Modifier bookkeeping (pure — unit-tested below)
// ---------------------------------------------------------------------------

// (generic CGEventFlags bit, device-specific NX bit) for the modifier keys
// this driver can press; None for everything else.
fn modifier_bits(key: Key) -> Option<(u64, u64)> {
    match key {
        Key::LShift => Some((CG_FLAG_SHIFT, NX_DEVICE_LSHIFT)),
        Key::RShift => Some((CG_FLAG_SHIFT, NX_DEVICE_RSHIFT)),
        Key::LCtrl => Some((CG_FLAG_CONTROL, NX_DEVICE_LCTL)),
        Key::RCtrl => Some((CG_FLAG_CONTROL, NX_DEVICE_RCTL)),
        Key::LAlt => Some((CG_FLAG_ALTERNATE, NX_DEVICE_LALT)),
        Key::RAlt => Some((CG_FLAG_ALTERNATE, NX_DEVICE_RALT)),
        Key::LCmd => Some((CG_FLAG_COMMAND, NX_DEVICE_LCMD)),
        Key::RCmd => Some((CG_FLAG_COMMAND, NX_DEVICE_RCMD)),
        _ => None,
    }
}

// Given the modifiers this driver currently holds (`held`, NX device bits
// + generic bits), the key being sent, and its direction, returns
// (held after this event, bits to force ON in this event's flags, bits to
// force OFF). The generic bit (e.g. Shift) is only cleared when neither
// side is still held by us — releasing LSHIFT while RSHIFT is down must
// keep Shift set.
fn next_flags(held: u64, key: Key, key_up: bool) -> (u64, u64, u64) {
    let Some((generic, device)) = modifier_bits(key) else {
        return (held, held, 0);
    };
    if !key_up {
        let held = held | generic | device;
        return (held, held, 0);
    }
    let mut held = held & !device;
    let sibling_still_held = match generic {
        CG_FLAG_SHIFT => held & (NX_DEVICE_LSHIFT | NX_DEVICE_RSHIFT),
        CG_FLAG_CONTROL => held & (NX_DEVICE_LCTL | NX_DEVICE_RCTL),
        CG_FLAG_ALTERNATE => held & (NX_DEVICE_LALT | NX_DEVICE_RALT),
        _ => held & (NX_DEVICE_LCMD | NX_DEVICE_RCMD),
    } != 0;
    let mut clear = device;
    if !sibling_still_held {
        held &= !generic;
        clear |= generic;
    }
    (held, held, clear)
}

static HELD_FLAGS: AtomicU64 = AtomicU64::new(0);

// ---------------------------------------------------------------------------
// Emission
// ---------------------------------------------------------------------------

// CGEventSourceRef is a CF object; CoreGraphics' event-creation calls are
// safe to use from any thread, so one process-wide source is enough.
struct EventSource(*mut c_void);
unsafe impl Send for EventSource {}
unsafe impl Sync for EventSource {}

fn event_source() -> *mut c_void {
    static SOURCE: OnceLock<EventSource> = OnceLock::new();
    SOURCE
        .get_or_init(|| EventSource(unsafe { CGEventSourceCreate(CG_EVENT_SOURCE_STATE_COMBINED_SESSION) }))
        .0
}

fn post_keyboard_event(code: u16, key_up: bool, set: u64, clear: u64) {
    unsafe {
        let ev = CGEventCreateKeyboardEvent(event_source(), code, !key_up);
        if ev.is_null() {
            eprintln!("WARNING: CGEventCreateKeyboardEvent failed for keycode {code:#x}");
            return;
        }
        // Start from the physical keyboard's modifiers (see module doc),
        // then apply what this driver holds.
        let flags = (CGEventGetFlags(ev) & !clear) | set;
        CGEventSetFlags(ev, flags);
        CGEventPost(CG_HID_EVENT_TAP, ev);
        CFRelease(ev);
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct NSPoint {
    x: f64,
    y: f64,
}
unsafe impl objc2::encode::Encode for NSPoint {
    const ENCODING: objc2::encode::Encoding = objc2::encode::Encoding::Struct(
        "CGPoint",
        &[objc2::encode::Encoding::Double, objc2::encode::Encoding::Double],
    );
}

// NSEvent SystemDefined (type 14) subtype 8: data1 = (NX_KEYTYPE << 16) |
// (0x0A for key-down / 0x0B for key-up) << 8. modifierFlags 0xA00/0xB00
// mirror the same nibble — that is what the real media keys send.
fn post_media_event(nx_keytype: u8, key_up: bool) {
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};
    const NS_EVENT_TYPE_SYSTEM_DEFINED: usize = 14;
    let state: isize = if key_up { 0x0B } else { 0x0A };
    let data1: isize = ((nx_keytype as isize) << 16) | (state << 8);
    let flags: usize = if key_up { 0xB00 } else { 0xA00 };
    objc2::rc::autoreleasepool(|_| unsafe {
        let ev: *mut AnyObject = msg_send![
            class!(NSEvent),
            otherEventWithType: NS_EVENT_TYPE_SYSTEM_DEFINED,
            location: NSPoint { x: 0.0, y: 0.0 },
            modifierFlags: flags,
            timestamp: 0.0f64,
            windowNumber: 0isize,
            context: std::ptr::null::<AnyObject>(),
            subtype: 8i16,
            data1: data1,
            data2: -1isize
        ];
        if ev.is_null() {
            eprintln!("WARNING: NSEvent creation failed for media key {nx_keytype}");
            return;
        }
        // The CGEvent is owned by the NSEvent (released with the pool).
        let cg: *mut c_void = msg_send![ev, CGEvent];
        CGEventPost(CG_HID_EVENT_TAP, cg);
    });
}

pub fn send_key(key: Key, key_up: bool) {
    match key.mac() {
        MacKey::Code(code) => {
            // fetch_update so two threads (analog loop + Phase 3 capture
            // thread) can't lose each other's modifier edges.
            let mut set = 0;
            let mut clear = 0;
            let _ = HELD_FLAGS.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |held| {
                let (next, s, c) = next_flags(held, key, key_up);
                set = s;
                clear = c;
                Some(next)
            });
            post_keyboard_event(code, key_up, set, clear);
        }
        MacKey::Media(nx) => post_media_event(nx, key_up),
        // Config parsing (Key::from_name) already refuses these on macOS,
        // so this only fires for a built-in default that has no macOS
        // equivalent. Once per process is enough.
        MacKey::Unsupported => {
            static WARNED: AtomicBool = AtomicBool::new(false);
            if !WARNED.swap(true, Ordering::SeqCst) {
                eprintln!(
                    "WARNING: {} has no macOS equivalent and is ignored (this is logged once).",
                    key.name()
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Permissions
// ---------------------------------------------------------------------------

// Accessibility (TCC) is what gates CGEventPost. Without it every event is
// silently dropped — no error, nothing typed — so the driver asks up front
// and says so in the log. The `prompt` option makes macOS show its own
// "wants to control this computer" dialog the first time; grants are
// attributed to the app the process was launched from (Terminal.app for a
// terminal run), which USAGE.md explains.
fn accessibility_trusted(prompt: bool) -> bool {
    unsafe {
        if !prompt {
            return AXIsProcessTrustedWithOptions(std::ptr::null());
        }
        let keys = [kAXTrustedCheckOptionPrompt];
        let values = [kCFBooleanTrue];
        let opts = CFDictionaryCreate(
            std::ptr::null(),
            keys.as_ptr(),
            values.as_ptr(),
            1,
            &kCFTypeDictionaryKeyCallBacks as *const c_void,
            &kCFTypeDictionaryValueCallBacks as *const c_void,
        );
        let trusted = AXIsProcessTrustedWithOptions(opts);
        if !opts.is_null() {
            CFRelease(opts);
        }
        trusted
    }
}

pub fn check_input_permissions() {
    if accessibility_trusted(true) {
        println!("Accessibility permission: granted.");
    } else {
        eprintln!(
            "WARNING: Accessibility permission NOT granted — analog keys will be detected but \
             nothing will be typed. Allow the app you launched this from (e.g. Terminal) under \
             System Settings > Privacy & Security > Accessibility, then restart the driver."
        );
    }
    println!(
        "If opening the keypad fails with 'not permitted' (0xE00002E2), also allow it under \
         Privacy & Security > Input Monitoring."
    );
}

// hidapi wraps IOKit's kIOReturnNotPermitted (0xE00002E2) in a plain
// string; that code means the Input Monitoring grant is missing.
pub fn hid_open_hint(err: &hidapi::HidError) -> Option<&'static str> {
    let msg = err.to_string();
    if msg.contains("E00002E2") || msg.contains("not permitted") {
        Some("Input Monitoring permission is missing — allow the app you launched this from under System Settings > Privacy & Security > Input Monitoring, then restart.")
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Process helpers
// ---------------------------------------------------------------------------

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
// otherwise (verified: 0xE00002C1 as a user, OK under sudo). Checked here
// rather than after a failed open so the log says exactly what to do.
pub fn is_root() -> bool {
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

// Inside a `.app` bundle the executable sits at
// Foo.app/Contents/MacOS/<exe>; writing config.toml / logs/ next to it
// would modify the bundle and break its code signature (and with it the
// TCC grants keyed to that signature). Bundled runs therefore keep their
// files in ~/Library/Application Support/open-tartarus-driver/ instead.
// Returns None for a bare CLI binary so main.rs's usual exe-relative
// lookup applies.
pub fn bundle_app_root(exe_dir: &std::path::Path) -> Option<std::path::PathBuf> {
    let in_bundle = exe_dir.file_name().is_some_and(|n| n == "MacOS")
        && exe_dir.parent().and_then(|p| p.file_name()).is_some_and(|n| n == "Contents");
    if !in_bundle {
        return None;
    }
    let home = std::env::var_os("HOME")?;
    Some(
        std::path::PathBuf::from(home)
            .join("Library")
            .join("Application Support")
            .join("open-tartarus-driver"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_keys_carry_held_modifiers_and_change_nothing() {
        let held = CG_FLAG_SHIFT | NX_DEVICE_LSHIFT;
        assert_eq!(next_flags(held, Key::A, false), (held, held, 0));
        assert_eq!(next_flags(held, Key::A, true), (held, held, 0));
        assert_eq!(next_flags(0, Key::F13, false), (0, 0, 0));
    }

    #[test]
    fn modifier_press_and_release_round_trip() {
        let (held, set, clear) = next_flags(0, Key::LCmd, false);
        assert_eq!(held, CG_FLAG_COMMAND | NX_DEVICE_LCMD);
        assert_eq!(set, held);
        assert_eq!(clear, 0);
        let (held, set, clear) = next_flags(held, Key::LCmd, true);
        assert_eq!(held, 0);
        assert_eq!(set, 0);
        assert_eq!(clear, CG_FLAG_COMMAND | NX_DEVICE_LCMD);
    }

    #[test]
    fn releasing_one_side_keeps_generic_bit_while_other_side_held() {
        let (held, _, _) = next_flags(0, Key::LShift, false);
        let (held, _, _) = next_flags(held, Key::RShift, false);
        assert_eq!(held, CG_FLAG_SHIFT | NX_DEVICE_LSHIFT | NX_DEVICE_RSHIFT);
        let (held, set, clear) = next_flags(held, Key::LShift, true);
        assert_eq!(held, CG_FLAG_SHIFT | NX_DEVICE_RSHIFT);
        assert_eq!(set, held);
        assert_eq!(clear, NX_DEVICE_LSHIFT, "generic Shift must survive");
        let (held, _, clear) = next_flags(held, Key::RShift, true);
        assert_eq!(held, 0);
        assert_eq!(clear, CG_FLAG_SHIFT | NX_DEVICE_RSHIFT);
    }

    #[test]
    fn left_and_right_variants_differ_only_in_device_bit() {
        for (l, r) in [
            (Key::LShift, Key::RShift),
            (Key::LCtrl, Key::RCtrl),
            (Key::LAlt, Key::RAlt),
            (Key::LCmd, Key::RCmd),
        ] {
            let (lg, ld) = modifier_bits(l).unwrap();
            let (rg, rd) = modifier_bits(r).unwrap();
            assert_eq!(lg, rg);
            assert_ne!(ld, rd);
        }
    }

    #[test]
    fn bundle_root_only_for_app_layout() {
        use std::path::Path;
        assert!(bundle_app_root(Path::new("/x/target/release")).is_none());
        assert!(bundle_app_root(Path::new("/Applications/Tartarus Driver.app/Contents")).is_none());
        let root = bundle_app_root(Path::new("/Applications/Tartarus Driver.app/Contents/MacOS")).unwrap();
        assert!(root.ends_with("Library/Application Support/open-tartarus-driver"));
    }
}
