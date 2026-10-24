//! Audible feedback for rate changes. Played only after the mouse has
//! confirmed the new rate, and always for the rate it actually reports.
//!
//! * `beep`: one short beep, higher pitch for a higher rate.
//! * `voice`: a spoken "two thousand hertz". The clips are built into the exe;
//!   a file `sounds\<hz>.wav` next to the exe (e.g. `sounds\2000.wav`) replaces
//!   the built-in clip for that rate, so any voice or language can be used.
//!
//! Nothing here runs unless a sound is due: no timer, no thread, no device is
//! held open.
use std::path::PathBuf;
use std::ptr::null_mut;

use windows_sys::Win32::Media::Audio::PlaySoundW;
use windows_sys::Win32::System::Diagnostics::Debug::Beep;

use crate::config::SoundMode;

const SND_ASYNC: u32 = 0x0001;
const SND_NODEFAULT: u32 = 0x0002;
const SND_MEMORY: u32 = 0x0004;
const SND_FILENAME: u32 = 0x0002_0000;

static CLIP_125: &[u8] = include_bytes!("../sounds/125.wav");
static CLIP_250: &[u8] = include_bytes!("../sounds/250.wav");
static CLIP_500: &[u8] = include_bytes!("../sounds/500.wav");
static CLIP_1000: &[u8] = include_bytes!("../sounds/1000.wav");
static CLIP_2000: &[u8] = include_bytes!("../sounds/2000.wav");
static CLIP_4000: &[u8] = include_bytes!("../sounds/4000.wav");
static CLIP_8000: &[u8] = include_bytes!("../sounds/8000.wav");

fn builtin_clip(hz: u16) -> Option<&'static [u8]> {
    Some(match hz {
        125 => CLIP_125,
        250 => CLIP_250,
        500 => CLIP_500,
        1000 => CLIP_1000,
        2000 => CLIP_2000,
        4000 => CLIP_4000,
        8000 => CLIP_8000,
        _ => return None,
    })
}

/// Beep pitch (Hz) for a polling rate: one musical step per rate.
fn pitch(hz: u16) -> u32 {
    match hz {
        125 => 523,
        250 => 587,
        500 => 659,
        1000 => 784,
        2000 => 988,
        4000 => 1175,
        _ => 1319,
    }
}

/// The mouse confirmed `hz`.
pub fn confirm(mode: SoundMode, hz: u16) {
    match mode {
        SoundMode::Off => {}
        SoundMode::Beep => beep(pitch(hz), 110),
        SoundMode::Voice => voice(hz),
    }
}

/// The rate could not be applied (mouse not answering, or no further step).
pub fn error(mode: SoundMode) {
    if mode != SoundMode::Off {
        beep(220, 250);
    }
}

/// Neutral acknowledgement that is not a rate (e.g. automatic mode resumed).
pub fn notice(mode: SoundMode) {
    if mode != SoundMode::Off {
        beep(880, 60);
    }
}

/// `Beep` blocks for its duration, so it gets its own short-lived thread.
fn beep(freq: u32, ms: u32) {
    std::thread::spawn(move || {
        // SAFETY: plain integer arguments.
        unsafe { Beep(freq, ms) };
    });
}

fn custom_clip_path(hz: u16) -> Option<Vec<u16>> {
    let exe = std::env::current_exe().ok()?;
    let path: PathBuf = exe.with_file_name("sounds").join(format!("{hz}.wav"));
    if !path.is_file() {
        return None;
    }
    Some(
        path.to_string_lossy()
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect(),
    )
}

fn voice(hz: u16) {
    if let Some(wpath) = custom_clip_path(hz) {
        // SAFETY: `wpath` is a NUL-terminated wide string that outlives the
        // call; SND_FILENAME makes PlaySound read it as a path. With SND_ASYNC
        // PlaySound copies what it needs before returning.
        unsafe { PlaySoundW(wpath.as_ptr(), null_mut(), SND_FILENAME | SND_ASYNC | SND_NODEFAULT) };
    } else if let Some(clip) = builtin_clip(hz) {
        // SAFETY: with SND_MEMORY the first argument is a pointer to the WAV
        // image, not a string; `clip` is `'static`, so it stays valid for the
        // whole asynchronous playback.
        unsafe {
            PlaySoundW(
                clip.as_ptr() as *const u16,
                null_mut(),
                SND_MEMORY | SND_ASYNC | SND_NODEFAULT,
            )
        };
    } else {
        beep(pitch(hz), 110);
    }
}
