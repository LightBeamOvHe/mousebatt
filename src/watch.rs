//! Which program has the focus, without ever scanning.
//!
//! Windows tells us when the foreground window changes (`SetWinEventHook`
//! with `EVENT_SYSTEM_FOREGROUND`). The hook is *out of context*: Windows
//! posts one message to our own message loop per change and nothing is
//! injected into any other process. Between changes there is no timer, no
//! thread and no process-list walk; per change the cost is one
//! `OpenProcess` + `QueryFullProcessImageNameW` (microseconds).
use std::ptr::null_mut;

use windows_sys::Win32::Foundation::{CloseHandle, HWND};
use windows_sys::Win32::System::Threading::{OpenProcess, QueryFullProcessImageNameW};
use windows_sys::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows_sys::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId;

const EVENT_SYSTEM_FOREGROUND: u32 = 0x0003;
const WINEVENT_OUTOFCONTEXT: u32 = 0x0000;
const WINEVENT_SKIPOWNPROCESS: u32 = 0x0002;
const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;

/// Signature Windows expects for a WinEvent callback.
pub type ForegroundProc = unsafe extern "system" fn(
    hook: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    id_object: i32,
    id_child: i32,
    event_thread: u32,
    event_time: u32,
);

/// Start receiving foreground-change events on the calling thread (which must
/// run a message loop). Returns the hook handle as an integer, 0 on failure.
pub fn install(callback: ForegroundProc) -> usize {
    // SAFETY: out-of-context hook, so no module handle is needed; `callback`
    // is a plain function that lives for the whole process.
    let hook = unsafe {
        SetWinEventHook(
            EVENT_SYSTEM_FOREGROUND,
            EVENT_SYSTEM_FOREGROUND,
            null_mut(),
            Some(callback),
            0,
            0,
            WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
        )
    };
    hook as usize
}

/// Stop receiving events; `hook` is a value returned by `install`.
pub fn uninstall(hook: usize) {
    if hook != 0 {
        // SAFETY: `hook` came from a successful `SetWinEventHook` and is
        // unhooked once (the caller clears its copy).
        unsafe { UnhookWinEvent(hook as HWINEVENTHOOK) };
    }
}

/// Lower-cased file name (`discovery.exe`) of the program owning `hwnd`.
/// `None` if the window has no process or Windows refuses the query; callers
/// treat that as "don't touch the mouse".
pub fn exe_name_of_window(hwnd: HWND) -> Option<String> {
    let mut pid: u32 = 0;
    // SAFETY: `pid` is a valid out-pointer.
    unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
    if pid == 0 {
        return None;
    }
    // Limited-query access is the least that can read an image name; it is
    // what Task Manager and overlays ask for, and it grants no way to read or
    // change the target's memory.
    // SAFETY: plain integer arguments.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if process.is_null() {
        return None;
    }
    let mut buf = [0u16; 1024];
    let mut len = buf.len() as u32;
    // SAFETY: `buf` holds `len` u16s; `process` is the handle opened above.
    let ok = unsafe { QueryFullProcessImageNameW(process, 0, buf.as_mut_ptr(), &mut len) };
    // SAFETY: `process` is a handle we own and close exactly once.
    unsafe { CloseHandle(process) };
    if ok == 0 {
        return None;
    }
    let path = String::from_utf16_lossy(&buf[..len as usize]);
    let file = path.rsplit(['\\', '/']).next()?;
    Some(file.to_lowercase())
}
