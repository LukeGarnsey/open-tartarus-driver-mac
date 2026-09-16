// Phase 0 hardware-discovery spike for the macOS port (docs/MACOS_PORT_PLAN.md).
// Throwaway: NOT shipped, NOT built on other OSes. Each subcommand answers one
// open question the plan lists; run them in order and paste the output into
// the plan's "Verified macOS facts" (or fix the plan where reality differs).
//
//   cargo build --release --example mac_probe
//   P=./target/release/examples/mac_probe
//
//   $P enumerate            step 1  which IOHIDDevice is IF0/IF1/IF2, usages, dedupe
//   $P analog               step 2  unlock via IF2 (shared), read 0x06 on IF1 — no root
//   $P raw <if> [secs]      step 3  hexdump changed reports on interface <if> (shared)
//   $P seize <if> [secs]    step 4  open <if> EXCLUSIVELY: expect IF2 OK as user,
//                                   IF0 kIOReturnNotPrivileged as user / OK under sudo
//   $P dualif2 [secs]       Phase 3 design check: shared IF2 handle keeps
//                                   sending feature reports (lighting) while a
//                                   SECOND handle in the same process has IF2 seized
//   $P emit                 step 5  CGEventPost + NSEvent media keys (focus TextEdit)
//   $P raw 1  (x2 terminals) step 6 both processes must keep receiving 0x06
//
// Run `log stream --predicate 'subsystem == "com.apple.iokit.IOUSBHostFamily"'`
// in another terminal during `analog` to spot the OpenRazer #2710 reset loop.
// Quit Razer Synapse / razer-macos first.

// `cargo test` compiles examples on every CI OS; a fully cfg'd-out binary
// fails with E0601 (no main), so non-macOS gets a stub main instead.
#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("mac_probe is a macOS-only hardware spike; nothing to do on this OS.");
}

#[cfg(target_os = "macos")]
mod probe {
use hidapi::{DeviceInfo, HidApi, HidDevice};
use std::ffi::c_void;
use std::time::{Duration, Instant};

const VID: u16 = 0x1532;
const PID: u16 = 0x0244;

pub(super) fn main() {
    let args: Vec<String> = std::env::args().collect();
    let secs = |i: usize, default: u64| args.get(i).and_then(|s| s.parse().ok()).unwrap_or(default);
    match args.get(1).map(String::as_str) {
        Some("enumerate") => enumerate(),
        Some("analog") => analog(secs(2, 15)),
        Some("raw") => raw(iface(&args), secs(3, 15)),
        Some("seize") => seize(iface(&args), secs(3, 10)),
        Some("dualif2") => dual_if2(secs(2, 15)),
        Some("emit") => emit(secs(2, 8)),
        _ => {
            eprintln!("usage: mac_probe enumerate | analog [secs] | raw <if> [secs] | seize <if> [secs] | emit [delay]");
            std::process::exit(2);
        }
    }
}

fn iface(args: &[String]) -> i32 {
    args.get(2).and_then(|s| s.parse().ok()).unwrap_or_else(|| {
        eprintln!("need an interface number (0, 1 or 2)");
        std::process::exit(2)
    })
}

fn api() -> HidApi {
    let api = HidApi::new().expect("hidapi init failed");
    println!(
        "hidapi open mode: {} (macos-shared-device feature => expect non-exclusive)",
        if api.get_open_exclusive() { "EXCLUSIVE" } else { "non-exclusive" }
    );
    api
}

fn tartarus(api: &HidApi) -> Vec<DeviceInfo> {
    api.device_list()
        .filter(|d| d.vendor_id() == VID && d.product_id() == PID)
        .cloned()
        .collect()
}

// One DeviceInfo per interface (first entry per path wins), keyed by
// interface_number. hidapi's macOS backend reads kUSBInterfaceNumber from
// the IORegistry; -1 means it couldn't, in which case the raw/seize
// subcommands can't address interfaces and enumerate's usage dump has to
// be matched by hand.
fn by_interface(api: &HidApi, want: i32) -> Option<DeviceInfo> {
    tartarus(api).into_iter().find(|d| d.interface_number() == want)
}

fn hexdump(buf: &[u8]) -> String {
    buf.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" ")
}

// ---------------------------------------------------------------- step 1

fn enumerate() {
    let api = api();
    let infos = tartarus(&api);
    if infos.is_empty() {
        eprintln!("Tartarus Pro {VID:#06x}:{PID:#06x} not found — plugged in? Synapse quit?");
        std::process::exit(1);
    }
    println!("\n{} DeviceInfo entries for the Tartarus Pro:", infos.len());
    println!("{:<4} {:<10} {:<8} {:<20} path", "if#", "usage_pg", "usage", "product");
    for d in &infos {
        println!(
            "{:<4} {:#06x}     {:#06x}   {:<20} {}",
            d.interface_number(),
            d.usage_page(),
            d.usage(),
            d.product_string().unwrap_or("?"),
            d.path().to_string_lossy()
        );
    }
    let mut paths: Vec<_> = infos.iter().map(|d| d.path().to_owned()).collect();
    paths.sort();
    paths.dedup();
    println!(
        "\n{} distinct path(s) => {} IOHIDDevice(s). Expect 3 (IF0 boot kbd, IF1 analog, IF2 control/mouse).",
        paths.len(),
        paths.len()
    );
    for p in &paths {
        let usages: Vec<String> = infos
            .iter()
            .filter(|d| d.path() == p.as_c_str())
            .map(|d| format!("{:#06x}/{:#06x}", d.usage_page(), d.usage()))
            .collect();
        let first = infos.iter().find(|d| d.path() == p.as_c_str()).unwrap();
        let primary = usages.first().cloned().unwrap_or_default();
        let needs_root = first.usage_page() == 0x0001 && (first.usage() == 0x0006 || first.usage() == 0x0007);
        println!(
            "  if{}  primary usage {}  all [{}]  {}",
            first.interface_number(),
            primary,
            usages.join(", "),
            if needs_root { "<- keyboard usage: seize needs root" } else { "" }
        );
    }
    println!("\nWhat the driver's analog_device_infos() filter would keep (non-boot, deduped):");
    let mut seen = std::collections::HashSet::new();
    for d in infos
        .iter()
        .filter(|d| !(d.usage_page() == 0x0001 && (d.usage() == 0x0002 || d.usage() == 0x0006)))
        .filter(|d| seen.insert(d.path().to_owned()))
    {
        println!("  if{} {:#06x}/{:#06x}", d.interface_number(), d.usage_page(), d.usage());
    }
}

// ---------------------------------------------------------------- step 2

fn open_shared(api: &HidApi, info: &DeviceInfo) -> HidDevice {
    match info.open_device(api) {
        Ok(d) => {
            println!(
                "opened if{} {:#06x}/{:#06x} (exclusive={:?})",
                info.interface_number(),
                info.usage_page(),
                info.usage(),
                d.is_open_exclusive()
            );
            d
        }
        Err(e) => {
            eprintln!(
                "open if{} FAILED: {e}\n  (first time? System Settings > Privacy & Security > Input Monitoring \
                 must list your terminal app — grant it, then re-run)",
                info.interface_number()
            );
            std::process::exit(1);
        }
    }
}

fn build_razer_cmd(txn: u8, class: u8, cmd: u8, args: &[u8]) -> [u8; 91] {
    let mut buf = [0u8; 91];
    buf[2] = txn;
    buf[6] = args.len() as u8;
    buf[7] = class;
    buf[8] = cmd;
    buf[9..9 + args.len()].copy_from_slice(args);
    let mut crc = 0u8;
    for b in &buf[3..89] {
        crc ^= *b;
    }
    buf[89] = crc;
    buf
}

fn analog(secs: u64) {
    let api = api();
    let ctrl_info = by_interface(&api, 2).unwrap_or_else(|| {
        eprintln!("no interface_number==2 entry; run `enumerate` and check interface numbers");
        std::process::exit(1)
    });
    let analog_info = by_interface(&api, 1).unwrap_or_else(|| {
        eprintln!("no interface_number==1 entry; run `enumerate`");
        std::process::exit(1)
    });
    let ctrl = open_shared(&api, &ctrl_info);
    let analog = open_shared(&api, &analog_info);

    let cmd = build_razer_cmd(0x01, 0x00, 0x04, &[0x03, 0x00]);
    match ctrl.send_feature_report(&cmd) {
        Ok(()) => println!("sent device-mode-3 unlock to if2"),
        Err(e) => eprintln!("unlock send_feature_report FAILED: {e}"),
    }
    println!(
        "reading if1 for {secs}s — press analog keys. Expect a first all-zero 0x06 report within ms, \
         then one per change. Watch `log stream` for USB re-enumeration."
    );
    dump_reports(&analog, secs, Some(0x06));
}

// Prints every report whose bytes differ from the previous one. `expect_id`
// flags whether byte[0] carries the report ID we think it does — on macOS
// hidapi only prepends the ID byte for numbered reports, so a length of 20
// (not 21) or a byte[0] that moves with key depth means IF1 is unnumbered.
fn dump_reports(dev: &HidDevice, secs: u64, expect_id: Option<u8>) {
    let deadline = Instant::now() + Duration::from_secs(secs);
    let mut buf = [0u8; 256];
    let mut last: Vec<u8> = Vec::new();
    let mut count = 0usize;
    let start = Instant::now();
    while Instant::now() < deadline {
        let n = match dev.read_timeout(&mut buf, 100) {
            Ok(n) => n,
            Err(e) => {
                eprintln!("read error: {e} (device unplugged?)");
                return;
            }
        };
        if n == 0 {
            continue;
        }
        count += 1;
        if buf[..n] != last[..] {
            let tag = match expect_id {
                Some(id) if buf[0] == id => "id-ok ",
                Some(_) => "id-?? ",
                None => "",
            };
            println!("[{:7.3}s] len={n:<3} {tag}{}", start.elapsed().as_secs_f64(), hexdump(&buf[..n]));
            last = buf[..n].to_vec();
        }
    }
    println!("{count} report(s) total, done.");
}

// ---------------------------------------------------------------- step 3

fn raw(if_no: i32, secs: u64) {
    let api = api();
    let info = by_interface(&api, if_no).unwrap_or_else(|| {
        eprintln!("no interface_number=={if_no} entry; run `enumerate`");
        std::process::exit(1)
    });
    let dev = open_shared(&api, &info);
    match if_no {
        0 => println!(
            "if0 = boot keyboard. Press each D-pad direction, then the Hyper Response thumb key.\n\
             Boot protocol: [mods][reserved][k1..k6]; arrows are usages 0x4f right 0x50 left 0x51 down 0x52 up;\n\
             modifier byte L-Alt = 0x04, R-Alt = 0x40. len=8 => unnumbered, len=9 => byte[0] is a report ID."
        ),
        2 => println!(
            "if2 = control/mouse. Roll the wheel up/down and click it (middle).\n\
             Boot mouse: [buttons][x][y][wheel]; middle = buttons bit 2 (0x04); wheel = signed i8."
        ),
        _ => println!("if{if_no}: press things, watch bytes."),
    }
    println!("reading for {secs}s (shared mode — the OS still sees these events too)…");
    dump_reports(&dev, secs, None);
}

// ---------------------------------------------------------------- step 4

fn seize(if_no: i32, secs: u64) {
    let api = api();
    let info = by_interface(&api, if_no).unwrap_or_else(|| {
        eprintln!("no interface_number=={if_no} entry; run `enumerate`");
        std::process::exit(1)
    });
    let root = unsafe { geteuid() } == 0;
    println!("euid={} ({})", unsafe { geteuid() }, if root { "root" } else { "user" });

    // The plan's Phase 3 design, minus the extern "C" hack: hidapi-rs 2.6.6
    // exposes the process-global toggle as HidApi::set_open_exclusive.
    api.set_open_exclusive(true);
    let opened = info.open_device(&api);
    api.set_open_exclusive(false);
    let dev = match opened {
        Ok(d) => {
            println!(
                "SEIZE if{if_no} OK (is_open_exclusive={:?}). The OS should no longer see this interface's \
                 input for the next {secs}s — type in this terminal to verify D-pad arrows / wheel are silent.",
                d.is_open_exclusive()
            );
            d
        }
        Err(e) => {
            println!(
                "SEIZE if{if_no} FAILED: {e}\n  expected for a keyboard-usage interface without sudo \
                 (kIOReturnNotPrivileged / 0xe00002c1)"
            );
            return;
        }
    };
    dump_reports(&dev, secs, None);
    drop(dev);
    println!("released; OS input from if{if_no} should be back.");
}

// ---------------------------------------------------------------- Phase 3 design check

fn static_color_cmd(r: u8, g: u8, b: u8) -> [u8; 91] {
    // lighting.rs: txn 0x1f, class 0x0f (extended matrix), cmd 0x02, args
    // [varstore 0x01, backlight 0x05, effect static 0x01, 0, 0, 1 color, r, g, b]
    build_razer_cmd(0x1f, 0x0f, 0x02, &[0x01, 0x05, 0x01, 0x00, 0x00, 0x01, r, g, b])
}

fn dual_if2(secs: u64) {
    let api = api();
    let info = by_interface(&api, 2).unwrap_or_else(|| {
        eprintln!("no interface_number==2 entry; run `enumerate`");
        std::process::exit(1)
    });
    // 1. what main.rs does: shared control handle first
    let ctrl = open_shared(&api, &info);
    // 2. what the Phase 3 capture thread would do: a second, seized handle
    api.set_open_exclusive(true);
    let seized = info.open_device(&api);
    api.set_open_exclusive(false);
    let seized = match seized {
        Ok(d) => {
            println!("second handle SEIZED if2 (is_open_exclusive={:?})", d.is_open_exclusive());
            d
        }
        Err(e) => {
            println!("second handle seize FAILED: {e} -> Phase 3 must share one handle instead");
            return;
        }
    };
    // 3. lighting through the ORIGINAL shared handle while seized
    match ctrl.send_feature_report(&static_color_cmd(0xff, 0x00, 0x00)) {
        Ok(()) => println!("feature report via shared handle while seized: OK -> pad should be RED"),
        Err(e) => println!("feature report via shared handle while seized: FAILED: {e}"),
    }
    // 4. and through the seized handle itself, for completeness
    match seized.send_feature_report(&static_color_cmd(0x00, 0x00, 0xff)) {
        Ok(()) => println!("feature report via seized handle: OK -> pad should be BLUE"),
        Err(e) => println!("feature report via seized handle: FAILED: {e}"),
    }
    println!("reading wheel/middle via the seized handle for {secs}s (OS should NOT scroll)…");
    dump_reports(&seized, secs, None);
    // 5. does the shared handle ALSO still see input reports? (informational)
    println!("now reading via the SHARED handle for 5s — roll the wheel again:");
    dump_reports(&ctrl, 5, None);
    match ctrl.send_feature_report(&static_color_cmd(0x00, 0xff, 0x00)) {
        Ok(()) => println!("final feature report via shared handle: OK -> pad GREEN"),
        Err(e) => println!("final feature report via shared handle: FAILED: {e}"),
    }
}

// ---------------------------------------------------------------- step 5

// Minimal hand-rolled FFI — exactly the shape Phase 2's send_key will use.
#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn CGEventSourceCreate(state_id: i32) -> *mut c_void;
    fn CGEventCreateKeyboardEvent(source: *mut c_void, virtual_key: u16, key_down: bool) -> *mut c_void;
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
        num: isize,
        key_cb: *const c_void,
        value_cb: *const c_void,
    ) -> *const c_void;
    static kCFBooleanTrue: *const c_void;
    static kCFTypeDictionaryKeyCallBacks: c_void;
    static kCFTypeDictionaryValueCallBacks: c_void;
}
// AppKit must be loaded for `class!(NSEvent)` to resolve at runtime.
#[link(name = "AppKit", kind = "framework")]
unsafe extern "C" {}
unsafe extern "C" {
    fn geteuid() -> u32;
}

const CG_EVENT_SOURCE_STATE_PRIVATE: i32 = -1;
const CG_HID_EVENT_TAP: u32 = 0;
const FLAG_SHIFT: u64 = 1 << 17;
const FLAG_CMD: u64 = 1 << 20;
// NX_DEVICEL*KEYMASK — the left/right-specific bits the plan tracks in HELD_FLAGS.
const NX_DEVICELSHIFTKEYMASK: u64 = 0x02;
const NX_DEVICELCMDKEYMASK: u64 = 0x08;

fn ax_trusted() -> bool {
    unsafe {
        let keys = [kAXTrustedCheckOptionPrompt];
        let vals = [kCFBooleanTrue];
        let dict = CFDictionaryCreate(
            std::ptr::null(),
            keys.as_ptr(),
            vals.as_ptr(),
            1,
            &kCFTypeDictionaryKeyCallBacks as *const c_void,
            &kCFTypeDictionaryValueCallBacks as *const c_void,
        );
        let ok = AXIsProcessTrustedWithOptions(dict);
        CFRelease(dict);
        ok
    }
}

fn post_key(src: *mut c_void, code: u16, down: bool, flags: u64) {
    unsafe {
        let ev = CGEventCreateKeyboardEvent(src, code, down);
        if ev.is_null() {
            eprintln!("CGEventCreateKeyboardEvent returned null for {code:#x}");
            return;
        }
        CGEventSetFlags(ev, flags);
        CGEventPost(CG_HID_EVENT_TAP, ev);
        CFRelease(ev);
    }
}

fn tap(src: *mut c_void, code: u16, flags: u64) {
    post_key(src, code, true, flags);
    post_key(src, code, false, flags);
    std::thread::sleep(Duration::from_millis(30));
}

#[repr(C)]
#[derive(Clone, Copy)]
struct NSPoint {
    x: f64,
    y: f64,
}
unsafe impl objc2::encode::Encode for NSPoint {
    const ENCODING: objc2::encode::Encoding =
        objc2::encode::Encoding::Struct("CGPoint", &[objc2::encode::Encoding::Double, objc2::encode::Encoding::Double]);
}

// NSEvent SystemDefined subtype 8: data1 = (NX_KEYTYPE << 16) | (0x0A key-down / 0x0B key-up) << 8.
fn post_media(nx_keytype: u8, down: bool) {
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};
    const NS_EVENT_TYPE_SYSTEM_DEFINED: usize = 14;
    let state: isize = if down { 0x0A } else { 0x0B };
    let data1: isize = ((nx_keytype as isize) << 16) | (state << 8);
    let flags: usize = if down { 0xA00 } else { 0xB00 };
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
            eprintln!("NSEvent otherEventWithType returned nil");
            return;
        }
        let cg: *mut c_void = msg_send![ev, CGEvent];
        CGEventPost(CG_HID_EVENT_TAP, cg);
    });
}

fn emit(delay: u64) {
    if !ax_trusted() {
        eprintln!(
            "Accessibility NOT granted to this process (a prompt should have appeared). Grant your \
             terminal app in System Settings > Privacy & Security > Accessibility, then re-run."
        );
        std::process::exit(1);
    }
    println!("Accessibility OK. Focus TextEdit (or any text field) — posting in {delay}s…");
    std::thread::sleep(Duration::from_secs(delay));

    let src = unsafe { CGEventSourceCreate(CG_EVENT_SOURCE_STATE_PRIVATE) };
    // kVK codes from src/key.rs: H=0x04 E=0x0E L=0x25 O=0x1F A=0x00 C=0x08
    // Space=0x31 Enter=0x24 LShift=0x38 LCmd=0x37 F13=0x69 Right=0x7C PageUp=0x74
    println!("1. 'hello' (plain letters)");
    for c in [0x04u16, 0x0E, 0x25, 0x25, 0x1F] {
        tap(src, c, 0);
    }
    tap(src, 0x31, 0);
    println!("2. held LSHIFT + a  => expect 'A'");
    post_key(src, 0x38, true, FLAG_SHIFT | NX_DEVICELSHIFTKEYMASK);
    tap(src, 0x00, FLAG_SHIFT | NX_DEVICELSHIFTKEYMASK);
    post_key(src, 0x38, false, 0);
    tap(src, 0x31, 0);
    println!("3. LCMD + a (select all) then Right arrow (caret to end) — text should stay intact");
    post_key(src, 0x37, true, FLAG_CMD | NX_DEVICELCMDKEYMASK);
    tap(src, 0x00, FLAG_CMD | NX_DEVICELCMDKEYMASK);
    post_key(src, 0x37, false, 0);
    tap(src, 0x7C, 0);
    println!("4. F13, PageUp, Enter (should be harmless in TextEdit)");
    tap(src, 0x69, 0);
    tap(src, 0x74, 0);
    tap(src, 0x24, 0);
    unsafe { CFRelease(src) };

    println!("5. media: VOLUME_UP (NX 0) then MEDIA_PLAY_PAUSE (NX 16) — watch the volume HUD / Music");
    post_media(0, true);
    post_media(0, false);
    std::thread::sleep(Duration::from_millis(300));
    post_media(16, true);
    post_media(16, false);
    println!("done. Expected in TextEdit: `hello A ` then a newline.");
}
}

#[cfg(target_os = "macos")]
fn main() {
    probe::main();
}
