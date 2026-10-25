//! The settings window (tray menu > Settings…): everything `mousebatt.ini`
//! holds, edited with drop-down lists, a list of games, and hotkey boxes
//! where you just press the keys. Plain Win32 controls, built with the
//! system's own look; nothing runs unless the window is open.
//!
//! Save writes `mousebatt.ini` and asks the main window to reload it. Cancel
//! just closes. While the window is open the real hotkeys are switched off,
//! so pressing one inside a hotkey box edits it instead of triggering it.
use std::cell::RefCell;
use std::collections::HashMap;
use std::mem::{size_of, zeroed};
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{BOOL, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{CreateFontW, DeleteObject, HBRUSH, HGDIOBJ};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Controls::{InitCommonControlsEx, INITCOMMONCONTROLSEX};
use windows_sys::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetFocus;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, EnumWindows, GetSystemMetrics,
    GetWindowTextLengthW, GetWindowTextW, IsDialogMessageW, IsWindowVisible, LoadCursorW,
    MessageBoxW, PostMessageW, RegisterClassW, SendMessageW, SetForegroundWindow, SetWindowPos,
    SetWindowTextW, ShowWindow, HMENU, MSG, WM_APP, WNDCLASSW,
};

use crate::config::{self, Action, Config, Fallback, Hotkey, SoundMode};
use crate::protocol::PULSAR_RATES;
use crate::watch;

/// Sent to the main window: switch the global hotkeys off / back on, or
/// re-read `mousebatt.ini` (after Save).
pub const MSG_SUSPEND_HOTKEYS: u32 = WM_APP + 5;
pub const MSG_RESUME_HOTKEYS: u32 = WM_APP + 6;
pub const MSG_RELOAD_CONFIG: u32 = WM_APP + 7;

const CLASS: &str = "mousebatt_settings_wnd";
const CLIENT_W: i32 = 540;
const CLIENT_H: i32 = 618;

// Control ids.
const ID_SAVE: i32 = 1; // IDOK: Enter
const ID_CANCEL: i32 = 2; // IDCANCEL: Esc
const ID_DESKTOP: i32 = 101;
const ID_PROGRAM: i32 = 102;
const ID_GAMERATE: i32 = 103;
const ID_ADD: i32 = 104;
const ID_LIST: i32 = 105;
const ID_REMOVE: i32 = 106;
const ID_SOUND: i32 = 107;
const ID_SOUNDAUTO: i32 = 108;
const ID_ENFORCE: i32 = 109;
const ID_GAMEDEFAULT: i32 = 110;
const ID_PAUSE: i32 = 111;
const ID_HKLIST: i32 = 120;
const ID_HKACTION: i32 = 121;
const ID_HKKEY: i32 = 122;
const ID_HKADD: i32 = 123;
const ID_HKREMOVE: i32 = 124;

// Window messages and styles (stable Win32 values).
const WM_COMMAND: u32 = 0x0111;
const WM_CLOSE: u32 = 0x0010;
const WM_DESTROY: u32 = 0x0002;
const WM_SETFONT: u32 = 0x0030;
const WM_KEYDOWN: u32 = 0x0100;
const VK_TAB: usize = 0x09;
const WS_CHILD: u32 = 0x4000_0000;
const WS_VISIBLE: u32 = 0x1000_0000;
const WS_TABSTOP: u32 = 0x0001_0000;
const WS_VSCROLL: u32 = 0x0020_0000;
const WS_BORDER: u32 = 0x0080_0000;
const WS_WINDOW: u32 = 0x00C8_0000; // WS_CAPTION | WS_SYSMENU
const BS_DEFPUSHBUTTON: u32 = 0x1;
const BS_AUTOCHECKBOX: u32 = 0x3;
const BS_GROUPBOX: u32 = 0x7;
const CBS_DROPDOWN: u32 = 0x2;
const CBS_DROPDOWNLIST: u32 = 0x3;
const LBS_NOTIFY: u32 = 0x1;
const LBS_NOINTEGRALHEIGHT: u32 = 0x100;
const CB_ADDSTRING: u32 = 0x0143;
const CB_GETCURSEL: u32 = 0x0147;
const CB_RESETCONTENT: u32 = 0x014B;
const CB_SETCURSEL: u32 = 0x014E;
const LB_ADDSTRING: u32 = 0x0180;
const LB_RESETCONTENT: u32 = 0x0184;
const LB_SETCURSEL: u32 = 0x0186;
const LB_GETCURSEL: u32 = 0x0188;
const BM_GETCHECK: u32 = 0x00F0;
const BM_SETCHECK: u32 = 0x00F1;
const HKM_SETHOTKEY: u32 = 0x0401;
const HKM_GETHOTKEY: u32 = 0x0402;
const HKM_SETRULES: u32 = 0x0403;
const HKCOMB_NONE: usize = 0x1;
const HKCOMB_S: usize = 0x2;
const HOTKEYF_SHIFT: u32 = 0x01;
const HOTKEYF_CONTROL: u32 = 0x02;
const HOTKEYF_ALT: u32 = 0x04;
const ICC_HOTKEY_CLASS: u32 = 0x40;
const SW_SHOW: i32 = 5;
const SWP_NOZORDER: u32 = 0x0004;
const SWP_NOACTIVATE: u32 = 0x0010;
const BN_CLICKED: u32 = 0;
const CBN_DROPDOWN: u32 = 7;
const LBN_SELCHANGE: u32 = 1;
const MB_ICONWARNING: u32 = 0x30;
const MB_ICONERROR: u32 = 0x10;
// Modifier bits as `config` writes them.
const MOD_ALT: u32 = 0x1;
const MOD_CONTROL: u32 = 0x2;
const MOD_SHIFT: u32 = 0x4;
const MOD_WIN: u32 = 0x8;

/// Window handles of the open window (as integers, so the struct is plain
/// data and can be copied out of the `UI` cell before any message is sent).
#[derive(Clone, Copy)]
struct Handles {
    main: usize,
    win: usize,
    font: usize,
    desktop: usize,
    gamedefault: usize,
    program: usize,
    gamerate: usize,
    list: usize,
    sound: usize,
    sound_auto: usize,
    enforce: usize,
    pause: usize,
    hk_list: usize,
    hk_action: usize,
    hk_key: usize,
}

struct Ui {
    h: Handles,
    /// The file as loaded: what the window doesn't edit (presets, delay) is
    /// written back unchanged.
    base: Config,
    /// Games in the list: (file name, rate), sorted by name.
    apps: Vec<(String, u16)>,
    /// What the hotkey drop-downs offer, in order (`None` = not used).
    actions: Vec<Option<Action>>,
    /// The hotkeys in the list, in order. The Win key can't be typed into the
    /// hotkey box, but a combination that uses it stays in the file.
    hotkeys: Vec<Hotkey>,
}

thread_local! {
    static UI: RefCell<Option<Ui>> = const { RefCell::new(None) };
    static FOUND: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

fn handles() -> Option<Handles> {
    UI.with(|u| u.borrow().as_ref().map(|u| u.h))
}

// ---------------------------------------------------------------------------
// Small Win32 helpers.

fn send(h: usize, msg: u32, w: usize, l: isize) -> isize {
    // SAFETY: `h` is a window of ours; every message used here takes plain
    // integers or a pointer that the caller keeps alive for the call.
    unsafe { SendMessageW(h as HWND, msg, w, l) }
}

fn add_str(h: usize, msg: u32, s: &str) {
    let w = crate::wide(s);
    send(h, msg, 0, w.as_ptr() as isize);
}

fn get_text(h: usize) -> String {
    let mut buf = [0u16; 260];
    // SAFETY: `buf` holds `buf.len()` u16s and `h` is a window of ours.
    let n = unsafe { GetWindowTextW(h as HWND, buf.as_mut_ptr(), buf.len() as i32) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

fn set_text(h: usize, s: &str) {
    // SAFETY: the NUL-terminated string lives for the call.
    unsafe { SetWindowTextW(h as HWND, crate::wide(s).as_ptr()) };
}

fn message(owner: usize, text: &str, style: u32) {
    // SAFETY: both strings are NUL-terminated and live for the call.
    unsafe {
        MessageBoxW(
            owner as HWND,
            crate::wide(text).as_ptr(),
            crate::wide("mousebatt settings").as_ptr(),
            style,
        )
    };
}

/// One child control, positioned in 96-DPI units scaled to `dpi`.
#[allow(clippy::too_many_arguments)]
fn child(
    parent: HWND,
    class: &str,
    text: &str,
    style: u32,
    id: i32,
    r: [i32; 4],
    dpi: i32,
    font: usize,
) -> usize {
    let sc = |v: i32| v * dpi / 96;
    // SAFETY: the strings are NUL-terminated and live for the call; `parent`
    // is our window; the id travels in the menu parameter, as for any child.
    let h = unsafe {
        CreateWindowExW(
            0,
            crate::wide(class).as_ptr(),
            crate::wide(text).as_ptr(),
            WS_CHILD | WS_VISIBLE | style,
            sc(r[0]),
            sc(r[1]),
            sc(r[2]),
            sc(r[3]),
            parent,
            id as usize as HMENU,
            GetModuleHandleW(null()),
            null(),
        )
    };
    let h = h as usize;
    if h != 0 {
        send(h, WM_SETFONT, font, 1);
    }
    h
}

fn rate_names() -> Vec<String> {
    PULSAR_RATES.iter().map(|hz| format!("{hz} Hz")).collect()
}

fn fill_combo(h: usize, items: &[String], select: usize) {
    for item in items {
        add_str(h, CB_ADDSTRING, item);
    }
    send(h, CB_SETCURSEL, select, 0);
}

/// Our modifier bits -> the hotkey box's (`HOTKEYF_*`).
fn to_box(mods: u32) -> usize {
    let mut m = 0;
    if mods & MOD_CONTROL != 0 {
        m |= HOTKEYF_CONTROL;
    }
    if mods & MOD_SHIFT != 0 {
        m |= HOTKEYF_SHIFT;
    }
    if mods & MOD_ALT != 0 {
        m |= HOTKEYF_ALT;
    }
    m as usize
}

fn from_box(m: u32) -> u32 {
    let mut o = 0;
    if m & HOTKEYF_CONTROL != 0 {
        o |= MOD_CONTROL;
    }
    if m & HOTKEYF_SHIFT != 0 {
        o |= MOD_SHIFT;
    }
    if m & HOTKEYF_ALT != 0 {
        o |= MOD_ALT;
    }
    o
}

fn action_label(a: &Option<Action>) -> String {
    match a {
        None => "(not used)".into(),
        Some(Action::Set(hz)) => format!("Set {hz} Hz"),
        Some(Action::Up) => "Step up one rate".into(),
        Some(Action::Down) => "Step down one rate".into(),
        Some(Action::Toggle(a, b)) => format!("Toggle {a} / {b} Hz"),
        Some(Action::Auto) => "Apply the automatic rate now".into(),
    }
}

/// The drop-down's entries: the usual actions, plus any other action the
/// file already uses (so nothing in it is lost).
fn action_list(base: &Config) -> Vec<Option<Action>> {
    let mut v: Vec<Option<Action>> = vec![None];
    v.extend(PULSAR_RATES.iter().map(|&hz| Some(Action::Set(hz))));
    v.push(Some(Action::Up));
    v.push(Some(Action::Down));
    for (a, b) in [(500, 1000), (1000, 2000), (500, 2000)] {
        v.push(Some(Action::Toggle(a, b)));
    }
    v.push(Some(Action::Auto));
    for h in &base.hotkeys {
        let a = Some(h.action.clone());
        if !v.contains(&a) {
            v.push(a);
        }
    }
    v
}

/// One line of the hotkey list: `Ctrl+Shift+P      Set 2000 Hz`.
fn hotkey_line(hk: &Hotkey) -> String {
    let combo = config::fmt_hotkey(hk.mods, hk.vk).unwrap_or_else(|| hk.label.clone());
    format!("{combo}      {}", action_label(&Some(hk.action.clone())))
}

fn game_label(name: &str, hz: u16) -> String {
    format!("{name}      {hz} Hz")
}

// ---------------------------------------------------------------------------
// Opening, building and closing the window.

/// Open the settings window (or bring the open one to the front).
pub fn open(main: HWND) {
    if let Some(h) = handles() {
        // SAFETY: handle of our own window.
        unsafe { SetForegroundWindow(h.win as HWND) };
        return;
    }
    // So that pressing a hotkey inside its box edits it instead of firing it.
    // SAFETY: plain handle and no payload.
    unsafe { PostMessageW(main, MSG_SUSPEND_HOTKEYS, 0, 0) };

    let class = crate::wide(CLASS);
    // SAFETY: WNDCLASSW is plain data for which all-zero is valid; `class`
    // outlives the calls that use it. A second RegisterClassW (window opened
    // again) just fails harmlessly.
    let win = unsafe {
        let hinstance = GetModuleHandleW(null());
        let mut wc: WNDCLASSW = zeroed();
        wc.lpfnWndProc = Some(settings_proc);
        wc.hInstance = hinstance;
        wc.hCursor = LoadCursorW(null_mut(), 32512 as *const u16); // IDC_ARROW
        wc.hbrBackground = 16usize as HBRUSH; // COLOR_BTNFACE + 1
        wc.lpszClassName = class.as_ptr();
        RegisterClassW(&wc);

        let icc = INITCOMMONCONTROLSEX {
            dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_HOTKEY_CLASS,
        };
        InitCommonControlsEx(&icc);

        // Created small at the primary screen's centre so that its DPI is the
        // primary monitor's; resized right below.
        let sw = GetSystemMetrics(0);
        let sh = GetSystemMetrics(1);
        CreateWindowExW(
            0,
            class.as_ptr(),
            crate::wide("mousebatt settings").as_ptr(),
            WS_WINDOW,
            sw / 2,
            sh / 2,
            100,
            100,
            null_mut(),
            null_mut(),
            hinstance,
            null(),
        )
    };
    if win.is_null() {
        // SAFETY: plain handle and no payload.
        unsafe { PostMessageW(main, MSG_RESUME_HOTKEYS, 0, 0) };
        return;
    }

    // SAFETY: `win` is the window just created.
    let dpi = unsafe { GetDpiForWindow(win) } as i32;
    let dpi = if dpi > 0 { dpi } else { 96 };
    // SAFETY: plain integer arguments (9 pt Segoe UI at this DPI).
    let font = unsafe {
        CreateFontW(
            -(9 * dpi / 72),
            0,
            0,
            0,
            400,
            0,
            0,
            0,
            1,
            0,
            0,
            5,
            0,
            crate::wide("Segoe UI").as_ptr(),
        )
    } as usize;

    let ui = build(win, main as usize, dpi, font, config::load());
    UI.with(|u| *u.borrow_mut() = Some(ui));
    populate();

    // Outer size from the wanted client size, centred on the primary screen.
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: CLIENT_W * dpi / 96,
        bottom: CLIENT_H * dpi / 96,
    };
    // SAFETY: `rect` is a valid in/out pointer; `win` is our window.
    unsafe {
        AdjustWindowRectExForDpi(&mut rect, WS_WINDOW, 0, 0, dpi as u32);
        let (w, h) = (rect.right - rect.left, rect.bottom - rect.top);
        let (x, y) = ((GetSystemMetrics(0) - w) / 2, (GetSystemMetrics(1) - h) / 2);
        SetWindowPos(
            win,
            null_mut(),
            x.max(0),
            y.max(0),
            w,
            h,
            SWP_NOZORDER | SWP_NOACTIVATE,
        );
        ShowWindow(win, SW_SHOW);
        SetForegroundWindow(win);
    }
}

fn build(win: HWND, main: usize, dpi: i32, font: usize, base: Config) -> Ui {
    let c = |class: &str, text: &str, style: u32, id: i32, r: [i32; 4]| {
        child(win, class, text, style, id, r, dpi, font)
    };
    let combo_style = CBS_DROPDOWNLIST | WS_VSCROLL | WS_TABSTOP;

    c("BUTTON", "Rates", BS_GROUPBOX, 0, [10, 8, 520, 100]);
    c("STATIC", "Desktop (no game running):", 0, 0, [24, 32, 200, 18]);
    let desktop = c("COMBOBOX", "", combo_style, ID_DESKTOP, [232, 28, 170, 200]);
    c("STATIC", "Other games (detected):", 0, 0, [24, 62, 200, 18]);
    let gamedefault = c("COMBOBOX", "", combo_style, ID_GAMEDEFAULT, [232, 58, 170, 200]);
    c(
        "STATIC",
        "Recognised by install folder: Steam, Epic, GOG, Xbox, Riot, Ubisoft, EA.",
        0,
        0,
        [24, 84, 494, 18],
    );

    c(
        "BUTTON",
        "Games with their own rate (they win over the above)",
        BS_GROUPBOX,
        0,
        [10, 116, 520, 170],
    );
    c("STATIC", "Program:", 0, 0, [24, 142, 56, 18]);
    let program = c(
        "COMBOBOX",
        "",
        CBS_DROPDOWN | WS_VSCROLL | WS_TABSTOP,
        ID_PROGRAM,
        [82, 138, 196, 220],
    );
    c("STATIC", "Rate:", 0, 0, [288, 142, 32, 18]);
    let gamerate = c("COMBOBOX", "", combo_style, ID_GAMERATE, [322, 138, 86, 200]);
    c("BUTTON", "+ Add / update", WS_TABSTOP, ID_ADD, [414, 137, 104, 24]);
    let list_style = WS_BORDER | LBS_NOTIFY | WS_VSCROLL | LBS_NOINTEGRALHEIGHT | WS_TABSTOP;
    let list = c("LISTBOX", "", list_style, ID_LIST, [24, 170, 494, 70]);
    c("BUTTON", "\u{2212} Remove", WS_TABSTOP, ID_REMOVE, [24, 248, 100, 24]);
    c(
        "STATIC",
        "Pick a program that is running, or type its file name (Task Manager > Details).",
        0,
        0,
        [134, 246, 384, 32],
    );

    c(
        "BUTTON",
        "Hotkeys (as many as you like)",
        BS_GROUPBOX,
        0,
        [10, 294, 520, 162],
    );
    let hk_list = c("LISTBOX", "", list_style, ID_HKLIST, [24, 320, 494, 64]);
    let hk_action = c("COMBOBOX", "", combo_style, ID_HKACTION, [24, 392, 236, 200]);
    let hk_key = c(
        "msctls_hotkey32",
        "",
        WS_BORDER | WS_TABSTOP,
        ID_HKKEY,
        [268, 393, 146, 22],
    );
    c("BUTTON", "+ Add / update", WS_TABSTOP, ID_HKADD, [420, 391, 98, 24]);
    c("BUTTON", "\u{2212} Remove", WS_TABSTOP, ID_HKREMOVE, [24, 424, 100, 24]);
    c(
        "STATIC",
        "Pick an action, click the box, press the keys, then + Add. Select a hotkey to change it.",
        0,
        0,
        [134, 422, 384, 32],
    );

    c("BUTTON", "Options", BS_GROUPBOX, 0, [10, 464, 520, 106]);
    c("STATIC", "Sound:", 0, 0, [24, 490, 50, 18]);
    let sound = c("COMBOBOX", "", combo_style, ID_SOUND, [78, 486, 150, 200]);
    let sound_auto = c(
        "BUTTON",
        "Also for automatic changes",
        BS_AUTOCHECKBOX | WS_TABSTOP,
        ID_SOUNDAUTO,
        [244, 488, 274, 20],
    );
    let enforce = c(
        "BUTTON",
        "Put the rate back if another tool changes it",
        BS_AUTOCHECKBOX | WS_TABSTOP,
        ID_ENFORCE,
        [24, 516, 494, 20],
    );
    let pause = c(
        "BUTTON",
        "Don't read the mouse while a game runs (battery display waits)",
        BS_AUTOCHECKBOX | WS_TABSTOP,
        ID_PAUSE,
        [24, 542, 494, 20],
    );
    c(
        "BUTTON",
        "Save",
        BS_DEFPUSHBUTTON | WS_TABSTOP,
        ID_SAVE,
        [330, 580, 100, 28],
    );
    c("BUTTON", "Cancel", WS_TABSTOP, ID_CANCEL, [440, 580, 90, 28]);

    let mut apps: Vec<(String, u16)> = base.apps.iter().map(|(k, &v)| (k.clone(), v)).collect();
    apps.sort();
    let actions = action_list(&base);
    let hotkeys = base.hotkeys.clone();

    Ui {
        h: Handles {
            main,
            win: win as usize,
            font,
            desktop,
            gamedefault,
            program,
            gamerate,
            list,
            sound,
            sound_auto,
            enforce,
            pause,
            hk_list,
            hk_action,
            hk_key,
        },
        base,
        apps,
        actions,
        hotkeys,
    }
}

/// Put the loaded settings into the controls.
fn populate() {
    let Some((h, base, apps, actions, hotkeys)) = UI.with(|u| {
        u.borrow().as_ref().map(|u| {
            (
                u.h,
                u.base.clone(),
                u.apps.clone(),
                u.actions.clone(),
                u.hotkeys.clone(),
            )
        })
    }) else {
        return;
    };

    let mut desktop_items = vec!["Leave as is".to_string()];
    desktop_items.extend(rate_names());
    let desktop_sel = match base.default {
        Fallback::Keep => 0,
        Fallback::Rate(hz) => PULSAR_RATES.iter().position(|&r| r == hz).map_or(0, |i| i + 1),
    };
    fill_combo(h.desktop, &desktop_items, desktop_sel);

    let mut game_items = vec!["Off (don't detect games)".to_string()];
    game_items.extend(rate_names());
    let game_sel = base
        .game_rate
        .and_then(|hz| PULSAR_RATES.iter().position(|&r| r == hz))
        .map_or(0, |i| i + 1);
    fill_combo(h.gamedefault, &game_items, game_sel);

    let fast = PULSAR_RATES.iter().position(|&r| r == 2000).unwrap_or(0);
    fill_combo(h.gamerate, &rate_names(), fast);
    refresh_programs(&h);
    refill_list(&h, &apps);

    let labels: Vec<String> = actions.iter().map(action_label).collect();
    fill_combo(h.hk_action, &labels, 0);
    // Require a modifier; a bare key gets Ctrl+Alt in front.
    send(
        h.hk_key,
        HKM_SETRULES,
        HKCOMB_NONE | HKCOMB_S,
        (HOTKEYF_CONTROL | HOTKEYF_ALT) as isize,
    );
    refill_hotkeys(&h, &hotkeys);

    let sounds = ["Off", "Soft beep", "Spoken number"].map(String::from);
    let sound_sel = match base.sound {
        SoundMode::Off => 0,
        SoundMode::Beep => 1,
        SoundMode::Voice => 2,
    };
    fill_combo(h.sound, &sounds, sound_sel);
    send(h.sound_auto, BM_SETCHECK, usize::from(base.sound_auto), 0);
    send(h.enforce, BM_SETCHECK, usize::from(base.enforce), 0);
    send(h.pause, BM_SETCHECK, usize::from(base.pause_in_game), 0);
}

fn refill_hotkeys(h: &Handles, hotkeys: &[Hotkey]) {
    send(h.hk_list, LB_RESETCONTENT, 0, 0);
    for hk in hotkeys {
        add_str(h.hk_list, LB_ADDSTRING, &hotkey_line(hk));
    }
    send(h.hk_list, LB_SETCURSEL, usize::MAX, 0); // nothing selected
}

fn refill_list(h: &Handles, apps: &[(String, u16)]) {
    send(h.list, LB_RESETCONTENT, 0, 0);
    for (name, hz) in apps {
        add_str(h.list, LB_ADDSTRING, &game_label(name, *hz));
    }
    send(h.list, LB_SETCURSEL, usize::MAX, 0); // nothing selected
}

/// Close the window; `reload` = after a save (re-read the file) rather than a
/// cancel (just hand the hotkeys back).
fn close(reload: bool) {
    let Some(h) = handles() else { return };
    // SAFETY: plain handle and no payload.
    unsafe {
        PostMessageW(
            h.main as HWND,
            if reload {
                MSG_RELOAD_CONFIG
            } else {
                MSG_RESUME_HOTKEYS
            },
            0,
            0,
        );
        DestroyWindow(h.win as HWND);
    }
}

fn cleanup() {
    if let Some(h) = handles() {
        // SAFETY: `font` was created by `open` and no control uses it any more
        // (the children are destroyed with the window).
        unsafe { DeleteObject(h.font as HGDIOBJ) };
    }
    UI.with(|u| *u.borrow_mut() = None);
}

// ---------------------------------------------------------------------------
// Events.

unsafe extern "system" fn settings_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_COMMAND => {
            on_command((wparam & 0xFFFF) as i32, ((wparam >> 16) & 0xFFFF) as u32);
            0
        }
        WM_CLOSE => {
            close(false);
            0
        }
        WM_DESTROY => {
            cleanup();
            0
        }
        // SAFETY: forwarding the unchanged arguments of this very call.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn on_command(id: i32, code: u32) {
    match (id, code) {
        (ID_SAVE, BN_CLICKED) => save(),
        (ID_CANCEL, BN_CLICKED) => close(false),
        (ID_ADD, BN_CLICKED) => on_add(),
        (ID_REMOVE, BN_CLICKED) => on_remove(),
        (ID_LIST, LBN_SELCHANGE) => on_list_select(),
        (ID_HKADD, BN_CLICKED) => on_hotkey_add(),
        (ID_HKREMOVE, BN_CLICKED) => on_hotkey_remove(),
        (ID_HKLIST, LBN_SELCHANGE) => on_hotkey_select(),
        (ID_PROGRAM, CBN_DROPDOWN) => {
            if let Some(h) = handles() {
                refresh_programs(&h);
            }
        }
        _ => {}
    }
}

/// "Add / update": the program typed or picked, with the rate chosen next to it.
fn on_add() {
    let Some(h) = handles() else { return };
    let typed = get_text(h.program);
    let file = typed.trim().rsplit(['\\', '/']).next().unwrap_or("").trim();
    if file.is_empty() {
        return;
    }
    let name = config::normalize_exe(file);
    let idx = usize::try_from(send(h.gamerate, CB_GETCURSEL, 0, 0)).ok();
    let Some(&hz) = idx.and_then(|i| PULSAR_RATES.get(i)) else {
        return;
    };
    let apps = UI.with(|u| {
        let mut u = u.borrow_mut();
        let ui = u.as_mut()?;
        match ui.apps.iter_mut().find(|(n, _)| *n == name) {
            Some(entry) => entry.1 = hz,
            None => {
                ui.apps.push((name.clone(), hz));
                ui.apps.sort();
            }
        }
        Some(ui.apps.clone())
    });
    if let Some(apps) = apps {
        refill_list(&h, &apps);
    }
}

fn on_remove() {
    let Some(h) = handles() else { return };
    let Ok(sel) = usize::try_from(send(h.list, LB_GETCURSEL, 0, 0)) else {
        return;
    };
    let apps = UI.with(|u| {
        let mut u = u.borrow_mut();
        let ui = u.as_mut()?;
        if sel < ui.apps.len() {
            ui.apps.remove(sel);
        }
        Some(ui.apps.clone())
    });
    if let Some(apps) = apps {
        refill_list(&h, &apps);
    }
}

/// A game picked in the list: show it in the program and rate boxes, ready
/// to be changed with "Add / update".
fn on_list_select() {
    let Some(h) = handles() else { return };
    let Ok(sel) = usize::try_from(send(h.list, LB_GETCURSEL, 0, 0)) else {
        return;
    };
    let entry = UI.with(|u| u.borrow().as_ref().and_then(|u| u.apps.get(sel).cloned()));
    if let Some((name, hz)) = entry {
        set_text(h.program, &name);
        if let Some(pos) = PULSAR_RATES.iter().position(|&r| r == hz) {
            send(h.gamerate, CB_SETCURSEL, pos, 0);
        }
    }
}

/// "+ Add / update" for hotkeys: the action and keys chosen above the button.
/// A hotkey with the same keys is replaced.
fn on_hotkey_add() {
    let Some(h) = handles() else { return };
    let action = UI.with(|u| {
        let u = u.borrow();
        let ui = u.as_ref()?;
        usize::try_from(send(h.hk_action, CB_GETCURSEL, 0, 0))
            .ok()
            .and_then(|i| ui.actions.get(i))
            .cloned()
            .flatten()
    });
    let Some(action) = action else {
        message(h.win, "Choose what the hotkey should do.", MB_ICONWARNING);
        return;
    };
    let keys = send(h.hk_key, HKM_GETHOTKEY, 0, 0) as u32;
    let (vk, mods) = (keys & 0xFF, from_box((keys >> 8) & 0xFF));
    if vk == 0 {
        message(h.win, "Click the key box and press the keys.", MB_ICONWARNING);
        return;
    }
    let Some(label) = config::fmt_hotkey(mods, vk) else {
        message(h.win, "That key is not supported.", MB_ICONWARNING);
        return;
    };
    let hotkeys = UI.with(|u| {
        let mut u = u.borrow_mut();
        let ui = u.as_mut()?;
        let hk = Hotkey {
            mods,
            vk,
            action,
            label,
        };
        match ui.hotkeys.iter_mut().find(|e| e.mods == mods && e.vk == vk) {
            Some(existing) => *existing = hk,
            None => ui.hotkeys.push(hk),
        }
        Some(ui.hotkeys.clone())
    });
    if let Some(hotkeys) = hotkeys {
        refill_hotkeys(&h, &hotkeys);
    }
}

fn on_hotkey_remove() {
    let Some(h) = handles() else { return };
    let Ok(sel) = usize::try_from(send(h.hk_list, LB_GETCURSEL, 0, 0)) else {
        return;
    };
    let hotkeys = UI.with(|u| {
        let mut u = u.borrow_mut();
        let ui = u.as_mut()?;
        if sel < ui.hotkeys.len() {
            ui.hotkeys.remove(sel);
        }
        Some(ui.hotkeys.clone())
    });
    if let Some(hotkeys) = hotkeys {
        refill_hotkeys(&h, &hotkeys);
    }
}

/// A hotkey picked in the list: show its action and keys above, ready to be
/// changed with "+ Add / update".
fn on_hotkey_select() {
    let Some(h) = handles() else { return };
    let Ok(sel) = usize::try_from(send(h.hk_list, LB_GETCURSEL, 0, 0)) else {
        return;
    };
    let picked = UI.with(|u| {
        let u = u.borrow();
        let ui = u.as_ref()?;
        let hk = ui.hotkeys.get(sel)?.clone();
        let pos = ui.actions.iter().position(|a| a.as_ref() == Some(&hk.action));
        Some((hk, pos))
    });
    if let Some((hk, pos)) = picked {
        send(h.hk_action, CB_SETCURSEL, pos.unwrap_or(0), 0);
        // The box can't show a combination that uses the Win key.
        let keys = if hk.mods & MOD_WIN == 0 {
            (to_box(hk.mods) << 8) | hk.vk as usize
        } else {
            0
        };
        send(h.hk_key, HKM_SETHOTKEY, keys, 0);
    }
}

unsafe extern "system" fn enum_windows(hwnd: HWND, _lparam: LPARAM) -> BOOL {
    // SAFETY: plain window-handle queries.
    let titled = unsafe { IsWindowVisible(hwnd) != 0 && GetWindowTextLengthW(hwnd) > 0 };
    if titled {
        if let Some((_, name, _)) = watch::process_of_window(hwnd) {
            FOUND.with(|f| f.borrow_mut().push(name));
        }
    }
    1 // keep enumerating
}

/// Programs that have a visible window right now, one entry each.
fn running_programs() -> Vec<String> {
    FOUND.with(|f| f.borrow_mut().clear());
    // SAFETY: `enum_windows` is a plain function; the parameter is unused.
    unsafe { EnumWindows(Some(enum_windows), 0) };
    let mut names = FOUND.with(|f| std::mem::take(&mut *f.borrow_mut()));
    names.sort();
    names.dedup();
    let me = std::env::current_exe()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_lowercase()));
    names.retain(|n| Some(n) != me.as_ref());
    names
}

/// Fill the program drop-down with what is running now (a game launched after
/// the window opened shows up when the list is opened again). The text typed
/// in the box is kept.
fn refresh_programs(h: &Handles) {
    let typed = get_text(h.program);
    send(h.program, CB_RESETCONTENT, 0, 0);
    for name in running_programs() {
        add_str(h.program, CB_ADDSTRING, &name);
    }
    set_text(h.program, &typed);
}

/// Read every control into a `Config`, or say what is wrong.
fn collect(ui: &Ui) -> Result<Config, String> {
    let h = &ui.h;
    let mut cfg = ui.base.clone();

    let d = send(h.desktop, CB_GETCURSEL, 0, 0);
    cfg.default = match usize::try_from(d) {
        Ok(i) if i > 0 => match PULSAR_RATES.get(i - 1) {
            Some(&hz) => Fallback::Rate(hz),
            None => Fallback::Keep,
        },
        _ => Fallback::Keep,
    };
    cfg.game_rate = usize::try_from(send(h.gamedefault, CB_GETCURSEL, 0, 0))
        .ok()
        .filter(|&i| i > 0)
        .and_then(|i| PULSAR_RATES.get(i - 1).copied());
    cfg.apps = ui.apps.iter().cloned().collect::<HashMap<_, _>>();
    cfg.sound = match send(h.sound, CB_GETCURSEL, 0, 0) {
        1 => SoundMode::Beep,
        2 => SoundMode::Voice,
        _ => SoundMode::Off,
    };
    cfg.sound_auto = send(h.sound_auto, BM_GETCHECK, 0, 0) == 1;
    cfg.enforce = send(h.enforce, BM_GETCHECK, 0, 0) == 1;
    cfg.pause_in_game = send(h.pause, BM_GETCHECK, 0, 0) == 1;

    cfg.hotkeys = ui.hotkeys.clone();
    Ok(cfg)
}

fn save() {
    let Some(win) = handles().map(|h| h.win) else {
        return;
    };
    let collected = UI.with(|u| u.borrow().as_ref().map(collect));
    match collected {
        Some(Ok(cfg)) => match config::save(&cfg) {
            Ok(()) => close(true),
            Err(e) => message(
                win,
                &format!("mousebatt.ini could not be written: {e}"),
                MB_ICONERROR,
            ),
        },
        Some(Err(why)) => message(win, &why, MB_ICONWARNING),
        None => {}
    }
}

/// Let the open window handle Tab / Enter / Esc. Keys pressed in a hotkey
/// box are left alone (Enter or Esc may be part of the combination), except
/// Tab, which still moves on.
pub fn dialog_message(msg: &MSG) -> bool {
    let Some((win, key)) = UI.with(|u| u.borrow().as_ref().map(|u| (u.h.win, u.h.hk_key))) else {
        return false;
    };
    // SAFETY: no arguments.
    let focus = unsafe { GetFocus() } as usize;
    if focus == key && !(msg.message == WM_KEYDOWN && msg.wParam == VK_TAB) {
        return false;
    }
    // SAFETY: `win` is our live window and `msg` a valid MSG.
    unsafe { IsDialogMessageW(win as HWND, msg) != 0 }
}
