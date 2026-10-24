//! mousebatt — tray battery monitor for Pulsar / VAXEE wireless mice.
//! Zero-CPU design: blocked in GetMessage, woken by a coarse poll timer,
//! device-change broadcasts, resume-from-sleep, and — only when
//! `mousebatt.ini` asks for them — focus-change events and hotkeys.
#![windows_subsystem = "windows"]
// False positive in clippy 1.98: the thread_local! initializers below are already `const`.
#![allow(clippy::missing_const_for_thread_local)]
#![warn(unsafe_op_in_unsafe_fn, clippy::undocumented_unsafe_blocks)]

mod config;
mod hid;
mod icon;
mod protocol;
mod sound;
mod watch;

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::mem::{size_of, zeroed};
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU32, AtomicUsize, Ordering};
use std::time::Duration;

use config::{Action, Config, SoundMode};
use icon::{BatteryGlyph, Hicon};
use protocol::{
    read_battery, set_polling, BatteryStatus, PollingInfo, ReadResult, PULSAR_RATES,
};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Registry::{
    RegCloseKey, RegDeleteValueW, RegGetValueW, RegOpenKeyExW, RegQueryValueExW, RegSetKeyValueW,
    RegSetValueExW, HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_DWORD, REG_SZ,
    RRF_RT_REG_DWORD,
};
use windows_sys::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows_sys::Win32::UI::Accessibility::HWINEVENTHOOK;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, UnregisterHotKey};
use windows_sys::Win32::UI::Shell::{
    ShellExecuteW, Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE,
    NIM_MODIFY, NOTIFYICONDATAW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DispatchMessageW,
    GetCursorPos, GetForegroundWindow, GetMessageW, KillTimer, MessageBoxW, PostMessageW,
    PostQuitMessage, RegisterClassW, RegisterWindowMessageW, SetForegroundWindow, SetTimer,
    TrackPopupMenu, TranslateMessage,
    CW_USEDEFAULT, MF_CHECKED, MF_POPUP, MF_SEPARATOR, MF_STRING, MSG, TPM_NONOTIFY, TPM_RETURNCMD,
    TPM_RIGHTBUTTON, WM_APP, WM_CREATE, WM_DESTROY, WM_LBUTTONUP, WM_RBUTTONUP, WM_TIMER,
    WNDCLASSW, WS_OVERLAPPED,
};

const POLL_INTERVAL_MS: u32 = 4 * 60 * 1000; // user preference: every 3-5 minutes
const DEBOUNCE_MS: u32 = 2500; // one replug fires many WM_DEVICECHANGE broadcasts

const WMAPP_TRAY: u32 = WM_APP + 1;
const WMAPP_POLLDONE: u32 = WM_APP + 2;
const WMAPP_REDRAW: u32 = WM_APP + 3;
const WMAPP_WARN: u32 = WM_APP + 4;
const TIMER_POLL: usize = 1;
const TIMER_DEBOUNCE: usize = 2;
const TIMER_THEME: usize = 3;
/// One-shot: fires `delay_ms` after the focus settled on a program whose rule
/// differs from the rate currently wanted.
const TIMER_AUTO: usize = 4;
/// One-shot: another try at a rate the mouse didn't confirm (asleep, lost write).
const TIMER_RETRY: usize = 5;
const MENU_REFRESH: usize = 1;
const MENU_AUTOSTART: usize = 2;
const MENU_EXIT: usize = 3;
const MENU_AUTO: usize = 4;
const MENU_EDIT_CFG: usize = 5;
const MENU_RELOAD_CFG: usize = 6;
/// Polling-rate items are `MENU_RATE_BASE + index into PollingInfo::rates`.
const MENU_RATE_BASE: usize = 100;
/// Battery-icon items are `MENU_GLYPH_BASE + index into BatteryGlyph::ALL`.
const MENU_GLYPH_BASE: usize = 200;

// Not re-exported cleanly by windows-sys; values are stable Win32 ABI.
const WM_DEVICECHANGE: u32 = 0x0219;
const DBT_DEVNODES_CHANGED: usize = 0x0007;
const WM_POWERBROADCAST: u32 = 0x0218;
const PBT_APMRESUMEAUTOMATIC: usize = 0x0012;
const WM_SETTINGCHANGE: u32 = 0x001A;
const WM_DPICHANGED: u32 = 0x02E0;
const WM_HOTKEY: u32 = 0x0312;
const MOD_NOREPEAT: u32 = 0x4000;
const MB_OK: u32 = 0x0;
const MB_ICONWARNING: u32 = 0x30;
const SW_SHOWNORMAL: i32 = 1;

/// Flags the poll worker sends along with its result (`WPARAM`).
/// A rate was written (or found already in place) and read back.
const POLL_TRIED: usize = 1;
/// ... and the mouse really reports the wanted rate.
const POLL_VERIFIED: usize = 2;
/// ... and the user asked for a sound once it is confirmed.
const POLL_ANNOUNCE: usize = 4;
/// Waits before the successive retries of an unconfirmed rate.
const RETRY_DELAYS_MS: [u32; 3] = [3_000, 10_000, 30_000];

static POLLING: AtomicBool = AtomicBool::new(false);
static TASKBAR_CREATED_MSG: AtomicU32 = AtomicU32::new(0);
/// Polling rate the user picked from the menu, applied by the next poll
/// thread before it reads the battery (0 = nothing pending).
static REQUESTED_HZ: AtomicU16 = AtomicU16::new(0);
/// Play a sound when the pending `REQUESTED_HZ` is confirmed.
static ANNOUNCE: AtomicBool = AtomicBool::new(false);
/// The main window, for timers set from the focus callback.
static MAIN_HWND: AtomicUsize = AtomicUsize::new(0);

// Main-thread UI state.
thread_local! {
    static LAST_GOOD: RefCell<Option<BatteryStatus>> = const { RefCell::new(None) };
    static CUR_ICON: RefCell<Option<Hicon>> = const { RefCell::new(None) };
    /// What the icon last showed, to redraw it when a display setting changes.
    static LAST_VIEW: RefCell<Option<TrayView>> = const { RefCell::new(None) };
    /// The user's battery-icon choice; loaded from the registry in `main`.
    static GLYPH: Cell<BatteryGlyph> = const { Cell::new(DEFAULT_GLYPH) };
}

// Rules, hotkeys and what we last asked of the mouse. Main thread only.
thread_local! {
    static CFG: RefCell<Config> = RefCell::new(Config::default());
    /// true = the per-program rules drive the rate; false = a hotkey or menu
    /// pick took manual control.
    static AUTO: Cell<bool> = const { Cell::new(true) };
    /// The rate we want the mouse to have (0 = nothing asked yet). Re-applied
    /// by the periodic check when `enforce` is on.
    static WANTED: Cell<u16> = const { Cell::new(0) };
    /// Rate of the rule that is waiting for `TIMER_AUTO`.
    static PENDING_AUTO: Cell<u16> = const { Cell::new(0) };
    static RETRY_STEP: Cell<u8> = const { Cell::new(0) };
    /// Focus-hook handle (0 = not installed).
    static HOOK: Cell<usize> = const { Cell::new(0) };
    static HOTKEY_COUNT: Cell<usize> = const { Cell::new(0) };
    static WARNINGS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn main() {
    // DPI aware, so the icon is drawn at the tray's real pixel size rather than
    // at 96 DPI and blurred by Windows' upscaling on scaled displays.
    // SAFETY: no preconditions; called before any window exists.
    unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    GLYPH.with(|g| g.set(load_glyph()));
    let class_name = wide("mousebatt_tray_wnd");
    // SAFETY: WNDCLASSW is plain data for which all-zero is valid; `class_name`
    // outlives the window (it lives until `main` returns), and the other
    // string temporaries live for the full statement that uses them.
    let hwnd = unsafe {
        let hinstance = GetModuleHandleW(null());
        let mut wc: WNDCLASSW = zeroed();
        wc.lpfnWndProc = Some(wndproc);
        wc.hInstance = hinstance;
        wc.lpszClassName = class_name.as_ptr();
        RegisterClassW(&wc);

        TASKBAR_CREATED_MSG.store(
            RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()),
            Ordering::Relaxed,
        );

        // Hidden top-level window (message-only windows miss WM_DEVICECHANGE).
        CreateWindowExW(
            0,
            class_name.as_ptr(),
            wide("mousebatt").as_ptr(),
            WS_OVERLAPPED,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            0,
            0,
            null_mut(),
            null_mut(),
            hinstance,
            null(),
        )
    };
    if hwnd.is_null() {
        return;
    }

    // SAFETY: MSG is plain data for which all-zero is valid and `msg` outlives
    // every call that borrows it.
    unsafe {
        let mut msg: MSG = zeroed();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// Window procedure. Runs only on the main thread, invoked by DispatchMessageW.
unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_CREATE => {
            MAIN_HWND.store(hwnd as usize, Ordering::Relaxed);
            update_tray(hwnd, "…", icon::COLOR_STALE, "mousebatt — reading…", true);
            // SAFETY: plain handle + id arguments; no callback pointer.
            unsafe { SetTimer(hwnd, TIMER_POLL, POLL_INTERVAL_MS, None) };
            reload_config(hwnd);
            start_poll(hwnd);
            0
        }
        WM_TIMER => {
            if wparam == TIMER_POLL {
                // Periodic heartbeat. This is what re-discovers a mouse that
                // slept (and woke) while the system stayed up: no
                // WM_DEVICECHANGE or power broadcast is sent for that, so
                // without this the tray would sit stale until a manual
                // refresh.
                start_poll(hwnd);
            } else if wparam == TIMER_DEBOUNCE {
                // SAFETY: plain handle + id arguments.
                unsafe { KillTimer(hwnd, TIMER_DEBOUNCE) };
                start_poll(hwnd);
            } else if wparam == TIMER_THEME {
                // SAFETY: plain handle + id arguments.
                unsafe { KillTimer(hwnd, TIMER_THEME) };
                redraw_worker(hwnd);
            } else if wparam == TIMER_AUTO {
                // SAFETY: plain handle + id arguments.
                unsafe { KillTimer(hwnd, TIMER_AUTO) };
                let hz = PENDING_AUTO.with(Cell::get);
                if AUTO.with(Cell::get) && hz != 0 && hz != WANTED.with(Cell::get) {
                    let announce = CFG.with(|c| c.borrow().sound_auto);
                    request_rate(hwnd, hz, announce);
                }
            } else if wparam == TIMER_RETRY {
                // SAFETY: plain handle + id arguments.
                unsafe { KillTimer(hwnd, TIMER_RETRY) };
                queue_apply(hwnd);
            }
            0
        }
        WM_HOTKEY => {
            on_hotkey(hwnd, wparam);
            0
        }
        WMAPP_WARN => {
            show_warnings();
            0
        }
        WM_DEVICECHANGE => {
            if wparam == DBT_DEVNODES_CHANGED {
                // SAFETY: plain handle + id arguments; no callback pointer.
                unsafe { SetTimer(hwnd, TIMER_DEBOUNCE, DEBOUNCE_MS, None) };
            }
            0
        }
        WM_POWERBROADCAST => {
            if wparam == PBT_APMRESUMEAUTOMATIC {
                // SAFETY: plain handle + id arguments; no callback pointer.
                unsafe { SetTimer(hwnd, TIMER_DEBOUNCE, 5000, None) };
            }
            0
        }
        WM_SETTINGCHANGE => {
            // Light/dark switch: re-read so the icon is redrawn in the matching palette.
            if lparam != 0 {
                let p = lparam as *const u16;
                // SAFETY: a non-null lparam of WM_SETTINGCHANGE is a
                // null-terminated wide string; `all` stops at the first
                // mismatch, so it never reads past the terminator.
                let theme = wide("ImmersiveColorSet")
                    .iter()
                    .enumerate()
                    .all(|(i, &c)| unsafe { *p.add(i) } == c);
                if theme {
                    // A fresh taskbar colour and a redraw of the last view are
                    // all a theme switch needs, so it gets its own path instead
                    // of a mouse poll (which would be dropped while one is in
                    // flight). This also stops every accent-colour broadcast
                    // from HID-polling the mouse.
                    // SAFETY: plain handle + id arguments; no callback pointer.
                    unsafe { SetTimer(hwnd, TIMER_THEME, 300, None) };
                }
            }
            0
        }
        WM_DPICHANGED => {
            // The tray's scale changed (a monitor's was, or the taskbar moved
            // to one with a different scale); render at the new size.
            redraw_worker(hwnd);
            0
        }
        WMAPP_POLLDONE => {
            // SAFETY: WMAPP_POLLDONE is only ever posted by `start_poll` with
            // `lparam` = a `Box<ReadResult>` leaked via `Box::into_raw`; this is
            // the single place that reclaims it, so it is freed exactly once.
            let result = unsafe { *Box::from_raw(lparam as *mut ReadResult) };
            on_poll_done(hwnd, result, wparam);
            0
        }
        WMAPP_REDRAW => {
            // Redrawn with the last reading, in the palette the worker just
            // read off the taskbar.
            redraw_tray(hwnd);
            0
        }
        WMAPP_TRAY => match (lparam & 0xffff) as u32 {
            WM_LBUTTONUP => {
                start_poll(hwnd);
                0
            }
            WM_RBUTTONUP => {
                show_menu(hwnd);
                0
            }
            _ => 0,
        },
        WM_DESTROY => {
            remove_tray(hwnd);
            // SAFETY: no preconditions.
            unsafe { PostQuitMessage(0) };
            0
        }
        _ => {
            if msg != 0 && msg == TASKBAR_CREATED_MSG.load(Ordering::Relaxed) {
                // Explorer restarted; re-add our icon.
                update_tray(hwnd, "…", icon::COLOR_STALE, "mousebatt — reading…", true);
                start_poll(hwnd);
                return 0;
            }
            // SAFETY: forwarding the exact arguments we were called with.
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
    }
}

fn start_poll(hwnd: HWND) {
    if POLLING.swap(true, Ordering::SeqCst) {
        return; // a poll is already in flight
    }
    let hwnd_addr = hwnd as usize;
    std::thread::spawn(move || {
        let hz = REQUESTED_HZ.swap(0, Ordering::SeqCst);
        let announce = hz != 0 && ANNOUNCE.swap(false, Ordering::SeqCst);
        // A requested rate is written, then read back from the mouse; the
        // outcome travels to the UI thread as flags next to the reading.
        let (read, flags) = if hz != 0 {
            let (read, verified) = apply_rate(hz);
            let flags = POLL_TRIED
                | if verified { POLL_VERIFIED } else { 0 }
                | if announce { POLL_ANNOUNCE } else { 0 };
            (read, flags)
        } else {
            (read_battery(), 0)
        };
        let result = Box::into_raw(Box::new(read));
        // Cleared *before* posting: the handler may start the next poll right
        // away (a rate requested meanwhile) and must not find the flag set.
        POLLING.store(false, Ordering::SeqCst);
        // SAFETY: on success, ownership of `result` passes to the message queue
        // and `wndproc` reclaims it. PostMessageW only reads its arguments.
        let posted = unsafe {
            PostMessageW(
                hwnd_addr as HWND,
                WMAPP_POLLDONE,
                flags,
                result as LPARAM,
            )
        };
        if posted == 0 {
            // SAFETY: the post failed (window gone), so nobody else has seen
            // the pointer; reclaiming it here frees it exactly once.
            drop(unsafe { Box::from_raw(result) });
        }
        // The taskbar colour is refreshed only after the result is posted: a
        // screen read can stall (fullscreen transitions, the lock screen) and
        // must never gate the battery read, and a failed read can cost the
        // colour but not the poll. The icon just posted was drawn with the
        // previous colour, so redraw it if the colour moved.
        if icon::refresh_taskbar_color() {
            // SAFETY: WMAPP_REDRAW carries no payload and `wndproc` ignores
            // both parameters.
            unsafe { PostMessageW(hwnd_addr as HWND, WMAPP_REDRAW, 0, 0) };
        }
    });
}

/// Does the reading show the mouse at `hz`?
fn reports(read: &ReadResult, hz: u16) -> bool {
    matches!(read, ReadResult::Ok(s) if s.polling.is_some_and(|p| p.hz == hz))
}

/// Put the mouse at `hz` and *prove* it: read the rate back after each write
/// and write again (up to three times) until the mouse reports it. A rate the
/// mouse already has is not written again. Returns the last reading and
/// whether it shows `hz`.
fn apply_rate(hz: u16) -> (ReadResult, bool) {
    let mut last = read_battery();
    for _ in 0..3 {
        if reports(&last, hz) {
            return (last, true);
        }
        let stop = match &last {
            // Asleep or unplugged: a write would only time out; the retry
            // timer comes back later.
            ReadResult::NoResponse(_) | ReadResult::NoDevice => true,
            // The current link can't do this rate: nothing to retry.
            ReadResult::Ok(s) => s.polling.is_some_and(|p| hz > p.max_hz),
        };
        if stop {
            return (last, false);
        }
        set_polling(hz);
        std::thread::sleep(Duration::from_millis(300));
        last = read_battery();
    }
    let verified = reports(&last, hz);
    (last, verified)
}

/// Re-read the taskbar colour off-screen and redraw what the icon last
/// showed. `refresh_taskbar_color` goes through the screen DC, so it runs on
/// a worker, and the redraw is posted back to the UI thread.
fn redraw_worker(hwnd: HWND) {
    let hwnd_addr = hwnd as usize;
    std::thread::spawn(move || {
        icon::refresh_taskbar_color();
        // SAFETY: WMAPP_REDRAW carries no payload and `wndproc` ignores both
        // parameters.
        unsafe { PostMessageW(hwnd_addr as HWND, WMAPP_REDRAW, 0, 0) };
    });
}

fn on_poll_done(hwnd: HWND, result: ReadResult, flags: usize) {
    let last_pct = LAST_GOOD.with(|g| g.borrow().as_ref().map(|s| s.percent));
    let view = tray_view(&result, last_pct);
    update_tray(hwnd, &view.text, view.color, &view.tip, false);
    let polling = match &result {
        ReadResult::Ok(s) => s.polling,
        _ => None,
    };
    match result {
        ReadResult::Ok(s) => LAST_GOOD.with(|g| *g.borrow_mut() = Some(s)),
        // Forget the last reading so a different mouse plugged in later
        // can't be shown with this one's percentage.
        ReadResult::NoDevice => LAST_GOOD.with(|g| *g.borrow_mut() = None),
        ReadResult::NoResponse(_) => {}
    }
    if flags & POLL_TRIED != 0 {
        if flags & POLL_VERIFIED != 0 {
            RETRY_STEP.with(|r| r.set(0));
            if flags & POLL_ANNOUNCE != 0 {
                // The rate the mouse reported back, not the one we asked for.
                if let Some(p) = polling {
                    sound::confirm(sound_mode(), p.hz);
                }
            }
        } else {
            schedule_retry(hwnd, flags & POLL_ANNOUNCE != 0);
        }
    } else {
        reconcile(hwnd, polling);
    }
    // A rate picked while this poll was in flight is still waiting.
    if REQUESTED_HZ.load(Ordering::SeqCst) != 0 {
        start_poll(hwnd);
    }
}

// ---------------------------------------------------------------------------
// Rates asked for by rules, hotkeys and the menu. Everything below runs on the
// main thread, and only in reaction to an event (focus change, hotkey, timer
// armed by one of those, poll result): there is no scanning loop.

fn sound_mode() -> SoundMode {
    CFG.with(|c| c.borrow().sound)
}

fn warn_user(msg: String) {
    WARNINGS.with(|w| w.borrow_mut().push(msg));
}

/// Highest rate the current link supports, if a reading has told us.
fn clamp_to_link(hz: u16) -> u16 {
    LAST_GOOD.with(|g| {
        g.borrow()
            .as_ref()
            .and_then(|s| s.polling)
            .map_or(hz, |p| hz.min(p.max_hz))
    })
}

/// The rate the mouse should have right now, as far as we know (0 = unknown).
fn current_rate() -> u16 {
    let wanted = WANTED.with(Cell::get);
    if wanted != 0 {
        return clamp_to_link(wanted);
    }
    LAST_GOOD.with(|g| {
        g.borrow()
            .as_ref()
            .and_then(|s| s.polling)
            .map_or(0, |p| p.hz)
    })
}

/// Rates the current link offers, ascending (all of them until a reading says
/// what the link supports).
fn rate_ladder() -> Vec<u16> {
    let info = LAST_GOOD.with(|g| g.borrow().as_ref().and_then(|s| s.polling));
    match info {
        Some(p) => p.rates.iter().copied().filter(|&hz| hz <= p.max_hz).collect(),
        None => PULSAR_RATES.to_vec(),
    }
}

/// Want `hz` on the mouse: remember it, write it and read it back.
fn request_rate(hwnd: HWND, hz: u16, announce: bool) {
    WANTED.with(|w| w.set(hz));
    RETRY_STEP.with(|r| r.set(0));
    // SAFETY: plain handle + id arguments. A newer request supersedes a retry.
    unsafe { KillTimer(hwnd, TIMER_RETRY) };
    ANNOUNCE.store(announce, Ordering::SeqCst);
    queue_apply(hwnd);
}

/// Hand the wanted rate (lowered to what the link supports) to the worker.
/// `ANNOUNCE` is stored before `REQUESTED_HZ` so the worker that sees the rate
/// also sees the sound request.
fn queue_apply(hwnd: HWND) {
    let hz = WANTED.with(Cell::get);
    if hz == 0 {
        return;
    }
    REQUESTED_HZ.store(clamp_to_link(hz), Ordering::SeqCst);
    start_poll(hwnd);
}

/// The mouse didn't confirm the rate (asleep, write lost): try again a few
/// times with growing pauses, then give up until the next periodic check.
fn schedule_retry(hwnd: HWND, announce: bool) {
    let step = RETRY_STEP.with(|r| {
        let s = r.get();
        r.set(s.saturating_add(1));
        s
    });
    match RETRY_DELAYS_MS.get(usize::from(step)) {
        Some(&ms) => {
            if announce {
                ANNOUNCE.store(true, Ordering::SeqCst);
            }
            // SAFETY: plain handle + id arguments; no callback pointer.
            unsafe { SetTimer(hwnd, TIMER_RETRY, ms, None) };
        }
        None => {
            if announce {
                sound::error(sound_mode());
            }
        }
    }
}

/// After a poll that wrote nothing: if the mouse reports another rate than
/// the one wanted (changed by another tool, reset by the Pulsar app, link
/// swapped between cable and dongle), put it back. Only with `enforce`.
/// A poll that did write never lands here, so this cannot loop.
fn reconcile(hwnd: HWND, polling: Option<PollingInfo>) {
    let Some(p) = polling else { return };
    let want = WANTED.with(Cell::get);
    if want == 0 || !CFG.with(|c| c.borrow().enforce) {
        return;
    }
    if p.hz != want.min(p.max_hz) {
        RETRY_STEP.with(|r| r.set(0));
        queue_apply(hwnd);
    }
}

/// Hotkey / menu pick: take manual control, pausing the per-program rules.
fn request_manual(hwnd: HWND, hz: u16, announce: bool) {
    AUTO.with(|a| a.set(false));
    request_rate(hwnd, hz, announce);
}

fn step_rate(hwnd: HWND, up: bool) {
    let cur = current_rate();
    let ladder = rate_ladder();
    let next = if up {
        ladder.iter().copied().find(|&hz| hz > cur)
    } else {
        ladder.iter().rev().copied().find(|&hz| hz < cur)
    };
    match next {
        Some(hz) if cur != 0 => request_manual(hwnd, hz, true),
        // Already at the end of the ladder, or the current rate is unknown.
        _ => sound::error(sound_mode()),
    }
}

fn on_hotkey(hwnd: HWND, id: usize) {
    let action = CFG.with(|c| {
        c.borrow()
            .hotkeys
            .get(id.wrapping_sub(1))
            .map(|h| h.action.clone())
    });
    let Some(action) = action else { return };
    match action {
        Action::Set(hz) => request_manual(hwnd, hz, true),
        Action::Up => step_rate(hwnd, true),
        Action::Down => step_rate(hwnd, false),
        Action::Toggle(a, b) => {
            let next = if current_rate() == a { b } else { a };
            request_manual(hwnd, next, true);
        }
        Action::Auto => {
            AUTO.with(|c| c.set(true));
            sound::notice(sound_mode());
            eval_foreground();
        }
    }
}

/// Focus callback. Windows posts one call to our message loop per change of
/// the foreground window; nothing runs in the other program.
unsafe extern "system" fn on_foreground(
    _hook: HWINEVENTHOOK,
    _event: u32,
    hwnd: HWND,
    _id_object: i32,
    _id_child: i32,
    _event_thread: u32,
    _event_time: u32,
) {
    if let Some(name) = watch::exe_name_of_window(hwnd) {
        foreground_changed(&name);
    }
}

/// Apply the focus rule for the program `exe` (lower-case file name): arm the
/// one-shot timer if its rate isn't the one already wanted, or disarm it if
/// the focus went back to a program whose rate is. Moving through other
/// windows in less than `delay_ms` (alt-tab) therefore writes nothing.
fn foreground_changed(exe: &str) {
    if !AUTO.with(Cell::get) {
        return;
    }
    let hwnd = MAIN_HWND.load(Ordering::Relaxed) as HWND;
    let (want, delay) = CFG.with(|c| {
        let c = c.borrow();
        (c.wanted_for(exe), c.delay_ms)
    });
    match want {
        Some(hz) if hz != WANTED.with(Cell::get) => {
            PENDING_AUTO.with(|p| p.set(hz));
            // SAFETY: plain handle + id arguments; no callback pointer. Setting
            // a timer that exists restarts it.
            unsafe { SetTimer(hwnd, TIMER_AUTO, delay, None) };
        }
        _ => {
            // SAFETY: plain handle + id arguments.
            unsafe { KillTimer(hwnd, TIMER_AUTO) };
        }
    }
}

/// Look at the program that has the focus right now (startup, reload, back to
/// automatic mode).
fn eval_foreground() {
    // SAFETY: no arguments.
    let hwnd = unsafe { GetForegroundWindow() };
    if let Some(name) = watch::exe_name_of_window(hwnd) {
        foreground_changed(&name);
    }
}

/// (Re)read `mousebatt.ini` and bring the hook and hotkeys in line with it.
fn reload_config(hwnd: HWND) {
    let mut cfg = config::load();
    for w in std::mem::take(&mut cfg.warnings) {
        warn_user(w);
    }
    CFG.with(|c| *c.borrow_mut() = cfg);
    sync_hotkeys(hwnd);
    sync_hook();
    eval_foreground();
    if WARNINGS.with(|w| !w.borrow().is_empty()) {
        // SAFETY: WMAPP_WARN carries no payload.
        unsafe { PostMessageW(hwnd, WMAPP_WARN, 0, 0) };
    }
}

/// The focus hook exists only while the file has rules: without any, mousebatt
/// receives no focus events at all.
fn sync_hook() {
    let need = CFG.with(|c| c.borrow().has_rules());
    let have = HOOK.with(Cell::get) != 0;
    if need && !have {
        let hook = watch::install(on_foreground);
        if hook == 0 {
            warn_user("Could not start the focus watcher; program rules are inactive.".into());
        }
        HOOK.with(|h| h.set(hook));
    } else if !need && have {
        watch::uninstall(HOOK.with(|h| h.replace(0)));
    }
}

fn sync_hotkeys(hwnd: HWND) {
    for id in 1..=HOTKEY_COUNT.with(Cell::get) {
        // SAFETY: plain handle + id arguments; an id that was never
        // registered just makes the call fail.
        unsafe { UnregisterHotKey(hwnd, id as i32) };
    }
    let keys: Vec<(String, u32, u32)> = CFG.with(|c| {
        c.borrow()
            .hotkeys
            .iter()
            .map(|h| (h.label.clone(), h.mods, h.vk))
            .collect()
    });
    for (i, (label, mods, vk)) in keys.iter().enumerate() {
        // SAFETY: plain handle + integer arguments. NOREPEAT: holding the keys
        // down triggers the action once.
        let ok = unsafe { RegisterHotKey(hwnd, (i + 1) as i32, *mods | MOD_NOREPEAT, *vk) };
        if ok == 0 {
            warn_user(format!(
                "Hotkey {label} could not be registered (already used by another program?)."
            ));
        }
    }
    HOTKEY_COUNT.with(|c| c.set(keys.len()));
}

fn show_warnings() {
    let lines = WARNINGS.with(|w| std::mem::take(&mut *w.borrow_mut()));
    if lines.is_empty() {
        return;
    }
    let mut text = lines.iter().take(12).cloned().collect::<Vec<_>>().join("\n");
    if lines.len() > 12 {
        text.push_str(&format!("\n… and {} more.", lines.len() - 12));
    }
    let wtext = wide(&text);
    let wtitle = wide("mousebatt - mousebatt.ini");
    // SAFETY: both strings are NUL-terminated and outlive the call; a null
    // owner window is allowed.
    unsafe {
        MessageBoxW(
            null_mut(),
            wtext.as_ptr(),
            wtitle.as_ptr(),
            MB_OK | MB_ICONWARNING,
        )
    };
}

/// Open `mousebatt.ini` in the default editor, creating the commented example
/// first if there is none.
fn open_config() {
    let path = config::path();
    if !path.exists() {
        let _ = std::fs::write(&path, config::TEMPLATE);
    }
    let wpath = wide(&path.to_string_lossy());
    // SAFETY: every string is NUL-terminated and lives for the call; null
    // owner window, parameters and directory are allowed.
    unsafe {
        ShellExecuteW(
            null_mut(),
            wide("open").as_ptr(),
            wpath.as_ptr(),
            null(),
            null(),
            SW_SHOWNORMAL,
        )
    };
}

/// What the tray should show for a poll result (pure; unit-tested).
#[derive(Clone)]
struct TrayView {
    text: String,
    color: u32,
    tip: String,
}

fn tray_view(result: &ReadResult, last_pct: Option<u8>) -> TrayView {
    match result {
        ReadResult::Ok(s) => {
            let color = if s.charging {
                icon::COLOR_CHARGING
            } else if s.percent <= 20 {
                icon::COLOR_LOW
            } else {
                icon::COLOR_NORMAL
            };
            let mut tip = format!("{} — {}%", s.product, s.percent);
            if s.charging {
                tip.push_str(" (charging)");
            }
            if let Some(mv) = s.voltage_mv {
                tip.push_str(&format!(" · {:.2} V", mv as f32 / 1000.0));
            }
            if let Some(p) = s.polling {
                tip.push_str(&format!(" · {} Hz", p.hz));
            }
            TrayView {
                text: s.percent.to_string(),
                color,
                tip,
            }
        }
        ReadResult::NoResponse(product) => match last_pct {
            Some(pct) => TrayView {
                text: pct.to_string(),
                color: icon::COLOR_STALE,
                tip: format!("{} — {}% (stale, mouse not responding)", product, pct),
            },
            None => TrayView {
                text: "?".into(),
                color: icon::COLOR_STALE,
                tip: format!("{} — not responding (asleep?)", product),
            },
        },
        ReadResult::NoDevice => TrayView {
            text: "?".into(),
            color: icon::COLOR_STALE,
            tip: "No supported mouse found".into(),
        },
    }
}

fn base_nid(hwnd: HWND) -> NOTIFYICONDATAW {
    // SAFETY: NOTIFYICONDATAW is plain data for which all-zero is valid.
    let mut nid: NOTIFYICONDATAW = unsafe { zeroed() };
    nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd = hwnd;
    nid.uID = 1;
    nid
}

fn update_tray(hwnd: HWND, text: &str, color: u32, tip: &str, add: bool) {
    let new_icon = icon::battery_icon(text, color, GLYPH.with(Cell::get));
    LAST_VIEW.with(|v| {
        *v.borrow_mut() = Some(TrayView {
            text: text.into(),
            color,
            tip: tip.into(),
        })
    });
    let mut nid = base_nid(hwnd);
    nid.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
    nid.uCallbackMessage = WMAPP_TRAY;
    nid.hIcon = new_icon.raw();
    let wtip = wide(tip);
    let n = wtip.len().min(nid.szTip.len() - 1); // keep the trailing NUL
    nid.szTip[..n].copy_from_slice(&wtip[..n]);
    // If the icon was never successfully added (e.g. NIM_ADD failed while the
    // shell was busy at logon), NIM_MODIFY fails; fall back to NIM_ADD.
    // SAFETY: `nid` is fully initialised with cbSize set; the shell copies the
    // icon, so `new_icon` need only live for the call.
    unsafe {
        if add || Shell_NotifyIconW(NIM_MODIFY, &nid) == 0 {
            Shell_NotifyIconW(NIM_ADD, &nid);
        }
    }
    // Replacing drops (DestroyIcon) the previous icon.
    CUR_ICON.with(|c| *c.borrow_mut() = Some(new_icon));
}

/// Redraw the icon with what it last showed, e.g. after a display setting
/// changed, without waiting for the mouse.
fn redraw_tray(hwnd: HWND) {
    if let Some(v) = LAST_VIEW.with(|v| v.borrow().clone()) {
        update_tray(hwnd, &v.text, v.color, &v.tip, false);
    }
}

fn remove_tray(hwnd: HWND) {
    let nid = base_nid(hwnd);
    // SAFETY: `nid` is fully initialised with cbSize set.
    unsafe { Shell_NotifyIconW(NIM_DELETE, &nid) };
}

fn show_menu(hwnd: HWND) {
    let autostart = autostart_enabled();
    let glyph = GLYPH.with(Cell::get);
    let polling = LAST_GOOD.with(|g| g.borrow().as_ref().and_then(|s| s.polling));
    let has_rules = CFG.with(|c| c.borrow().has_rules());
    let auto = AUTO.with(Cell::get);
    let mut pt = POINT { x: 0, y: 0 };
    // SAFETY: the menu (and the submenu it owns) is created and destroyed
    // within this block; every string temporary lives for the full statement
    // that passes it; `pt` is a valid out-pointer.
    let cmd = unsafe {
        let menu = CreatePopupMenu();
        AppendMenuW(menu, MF_STRING, MENU_REFRESH, wide("Refresh now").as_ptr());
        // Only shown once the mouse has reported its rate; lists what the
        // current link (cable or dongle) can do.
        if let Some(p) = polling {
            let sub = CreatePopupMenu();
            for (i, &hz) in p.rates.iter().enumerate() {
                if hz > p.max_hz {
                    break;
                }
                AppendMenuW(
                    sub,
                    MF_STRING | if hz == p.hz { MF_CHECKED } else { 0 },
                    MENU_RATE_BASE + i,
                    wide(&format!("{hz} Hz")).as_ptr(),
                );
            }
            // DestroyMenu(menu) below also destroys the attached submenu.
            AppendMenuW(menu, MF_POPUP, sub as usize, wide("Polling rate").as_ptr());
        }
        let icon_sub = CreatePopupMenu();
        for (i, g) in BatteryGlyph::ALL.into_iter().enumerate() {
            AppendMenuW(
                icon_sub,
                MF_STRING | if g == glyph { MF_CHECKED } else { 0 },
                MENU_GLYPH_BASE + i,
                wide(glyph_label(g)).as_ptr(),
            );
        }
        AppendMenuW(
            menu,
            MF_POPUP,
            icon_sub as usize,
            wide("Battery icon").as_ptr(),
        );
        if has_rules {
            AppendMenuW(
                menu,
                MF_STRING | if auto { MF_CHECKED } else { 0 },
                MENU_AUTO,
                wide("Automatic (mousebatt.ini rules)").as_ptr(),
            );
        }
        AppendMenuW(
            menu,
            MF_STRING,
            MENU_EDIT_CFG,
            wide("Edit mousebatt.ini…").as_ptr(),
        );
        AppendMenuW(
            menu,
            MF_STRING,
            MENU_RELOAD_CFG,
            wide("Reload mousebatt.ini").as_ptr(),
        );
        AppendMenuW(
            menu,
            MF_STRING | if autostart { MF_CHECKED } else { 0 },
            MENU_AUTOSTART,
            wide("Start with Windows").as_ptr(),
        );
        AppendMenuW(menu, MF_SEPARATOR, 0, null());
        AppendMenuW(menu, MF_STRING, MENU_EXIT, wide("Exit").as_ptr());

        GetCursorPos(&mut pt);
        SetForegroundWindow(hwnd); // required so the menu dismisses on outside click
        let cmd = TrackPopupMenu(
            menu,
            TPM_RIGHTBUTTON | TPM_RETURNCMD | TPM_NONOTIFY,
            pt.x,
            pt.y,
            0,
            hwnd,
            null(),
        );
        DestroyMenu(menu);
        cmd
    };
    match cmd as usize {
        MENU_REFRESH => start_poll(hwnd),
        MENU_AUTOSTART => set_autostart(!autostart),
        MENU_AUTO => {
            AUTO.with(|a| a.set(!auto));
            if !auto {
                eval_foreground();
            }
        }
        MENU_EDIT_CFG => open_config(),
        MENU_RELOAD_CFG => reload_config(hwnd),
        MENU_EXIT => {
            remove_tray(hwnd);
            // SAFETY: no preconditions.
            unsafe { PostQuitMessage(0) };
        }
        id => {
            if let Some(&g) = id
                .checked_sub(MENU_GLYPH_BASE)
                .and_then(|i| BatteryGlyph::ALL.get(i))
            {
                GLYPH.with(|c| c.set(g));
                save_glyph(g);
                redraw_tray(hwnd);
            } else if let Some(&hz) = id
                .checked_sub(MENU_RATE_BASE)
                .and_then(|i| polling?.rates.get(i))
            {
                request_manual(hwnd, hz, false);
            }
        }
    }
}

fn glyph_label(g: BatteryGlyph) -> &'static str {
    match g {
        BatteryGlyph::Hidden => "Hidden",
        BatteryGlyph::Above => "Above the number",
        BatteryGlyph::Below => "Below the number",
    }
}

const SETTINGS_KEY: &str = "Software\\mousebatt";
const GLYPH_VALUE: &str = "BatteryGlyph";
/// Used until the user picks another from the menu: the plain number, as
/// before the glyph existed.
const DEFAULT_GLYPH: BatteryGlyph = BatteryGlyph::Hidden;

/// The REG_DWORD stored for a battery-icon choice.
fn glyph_setting(g: BatteryGlyph) -> u32 {
    match g {
        BatteryGlyph::Hidden => 0,
        BatteryGlyph::Above => 1,
        BatteryGlyph::Below => 2,
    }
}

fn glyph_from_setting(value: u32) -> Option<BatteryGlyph> {
    BatteryGlyph::ALL
        .into_iter()
        .find(|&g| glyph_setting(g) == value)
}

/// The saved battery-icon choice, or the default if none (or an unknown one)
/// is stored.
fn load_glyph() -> BatteryGlyph {
    let mut data: u32 = 0;
    let mut size = size_of::<u32>() as u32;
    // SAFETY: the name temporaries live for the full statement, and
    // `data`/`size` describe a writable 4-byte buffer for a REG_DWORD.
    let rc = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            wide(SETTINGS_KEY).as_ptr(),
            wide(GLYPH_VALUE).as_ptr(),
            RRF_RT_REG_DWORD,
            null_mut(),
            &mut data as *mut u32 as *mut c_void,
            &mut size,
        )
    };
    if rc == 0 {
        glyph_from_setting(data).unwrap_or(DEFAULT_GLYPH)
    } else {
        DEFAULT_GLYPH
    }
}

fn save_glyph(g: BatteryGlyph) {
    let data = glyph_setting(g);
    // SAFETY: the name temporaries live for the full statement, and `data` is
    // a 4-byte REG_DWORD that is copied. The key is created if missing.
    unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            wide(SETTINGS_KEY).as_ptr(),
            wide(GLYPH_VALUE).as_ptr(),
            REG_DWORD,
            &data as *const u32 as *const c_void,
            size_of::<u32>() as u32,
        );
    }
}

const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const RUN_VALUE: &str = "MouseBatt";

/// Open registry key, closed on drop.
struct RegKey(HKEY);

impl RegKey {
    fn open_run(access: u32) -> Option<RegKey> {
        let mut key = null_mut();
        // SAFETY: the path temporary lives for the full statement; `key` is a
        // valid out-pointer, and is only wrapped if the open succeeded.
        let rc = unsafe {
            RegOpenKeyExW(
                HKEY_CURRENT_USER,
                wide(RUN_KEY).as_ptr(),
                0,
                access,
                &mut key,
            )
        };
        (rc == 0).then_some(RegKey(key))
    }
}

impl Drop for RegKey {
    fn drop(&mut self) {
        // SAFETY: `self.0` came from a successful RegOpenKeyExW and is closed
        // exactly once.
        unsafe { RegCloseKey(self.0) };
    }
}

fn autostart_enabled() -> bool {
    let Some(key) = RegKey::open_run(KEY_QUERY_VALUE) else {
        return false;
    };
    // SAFETY: all-null out-pointers are permitted (existence check only).
    unsafe {
        RegQueryValueExW(
            key.0,
            wide(RUN_VALUE).as_ptr(),
            null_mut(),
            null_mut(),
            null_mut(),
            null_mut(),
        ) == 0
    }
}

fn set_autostart(enable: bool) {
    let Some(key) = RegKey::open_run(KEY_SET_VALUE) else {
        return;
    };
    if enable {
        if let Ok(exe) = std::env::current_exe() {
            let value = wide(&format!("\"{}\"", exe.display()));
            // SAFETY: `value` is a NUL-terminated wide string and the byte
            // length passed (u16 count * 2) covers it including the terminator,
            // as REG_SZ requires.
            unsafe {
                RegSetValueExW(
                    key.0,
                    wide(RUN_VALUE).as_ptr(),
                    0,
                    REG_SZ,
                    value.as_ptr() as *const u8,
                    (value.len() * 2) as u32,
                );
            }
        }
    } else {
        // SAFETY: the name temporary lives for the full statement.
        unsafe { RegDeleteValueW(key.0, wide(RUN_VALUE).as_ptr()) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(percent: u8, charging: bool, mv: Option<u16>) -> ReadResult {
        ReadResult::Ok(BatteryStatus {
            percent,
            charging,
            voltage_mv: mv,
            product: "X3".into(),
            polling: None,
        })
    }

    #[test]
    fn ok_tip_includes_polling_rate() {
        let mut r = status(85, false, Some(3912));
        if let ReadResult::Ok(s) = &mut r {
            s.polling = Some(protocol::PollingInfo {
                hz: 4000,
                max_hz: 8000,
                rates: &protocol::PULSAR_RATES,
            });
        }
        assert_eq!(tray_view(&r, None).tip, "X3 — 85% · 3.91 V · 4000 Hz");
    }

    #[test]
    fn ok_normal_with_voltage() {
        let v = tray_view(&status(85, false, Some(3912)), None);
        assert_eq!(v.text, "85");
        assert_eq!(v.color, icon::COLOR_NORMAL);
        assert_eq!(v.tip, "X3 — 85% · 3.91 V");
    }

    #[test]
    fn ok_low_threshold_is_inclusive() {
        assert_eq!(
            tray_view(&status(20, false, None), None).color,
            icon::COLOR_LOW
        );
        assert_eq!(
            tray_view(&status(21, false, None), None).color,
            icon::COLOR_NORMAL
        );
    }

    #[test]
    fn ok_charging_overrides_low() {
        let v = tray_view(&status(5, true, None), None);
        assert_eq!(v.color, icon::COLOR_CHARGING);
        assert_eq!(v.tip, "X3 — 5% (charging)");
    }

    #[test]
    fn no_response_shows_stale_last_value() {
        let v = tray_view(&ReadResult::NoResponse("X3".into()), Some(60));
        assert_eq!(v.text, "60");
        assert_eq!(v.color, icon::COLOR_STALE);
        assert_eq!(v.tip, "X3 — 60% (stale, mouse not responding)");
    }

    #[test]
    fn no_response_without_history() {
        let v = tray_view(&ReadResult::NoResponse("X3".into()), None);
        assert_eq!(v.text, "?");
        assert_eq!(v.tip, "X3 — not responding (asleep?)");
    }

    #[test]
    fn glyph_setting_round_trips_and_ignores_unknown_values() {
        for g in BatteryGlyph::ALL {
            assert_eq!(glyph_from_setting(glyph_setting(g)), Some(g));
        }
        let stored: Vec<u32> = BatteryGlyph::ALL.into_iter().map(glyph_setting).collect();
        assert_eq!(stored, [0, 1, 2], "stored values must stay stable");
        assert_eq!(glyph_from_setting(3), None);
    }

    #[test]
    fn no_device() {
        let v = tray_view(&ReadResult::NoDevice, Some(60));
        assert_eq!(v.text, "?");
        assert_eq!(v.color, icon::COLOR_STALE);
        assert_eq!(v.tip, "No supported mouse found");
    }
}
