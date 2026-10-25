//! Which programs are running, without ever scanning while you play.
//!
//! Windows tells us when the foreground window changes (`SetWinEventHook`
//! with `EVENT_SYSTEM_FOREGROUND`). The hook is *out of context*: Windows
//! posts one message to our own message loop per change and nothing is
//! injected into any other process. Between changes there is no timer, no
//! thread and no process-list walk.
//!
//! A listed program is noticed the first time its window takes the focus; from
//! then on it counts as "running" whatever has the focus. At each focus change
//! the few programs we are tracking get one cheap "is it gone?" check (one
//! `OpenProcess`), which is how a closed game is noticed. The only full
//! process listing is `find_running`, used once at startup / config reload.
use std::ptr::null_mut;

use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, HANDLE, HWND, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
};
use windows_sys::Win32::System::Threading::{
    GetExitCodeProcess, OpenProcess, QueryFullProcessImageNameW,
};
use windows_sys::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows_sys::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId;

const EVENT_SYSTEM_FOREGROUND: u32 = 0x0003;
const WINEVENT_OUTOFCONTEXT: u32 = 0x0000;
const WINEVENT_SKIPOWNPROCESS: u32 = 0x0002;
const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
const TH32CS_SNAPPROCESS: u32 = 0x0000_0002;
const STILL_ACTIVE: u32 = 259;
const ERROR_INVALID_PARAMETER: u32 = 87;

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

/// Lower-cased full path of the program behind an open process handle.
fn image_path(process: HANDLE) -> Option<String> {
    let mut buf = [0u16; 1024];
    let mut len = buf.len() as u32;
    // SAFETY: `buf` holds `len` u16s; `process` is a valid open handle.
    let ok = unsafe { QueryFullProcessImageNameW(process, 0, buf.as_mut_ptr(), &mut len) };
    if ok == 0 {
        return None;
    }
    Some(String::from_utf16_lossy(&buf[..len as usize]).to_lowercase())
}

/// The file name at the end of a path (`c:\games\x\x.exe` -> `x.exe`).
fn file_name(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

/// Process id, lower-cased file name (`discovery.exe`) and lower-cased full
/// path of the program that owns `hwnd`. `None` if Windows refuses the query;
/// callers treat that as "don't touch the mouse".
pub fn process_of_window(hwnd: HWND) -> Option<(u32, String, String)> {
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
    let path = image_path(process);
    // SAFETY: `process` is a handle we own and close exactly once.
    unsafe { CloseHandle(process) };
    path.map(|p| (pid, file_name(&p).to_string(), p))
}

/// True only when process `pid` is certainly no longer the program `exe`:
/// it has exited, its id no longer exists, or the id now belongs to another
/// program. A failed query (access denied, ...) counts as "still there", so a
/// hiccup can never drop a game's rate while it runs.
pub fn is_gone(pid: u32, exe: &str) -> bool {
    // SAFETY: plain integer arguments.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if process.is_null() {
        // SAFETY: no arguments.
        return unsafe { GetLastError() } == ERROR_INVALID_PARAMETER;
    }
    let mut code: u32 = 0;
    // SAFETY: `code` is a valid out-pointer; `process` is a valid handle.
    let got_code = unsafe { GetExitCodeProcess(process, &mut code) } != 0;
    let name = image_path(process).map(|p| file_name(&p).to_string());
    // SAFETY: `process` is a handle we own and close exactly once.
    unsafe { CloseHandle(process) };
    if got_code && code != STILL_ACTIVE {
        return true;
    }
    matches!(name, Some(n) if n != exe)
}

/// One snapshot of the running programs: every (name, pid) whose lower-cased
/// file name is in `names`. Called once at startup and on config reload,
/// never while a game runs.
pub fn find_running(names: &[String]) -> Vec<(String, u32)> {
    let mut found = Vec::new();
    if names.is_empty() {
        return found;
    }
    // SAFETY: plain integer arguments; the handle is checked below.
    let snap = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snap == INVALID_HANDLE_VALUE || snap.is_null() {
        return found;
    }
    // SAFETY: PROCESSENTRY32W is plain data for which all-zero is valid.
    let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
    entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
    // SAFETY: `snap` is a valid snapshot handle and `entry` has dwSize set.
    let mut more = unsafe { Process32FirstW(snap, &mut entry) } != 0;
    while more {
        let len = entry
            .szExeFile
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(entry.szExeFile.len());
        let name = String::from_utf16_lossy(&entry.szExeFile[..len]).to_lowercase();
        if names.iter().any(|n| *n == name) {
            found.push((name, entry.th32ProcessID));
        }
        // SAFETY: same as for Process32FirstW.
        more = unsafe { Process32NextW(snap, &mut entry) } != 0;
    }
    // SAFETY: `snap` is a handle we own and close exactly once.
    unsafe { CloseHandle(snap) };
    found
}
