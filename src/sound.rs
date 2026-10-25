//! Audible feedback for rate changes. Played only after the mouse has
//! confirmed the new rate, and always for the rate it actually reports.
//!
//! * `beep`: a soft bell note, higher for a higher rate (a pentatonic scale,
//!   so every step sounds pleasant).
//! * `voice`: the rate spoken as a number ("two thousand"). The clips are
//!   built into the exe; a file `sounds\<hz>.wav` next to the exe (e.g.
//!   `sounds\2000.wav`) replaces the built-in clip for that rate, so any
//!   voice or language can be used.
//!
//! Everything is played from memory with one `PlaySound` call: no thread, no
//! timer and no audio device held open while nothing sounds.
use std::path::PathBuf;
use std::ptr::null_mut;

use windows_sys::Win32::Media::Audio::PlaySoundW;

use crate::config::SoundMode;

const SND_ASYNC: u32 = 0x0001;
const SND_NODEFAULT: u32 = 0x0002;
const SND_MEMORY: u32 = 0x0004;
const SND_FILENAME: u32 = 0x0002_0000;

static VOICE_125: &[u8] = include_bytes!("../sounds/125.wav");
static VOICE_250: &[u8] = include_bytes!("../sounds/250.wav");
static VOICE_500: &[u8] = include_bytes!("../sounds/500.wav");
static VOICE_1000: &[u8] = include_bytes!("../sounds/1000.wav");
static VOICE_2000: &[u8] = include_bytes!("../sounds/2000.wav");
static VOICE_4000: &[u8] = include_bytes!("../sounds/4000.wav");
static VOICE_8000: &[u8] = include_bytes!("../sounds/8000.wav");

static CHIME_125: &[u8] = include_bytes!("../sounds/chime_125.wav");
static CHIME_250: &[u8] = include_bytes!("../sounds/chime_250.wav");
static CHIME_500: &[u8] = include_bytes!("../sounds/chime_500.wav");
static CHIME_1000: &[u8] = include_bytes!("../sounds/chime_1000.wav");
static CHIME_2000: &[u8] = include_bytes!("../sounds/chime_2000.wav");
static CHIME_4000: &[u8] = include_bytes!("../sounds/chime_4000.wav");
static CHIME_8000: &[u8] = include_bytes!("../sounds/chime_8000.wav");

static ERROR: &[u8] = include_bytes!("../sounds/error.wav");
static NOTICE: &[u8] = include_bytes!("../sounds/notice.wav");

fn voice_clip(hz: u16) -> Option<&'static [u8]> {
    Some(match hz {
        125 => VOICE_125,
        250 => VOICE_250,
        500 => VOICE_500,
        1000 => VOICE_1000,
        2000 => VOICE_2000,
        4000 => VOICE_4000,
        8000 => VOICE_8000,
        _ => return None,
    })
}

fn chime_clip(hz: u16) -> &'static [u8] {
    match hz {
        125 => CHIME_125,
        250 => CHIME_250,
        500 => CHIME_500,
        1000 => CHIME_1000,
        2000 => CHIME_2000,
        4000 => CHIME_4000,
        _ => CHIME_8000,
    }
}

/// The mouse confirmed `hz`.
pub fn confirm(mode: SoundMode, hz: u16) {
    match mode {
        SoundMode::Off => {}
        SoundMode::Beep => play_memory(chime_clip(hz)),
        SoundMode::Voice => voice(hz),
    }
}

/// The rate could not be applied (mouse not answering, or no further step).
pub fn error(mode: SoundMode) {
    if mode != SoundMode::Off {
        play_memory(ERROR);
    }
}

/// Neutral acknowledgement that is not a rate (e.g. "apply the rules now").
pub fn notice(mode: SoundMode) {
    if mode != SoundMode::Off {
        play_memory(NOTICE);
    }
}

/// Play a WAV image that lives for the whole process.
fn play_memory(clip: &'static [u8]) {
    // SAFETY: with SND_MEMORY the first argument is a pointer to the WAV
    // image, not a string; `clip` is `'static`, so it stays valid for the
    // whole asynchronous playback. The module handle may be null.
    unsafe {
        PlaySoundW(
            clip.as_ptr() as *const u16,
            null_mut(),
            SND_MEMORY | SND_ASYNC | SND_NODEFAULT,
        )
    };
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
        unsafe {
            PlaySoundW(
                wpath.as_ptr(),
                null_mut(),
                SND_FILENAME | SND_ASYNC | SND_NODEFAULT,
            )
        };
    } else if let Some(clip) = voice_clip(hz) {
        play_memory(clip);
    } else {
        play_memory(chime_clip(hz));
    }
}
