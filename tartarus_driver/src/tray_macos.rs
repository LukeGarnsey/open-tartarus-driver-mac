// Menu-bar icon for `tray` mode on macOS — the counterpart of tray.rs
// (Windows). Same menu, same semantics: "設定を開く" opens the configui web
// page (started by run_tray_mode before this), a disabled version line,
// "アップデートを確認" opens the Releases page, "終了" sets SHUTDOWN_REQUESTED
// so the driver loop exits through its normal stuck-key cleanup.
//
// Threading is the reverse of Windows: AppKit insists that NSStatusItem and
// its menu live on the MAIN thread inside a running NSApplication event
// loop, so here the tray owns the main thread (`run_menu_bar` never
// returns) and run_driver runs on a worker thread spawned by
// run_tray_mode, which calls std::process::exit once the driver loop has
// finished its cleanup. That is the only way out of `[NSApp run]`, and it
// covers both "終了" and Ctrl+C/SIGTERM (ctrlc sets the same flag).
//
// `tray-icon` (tauri's crate, with its `muda` menu) does the NSStatusItem
// work; NSApplication itself is driven through two raw `msg_send!`s so we
// don't need objc2-app-kit as a direct dependency. The icon is drawn in
// code (a keypad glyph) and marked as a template image, so macOS recolours
// it for light/dark menu bars — no image assets to ship or decode.

use crate::{eprintln, println, CONFIGUI_URL, RELEASES_URL, SHUTDOWN_REQUESTED, VERSION};
use objc2::runtime::AnyObject;
use objc2::{class, msg_send};
use std::sync::atomic::Ordering;
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIconBuilder};

const ID_OPEN_SETTINGS: &str = "open-settings";
const ID_CHECK_UPDATES: &str = "check-updates";
const ID_QUIT: &str = "quit";

// NSApplicationActivationPolicyAccessory: menu-bar presence only, no Dock
// icon, no app menu — the same footprint as a Windows tray-only process.
const NS_APPLICATION_ACTIVATION_POLICY_ACCESSORY: isize = 1;

// A 32x32 keypad glyph: a rounded outline with a 3x2 grid of key caps.
// Only the alpha channel matters for a template image.
fn keypad_icon() -> Icon {
    const W: u32 = 32;
    const H: u32 = 32;
    let mut rgba = vec![0u8; (W * H * 4) as usize];
    let mut set = |x: u32, y: u32| {
        let i = ((y * W + x) * 4) as usize;
        rgba[i..i + 4].copy_from_slice(&[0, 0, 0, 255]);
    };
    // outline (2px), corners left open for a rounded look
    for x in 2..30 {
        for y in [2u32, 3, 28, 29] {
            if (4..28).contains(&x) || (5..27).contains(&y) {
                set(x, y);
            }
        }
    }
    for y in 4..28 {
        for x in [2u32, 3, 28, 29] {
            set(x, y);
        }
    }
    // key caps: 3 columns x 2 rows of 5x5 squares
    for row in 0..2u32 {
        for col in 0..3u32 {
            let x0 = 7 + col * 7;
            let y0 = 8 + row * 9;
            for x in x0..x0 + 5 {
                for y in y0..y0 + 5 {
                    set(x, y);
                }
            }
        }
    }
    Icon::from_rgba(rgba, W, H).expect("static icon dimensions are valid")
}

fn on_menu_event(event: MenuEvent) {
    match event.id.0.as_str() {
        ID_OPEN_SETTINGS => crate::platform::open_url(CONFIGUI_URL),
        ID_CHECK_UPDATES => crate::platform::open_url(RELEASES_URL),
        ID_QUIT => {
            println!("[tray] \"終了\"が選択されました。シャットダウンします。");
            SHUTDOWN_REQUESTED.store(true, Ordering::SeqCst);
        }
        _ => {}
    }
}

// Must be called on the main thread. Never returns: the process ends when
// the driver thread, having noticed SHUTDOWN_REQUESTED and released every
// held key, calls std::process::exit (see run_tray_mode in main.rs).
pub fn run_menu_bar() -> ! {
    let app: *mut AnyObject = unsafe { msg_send![class!(NSApplication), sharedApplication] };
    let _: bool = unsafe { msg_send![app, setActivationPolicy: NS_APPLICATION_ACTIVATION_POLICY_ACCESSORY] };

    let menu = Menu::new();
    let open = MenuItem::with_id(ID_OPEN_SETTINGS, "設定を開く (configui)", true, None);
    let version = MenuItem::new(format!("バージョン: v{VERSION}"), false, None);
    let updates = MenuItem::with_id(ID_CHECK_UPDATES, "アップデートを確認 (GitHub)", true, None);
    let quit = MenuItem::with_id(ID_QUIT, "終了", true, None);
    if let Err(e) = menu.append_items(&[&open, &version, &updates, &PredefinedMenuItem::separator(), &quit]) {
        eprintln!("WARNING: could not build the menu-bar menu: {e}");
    }
    MenuEvent::set_event_handler(Some(on_menu_event));

    // Kept alive for the life of the process by the `[NSApp run]` below.
    let _tray = match TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("Tartarus Driver")
        .with_icon(keypad_icon())
        .with_icon_as_template(true)
        .build()
    {
        Ok(t) => Some(t),
        Err(e) => {
            eprintln!(
                "WARNING: menu-bar icon could not be created ({e}) — the driver keeps running; \
                 stop it with Ctrl+C / `kill`, settings page at {CONFIGUI_URL}"
            );
            None
        }
    };
    println!("[tray] menu-bar icon ready — settings page at {CONFIGUI_URL}");

    let _: () = unsafe { msg_send![app, run] };
    // `run` only returns if something called [NSApp stop:], which nothing
    // here does; treat it as a shutdown request all the same.
    SHUTDOWN_REQUESTED.store(true, Ordering::SeqCst);
    loop {
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}
