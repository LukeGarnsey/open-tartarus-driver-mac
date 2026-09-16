// OS-neutral half of the D-pad / scroll-wheel / wheel-click / Hyper Response
// remap: given "the Tartarus's D-pad LEFT just went down" (etc.), decide what
// to emit and keep the held-key bookkeeping. Which OS mechanism actually
// observes and suppresses the physical event — Windows: the Interception
// kernel driver (dpad.rs); macOS: seizing the device's HID interfaces
// (platform/macos.rs) — is the caller's business; both funnel every edge
// through the handlers here so the emit logic exists exactly once.
//
// The physical facts these handlers rely on are the same on every OS: the
// D-pad sends plain arrow keys and the Hyper Response thumb button a plain
// Alt through the device's boot keyboard interface; the wheel is a standard
// mouse wheel and the wheel-click a standard middle button.

// Only the Windows (dpad.rs) and macOS (platform/macos.rs) backends drive
// these handlers; the stub backend for other OSes has no capture at all.
#![cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))]

use crate::config::DpadKeymap;
use crate::key::Key;
use crate::{cfg, println, send_key};
use std::sync::Mutex;

// TEST/PLACEHOLDER D-pad keymap — same throwaway style as main.rs's
// TEST_KEYMAP. The letters deliberately avoid everything TEST_KEYMAP
// ('1'..'0', 'A'..'J') and LAYER1_TEST_KEYMAP (F1..F20) already use (which
// rules out the obvious W/A/S/D set: A and D are taken), so remapped output
// is unambiguous during testing. These are the config module's built-in
// defaults for the D-pad/wheel/middle-click (see config::DriverConfig::defaults()).
pub const DPAD_ARROW_TEST_KEYMAP_LEFT: Key = Key::K;
pub const DPAD_ARROW_TEST_KEYMAP_UP: Key = Key::W;
pub const DPAD_ARROW_TEST_KEYMAP_RIGHT: Key = Key::L;
pub const DPAD_ARROW_TEST_KEYMAP_DOWN: Key = Key::S;
pub const WHEEL_UP_TEST_KEY: Key = Key::O; // wheel up -> 'O' (tap per notch)
pub const WHEEL_DOWN_TEST_KEY: Key = Key::P; // wheel down -> 'P' (tap per notch)
pub const MIDDLE_CLICK_TEST_KEY: Key = Key::M; // middle click -> 'M' (held)

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DpadDirection {
    Left,
    Up,
    Right,
    Down,
}

// The configured remap key for one D-pad direction (Phase 4: from
// config.toml, or the built-in placeholder defaults if unset — see
// config/load.rs).
pub fn dpad_key_for(dir: DpadDirection, dpad: &DpadKeymap) -> Key {
    match dir {
        DpadDirection::Left => dpad.left,
        DpadDirection::Up => dpad.up,
        DpadDirection::Right => dpad.right,
        DpadDirection::Down => dpad.down,
    }
}

// Remapped keys currently held down (D-pad arrows / middle-click), so
// main.rs's run_driver can force-release them at shutdown exactly like the
// analog keys.
static HELD_KEYS: Mutex<Vec<Key>> = Mutex::new(Vec::new());

fn send_held_key(key: Key, key_up: bool) {
    send_key(key, key_up);
    if let Ok(mut held) = HELD_KEYS.lock() {
        track_held(&mut held, key, key_up);
    }
}

// The bookkeeping half of send_held_key, separated so it can be unit-tested
// without emitting anything (on Windows, send_key really presses keys).
fn track_held(held: &mut Vec<Key>, key: Key, key_up: bool) {
    if key_up {
        held.retain(|k| *k != key);
    } else if !held.contains(&key) {
        held.push(key);
    }
}

// Shutdown safety: force-release any remapped D-pad/middle-click key still
// logically held so we never leave a stuck key on the OS (mirrors the analog
// force-release at the end of run_driver in main.rs).
pub fn release_held() {
    let held: Vec<Key> = match HELD_KEYS.lock() {
        Ok(mut h) => h.drain(..).collect(),
        Err(_) => return,
    };
    for key in held {
        send_key(key, true);
    }
}

// A confirmed-Tartarus D-pad arrow press/release: emit the configured key
// (held for as long as the arrow is).
pub fn on_dpad_arrow(dir: DpadDirection, key_up: bool) {
    let mapped = dpad_key_for(dir, &cfg().dpad);
    send_held_key(mapped, key_up);
    println!(
        "[dpad] Tartarus D-pad {dir:?} {} -> {}",
        if key_up { "UP  " } else { "DOWN" },
        mapped.name()
    );
}

// A confirmed-Tartarus Hyper Response (physical Alt) press/release: routed
// through hypershift::on_trigger_edge (which layer/key it produces depends on
// config.toml's [hypershift] — see hypershift.rs). The physical Alt itself
// must already have been suppressed by the caller.
pub fn on_hyper_response(key_down: bool) {
    println!(
        "[dpad] Tartarus Hyper Response (Alt) {} edge detected",
        if key_down { "DOWN" } else { "UP  " }
    );
    crate::hypershift::on_trigger_edge(key_down);
}

// One wheel movement from the Tartarus: one key tap (down + up) per notch,
// wheel_up for positive `rolling`, wheel_down for negative. Callers pass the
// OS's raw signed wheel delta; only its sign matters here.
pub fn on_wheel(rolling: i32) {
    let dpad = &cfg().dpad;
    let mapped = if rolling >= 0 { dpad.wheel_up } else { dpad.wheel_down };
    send_key(mapped, false);
    send_key(mapped, true);
    println!("[dpad] Tartarus wheel rolling={rolling} -> {} tap", mapped.name());
}

// The Tartarus's wheel-click (middle button): held key, like the arrows.
pub fn on_middle(key_up: bool) {
    let mapped = cfg().dpad.middle_click;
    send_held_key(mapped, key_up);
    println!(
        "[dpad] Tartarus middle {} -> {}",
        if key_up { "UP  " } else { "DOWN" },
        mapped.name()
    );
}

// One-line summary of the active D-pad/wheel/middle-click assignments for
// the startup log, shared by every OS backend.
pub fn describe_assignments() -> String {
    let dpad = &cfg().dpad;
    format!(
        "Tartarus D-pad -> {}/{}/{}/{}, wheel -> {}/{}, middle-click -> {}",
        dpad.left.name(),
        dpad.up.name(),
        dpad.right.name(),
        dpad.down.name(),
        dpad.wheel_up.name(),
        dpad.wheel_down.name(),
        dpad.middle_click.name()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directions_map_to_the_expected_test_keys() {
        let dpad = crate::config::DriverConfig::defaults().dpad;
        assert_eq!(dpad_key_for(DpadDirection::Left, &dpad), DPAD_ARROW_TEST_KEYMAP_LEFT);
        assert_eq!(dpad_key_for(DpadDirection::Up, &dpad), DPAD_ARROW_TEST_KEYMAP_UP);
        assert_eq!(dpad_key_for(DpadDirection::Right, &dpad), DPAD_ARROW_TEST_KEYMAP_RIGHT);
        assert_eq!(dpad_key_for(DpadDirection::Down, &dpad), DPAD_ARROW_TEST_KEYMAP_DOWN);
    }

    #[test]
    fn held_key_tracker_dedupes_presses_and_drops_releases() {
        let mut held = Vec::new();
        track_held(&mut held, Key::K, false);
        track_held(&mut held, Key::M, false);
        track_held(&mut held, Key::K, false); // duplicate press is not double-tracked
        assert_eq!(held, [Key::K, Key::M]);
        track_held(&mut held, Key::K, true);
        assert_eq!(held, [Key::M]);
        track_held(&mut held, Key::K, true); // releasing an untracked key is a no-op
        assert_eq!(held, [Key::M]);
    }
}
