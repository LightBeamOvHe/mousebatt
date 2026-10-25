//! `mousebatt.ini` (next to the exe): per-program polling rates, hotkeys and
//! sound feedback. Hand-rolled INI parser, so no extra dependency. A missing
//! file gives the plain defaults, which behave exactly like mousebatt without
//! this feature: no hook, no hotkeys, no automatic change.
//!
//! The parser is pure (text in, `Config` out) and unit-tested; problems are
//! collected in `Config::warnings` instead of aborting, so one typo never
//! disables the rest of the file.
use std::collections::HashMap;
use std::path::PathBuf;

use crate::protocol::PULSAR_RATES;

/// Where games usually live; a program whose path contains one of these is
/// treated as a game (when `game_rate` is set). Lower-case.
pub const DEFAULT_GAME_FOLDERS: &[&str] = &[
    "\\steamapps\\common\\",
    "\\epic games\\",
    "\\gog galaxy\\games\\",
    "\\gog games\\",
    "\\xboxgames\\",
    "\\riot games\\",
    "\\ubisoft game launcher\\games\\",
    "\\ea games\\",
];

/// Launchers and helpers that sit in those folders (or next to them) but are
/// never the game. An explicit `[apps]` rule still wins over this list.
const LAUNCHERS: &[&str] = &[
    "steam.exe",
    "steamwebhelper.exe",
    "epicgameslauncher.exe",
    "epicwebhelper.exe",
    "galaxyclient.exe",
    "eadesktop.exe",
    "eabackgroundservice.exe",
    "battle.net.exe",
    "upc.exe",
    "ubisoftconnect.exe",
    "riotclientservices.exe",
    "riotclientux.exe",
    "riotclientuxrender.exe",
];

/// Commented example written by "Edit mousebatt.ini" when no file exists yet.
pub const TEMPLATE: &str = include_str!("../mousebatt.example.ini");

const MOD_ALT: u32 = 0x1;
const MOD_CONTROL: u32 = 0x2;
const MOD_SHIFT: u32 = 0x4;
const MOD_WIN: u32 = 0x8;

/// Rate for a program that has no rule of its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fallback {
    /// Never change the rate on our own for unlisted programs.
    Keep,
    Rate(u16),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SoundMode {
    Off,
    /// A soft bell note whose pitch rises with the rate.
    Beep,
    /// Spoken rate ("two thousand"); `sounds\<hz>.wav` next to the exe
    /// overrides the built-in clip.
    Voice,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Set(u16),
    Up,
    Down,
    Toggle(u16, u16),
    /// Apply the rate the program rules want right now (undoes a manual pick).
    Auto,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hotkey {
    pub mods: u32,
    pub vk: u32,
    pub action: Action,
    /// The combination as written in the file, for messages.
    pub label: String,
}

#[derive(Clone, Debug)]
pub struct Config {
    pub default: Fallback,
    /// Wait this long after a game starts or stops before writing, so a
    /// short-lived process or a crash-and-relaunch doesn't flap the rate.
    pub delay_ms: u32,
    /// Re-apply the wanted rate when a periodic check finds another one.
    pub enforce: bool,
    pub sound: SoundMode,
    /// Also play the sound for changes made by the per-program rules.
    pub sound_auto: bool,
    /// While a game runs, skip the periodic and device-change reads of the
    /// mouse, so nothing at all is done during play (the battery display
    /// simply stays as it was until the game ends).
    pub pause_in_game: bool,
    /// Rate for a program recognised as a game by where it is installed but
    /// not listed in `apps`; `None` = don't recognise games.
    pub game_rate: Option<u16>,
    /// Folder fragments (lower-case, backslashes) that mark a game; empty =
    /// `DEFAULT_GAME_FOLDERS`.
    pub game_folders: Vec<String>,
    /// Lower-cased program file name -> rate.
    pub apps: HashMap<String, u16>,
    /// Named rates, sorted by name. Only read by hand-edited files, but kept
    /// when the settings window rewrites the file.
    pub presets: Vec<(String, u16)>,
    pub hotkeys: Vec<Hotkey>,
    pub warnings: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            default: Fallback::Keep,
            delay_ms: 1500,
            enforce: false,
            sound: SoundMode::Off,
            sound_auto: false,
            pause_in_game: false,
            game_rate: None,
            game_folders: Vec::new(),
            apps: HashMap::new(),
            presets: Vec::new(),
            hotkeys: Vec::new(),
            warnings: Vec::new(),
        }
    }
}

pub fn path() -> PathBuf {
    match std::env::current_exe() {
        Ok(p) => p.with_file_name("mousebatt.ini"),
        Err(_) => PathBuf::from("mousebatt.ini"),
    }
}

/// Read `mousebatt.ini`; a missing file is not an error.
pub fn load() -> Config {
    match std::fs::read_to_string(path()) {
        Ok(text) => Config::parse(&text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Config::default(),
        Err(e) => {
            let mut cfg = Config::default();
            cfg.warnings.push(format!(
                "Cannot read mousebatt.ini ({e}). Save it as UTF-8 text."
            ));
            cfg
        }
    }
}

impl Config {
    /// True when something in the file needs the foreground hook.
    pub fn has_rules(&self) -> bool {
        !self.apps.is_empty() || self.default != Fallback::Keep || self.game_rate.is_some()
    }

    /// Is the program at `path` (lower-case) named `exe` (lower-case) a game
    /// by where it is installed? Always false while `game_rate` is off.
    pub fn is_game_path(&self, path: &str, exe: &str) -> bool {
        if self.game_rate.is_none() || LAUNCHERS.contains(&exe) {
            return false;
        }
        if self.game_folders.is_empty() {
            DEFAULT_GAME_FOLDERS.iter().any(|f| path.contains(f))
        } else {
            self.game_folders.iter().any(|f| path.contains(f.as_str()))
        }
    }

    /// The rate the rules want while the program `exe` (lower-case file name)
    /// runs: its own rule, else the rate for games recognised by their folder,
    /// else the default.
    pub fn wanted_for(&self, exe: &str) -> Option<u16> {
        if let Some(&hz) = self.apps.get(exe) {
            return Some(hz);
        }
        self.game_rate.or_else(|| self.default_rate())
    }

    /// The rate when no listed program is running; `None` = leave the mouse
    /// alone.
    pub fn default_rate(&self) -> Option<u16> {
        match self.default {
            Fallback::Keep => None,
            Fallback::Rate(hz) => Some(hz),
        }
    }

    pub fn parse(text: &str) -> Config {
        let mut cfg = Config::default();
        // (line number, section, key, value)
        let mut entries: Vec<(usize, String, String, String)> = Vec::new();
        let mut section = String::new();
        for (i, raw) in text.trim_start_matches('\u{feff}').lines().enumerate() {
            let line = strip_comment(raw).trim();
            if line.is_empty() {
                continue;
            }
            if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                section = name.trim().to_ascii_lowercase();
                continue;
            }
            match line.split_once('=') {
                Some((k, v)) => entries.push((
                    i + 1,
                    section.clone(),
                    k.trim().to_string(),
                    v.trim().to_string(),
                )),
                None => cfg.warn(i + 1, "expected `name = value`"),
            }
        }

        // Presets first, so every other section can refer to them by name
        // whatever the order in the file.
        let mut presets: HashMap<String, u16> = HashMap::new();
        for (n, sec, k, v) in &entries {
            if sec == "presets" {
                let parsed = parse_rate(v, &presets);
                match parsed {
                    Some(hz) => {
                        presets.insert(k.to_ascii_lowercase(), hz);
                    }
                    None => cfg.warn(*n, &format!("`{v}` is not a supported rate")),
                }
            }
        }

        cfg.presets = presets.iter().map(|(k, &v)| (k.clone(), v)).collect();
        cfg.presets.sort();

        for (n, sec, k, v) in &entries {
            match sec.as_str() {
                "presets" => {}
                "general" => cfg.set_general(*n, k, v, &presets),
                "apps" => match parse_rate(v, &presets) {
                    Some(hz) => {
                        cfg.apps.insert(normalize_exe(k), hz);
                    }
                    None => cfg.warn(*n, &format!("`{v}` is not a supported rate or preset")),
                },
                "hotkeys" => match (parse_hotkey(k), parse_action(v, &presets)) {
                    (Ok((mods, vk)), Some(action)) => cfg.hotkeys.push(Hotkey {
                        mods,
                        vk,
                        action,
                        label: k.clone(),
                    }),
                    (Err(why), _) => cfg.warn(*n, &format!("hotkey `{k}`: {why}")),
                    (_, None) => cfg.warn(*n, &format!("`{v}` is not a valid action")),
                },
                "" => cfg.warn(*n, "this line is before any [section]"),
                other => cfg.warn(*n, &format!("unknown section [{other}]")),
            }
        }
        cfg
    }

    fn warn(&mut self, line: usize, msg: &str) {
        self.warnings.push(format!("line {line}: {msg}"));
    }

    fn set_general(&mut self, n: usize, key: &str, value: &str, presets: &HashMap<String, u16>) {
        let lower = value.to_ascii_lowercase();
        match key.to_ascii_lowercase().as_str() {
            "default" => {
                if lower == "keep" {
                    self.default = Fallback::Keep;
                } else if let Some(hz) = parse_rate(value, presets) {
                    self.default = Fallback::Rate(hz);
                } else {
                    self.warn(n, "default must be `keep`, a rate or a preset");
                }
            }
            "delay_ms" => match value.parse::<u32>() {
                Ok(ms) if (100..=60_000).contains(&ms) => self.delay_ms = ms,
                _ => self.warn(n, "delay_ms must be between 100 and 60000"),
            },
            "enforce" => match parse_bool(&lower) {
                Some(b) => self.enforce = b,
                None => self.warn(n, "enforce must be true or false"),
            },
            "sound" => match lower.as_str() {
                "off" | "none" => self.sound = SoundMode::Off,
                "beep" | "chime" => self.sound = SoundMode::Beep,
                "voice" => self.sound = SoundMode::Voice,
                _ => self.warn(n, "sound must be off, beep or voice"),
            },
            "sound_auto" => match parse_bool(&lower) {
                Some(b) => self.sound_auto = b,
                None => self.warn(n, "sound_auto must be true or false"),
            },
            "pause_in_game" => match parse_bool(&lower) {
                Some(b) => self.pause_in_game = b,
                None => self.warn(n, "pause_in_game must be true or false"),
            },
            "game_rate" => {
                if matches!(lower.as_str(), "off" | "none") {
                    self.game_rate = None;
                } else if let Some(hz) = parse_rate(value, presets) {
                    self.game_rate = Some(hz);
                } else {
                    self.warn(n, "game_rate must be `off`, a rate or a preset");
                }
            }
            "game_folders" => {
                self.game_folders = value
                    .split('|')
                    .map(|f| f.trim().to_lowercase().replace('/', "\\"))
                    .filter(|f| !f.is_empty())
                    .collect();
            }
            other => self.warn(n, &format!("unknown setting `{other}`")),
        }
    }
}

/// Write `cfg` to `mousebatt.ini`, replacing the file.
pub fn save(cfg: &Config) -> std::io::Result<()> {
    std::fs::write(path(), cfg.to_ini())
}

impl Config {
    /// The file the settings window writes. It parses back to the same
    /// settings; comments of a hand-edited file are not kept.
    pub fn to_ini(&self) -> String {
        let mut out = String::new();
        out.push_str(
            "# mousebatt.ini - written by the settings window (tray icon > Settings...).\n\
             # You can still edit it by hand, but the window rewrites the file when it\n\
             # saves and does not keep comments. mousebatt.example.ini lists every option.\n\n",
        );
        out.push_str("[general]\n");
        match self.default {
            Fallback::Keep => out.push_str("default = keep\n"),
            Fallback::Rate(hz) => out.push_str(&format!("default = {hz}\n")),
        }
        out.push_str(&format!("delay_ms = {}\n", self.delay_ms));
        out.push_str(&format!("enforce = {}\n", self.enforce));
        out.push_str(match self.sound {
            SoundMode::Off => "sound = off\n",
            SoundMode::Beep => "sound = beep\n",
            SoundMode::Voice => "sound = voice\n",
        });
        out.push_str(&format!("sound_auto = {}\n", self.sound_auto));
        out.push_str(&format!("pause_in_game = {}\n", self.pause_in_game));
        match self.game_rate {
            None => out.push_str("game_rate = off\n"),
            Some(hz) => out.push_str(&format!("game_rate = {hz}\n")),
        }
        if !self.game_folders.is_empty() {
            out.push_str(&format!("game_folders = {}\n", self.game_folders.join(" | ")));
        }
        if !self.presets.is_empty() {
            out.push_str("\n[presets]\n");
            for (name, hz) in &self.presets {
                out.push_str(&format!("{name} = {hz}\n"));
            }
        }
        out.push_str("\n[apps]\n");
        let mut apps: Vec<(&String, &u16)> = self.apps.iter().collect();
        apps.sort();
        for (name, hz) in apps {
            out.push_str(&format!("{name} = {hz}\n"));
        }
        out.push_str("\n[hotkeys]\n");
        for h in &self.hotkeys {
            let combo = fmt_hotkey(h.mods, h.vk).unwrap_or_else(|| h.label.clone());
            out.push_str(&format!("{combo} = {}\n", fmt_action(&h.action)));
        }
        out
    }
}

/// The action as written in the file (`set 2000`, `toggle 1000 2000`, ...).
pub fn fmt_action(action: &Action) -> String {
    match action {
        Action::Set(hz) => format!("set {hz}"),
        Action::Up => "up".into(),
        Action::Down => "down".into(),
        Action::Toggle(a, b) => format!("toggle {a} {b}"),
        Action::Auto => "auto".into(),
    }
}

/// `Ctrl+Shift+P` for modifier bits and a virtual-key code; `None` if the key
/// has no name `parse_hotkey` understands.
pub fn fmt_hotkey(mods: u32, vk: u32) -> Option<String> {
    let key = vk_name(vk)?;
    let mut out = String::new();
    for (bit, name) in [
        (MOD_CONTROL, "Ctrl"),
        (MOD_SHIFT, "Shift"),
        (MOD_ALT, "Alt"),
        (MOD_WIN, "Win"),
    ] {
        if mods & bit != 0 {
            out.push_str(name);
            out.push('+');
        }
    }
    out.push_str(&key);
    Some(out)
}

fn vk_name(vk: u32) -> Option<String> {
    let named = match vk {
        0x20 => "Space",
        0x09 => "Tab",
        0x0D => "Enter",
        0x1B => "Esc",
        0x21 => "PageUp",
        0x22 => "PageDown",
        0x23 => "End",
        0x24 => "Home",
        0x25 => "Left",
        0x26 => "Up",
        0x27 => "Right",
        0x28 => "Down",
        0x2D => "Insert",
        0x2E => "Delete",
        0x30..=0x39 | 0x41..=0x5A => return char::from_u32(vk).map(|c| c.to_string()),
        0x60..=0x69 => return Some(format!("Numpad{}", vk - 0x60)),
        0x70..=0x87 => return Some(format!("F{}", vk - 0x70 + 1)),
        _ => return None,
    };
    Some(named.to_string())
}

/// Drop everything from the first `#` or `;`.
fn strip_comment(s: &str) -> &str {
    match s.find(['#', ';']) {
        Some(i) => &s[..i],
        None => s,
    }
}

/// Program names are matched case-insensitively on the bare file name;
/// `.exe` is added when left out.
pub fn normalize_exe(name: &str) -> String {
    let lower = name.trim().to_lowercase();
    if lower.contains('.') {
        lower
    } else {
        format!("{lower}.exe")
    }
}

fn parse_bool(lower: &str) -> Option<bool> {
    match lower {
        "true" | "yes" | "on" | "1" => Some(true),
        "false" | "no" | "off" | "0" => Some(false),
        _ => None,
    }
}

/// `2000`, `2000hz`, `2 kHz`, `2k` or the name of a preset.
fn parse_rate(value: &str, presets: &HashMap<String, u16>) -> Option<u16> {
    let v = value.trim().to_ascii_lowercase();
    if let Some(&hz) = presets.get(&v) {
        return Some(hz);
    }
    let v = v.strip_suffix("hz").unwrap_or(&v).trim();
    let hz: u32 = match v.strip_suffix('k') {
        Some(k) => k.trim().parse::<u32>().ok()?.checked_mul(1000)?,
        None => v.parse().ok()?,
    };
    let hz = u16::try_from(hz).ok()?;
    PULSAR_RATES.contains(&hz).then_some(hz)
}

fn parse_action(value: &str, presets: &HashMap<String, u16>) -> Option<Action> {
    let lower = value.to_ascii_lowercase();
    let mut words = lower
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|w| !w.is_empty());
    let head = words.next()?;
    let rest: Vec<&str> = words.collect();
    match (head, rest.as_slice()) {
        ("up", []) => Some(Action::Up),
        ("down", []) => Some(Action::Down),
        ("auto", []) => Some(Action::Auto),
        ("set", [r]) => parse_rate(r, presets).map(Action::Set),
        ("toggle", [a, b]) => Some(Action::Toggle(
            parse_rate(a, presets)?,
            parse_rate(b, presets)?,
        )),
        // `Ctrl+Shift+P = 2000` is shorthand for `set 2000`.
        (r, []) => parse_rate(r, presets).map(Action::Set),
        _ => None,
    }
}

/// `Ctrl+Shift+P` -> (modifier bits, virtual-key code). At least one modifier
/// is required: a bare letter registered system-wide would break typing.
fn parse_hotkey(combo: &str) -> Result<(u32, u32), String> {
    let mut mods = 0;
    let mut vk = None;
    for part in combo.split('+').map(|p| p.trim().to_ascii_lowercase()) {
        match part.as_str() {
            "ctrl" | "control" => mods |= MOD_CONTROL,
            "shift" => mods |= MOD_SHIFT,
            "alt" => mods |= MOD_ALT,
            "win" | "windows" => mods |= MOD_WIN,
            key => {
                if vk.is_some() {
                    return Err("only one key is allowed".into());
                }
                vk = Some(parse_vk(key).ok_or_else(|| format!("unknown key `{key}`"))?);
            }
        }
    }
    let vk = vk.ok_or("no key given")?;
    if mods == 0 {
        return Err("needs Ctrl, Shift, Alt or Win".into());
    }
    Ok((mods, vk))
}

/// Letters, digits, F1-F24, numpad digits, arrows and a few named keys.
fn parse_vk(key: &str) -> Option<u32> {
    let mut chars = key.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        if c.is_ascii_lowercase() {
            return Some(c.to_ascii_uppercase() as u32);
        }
        if c.is_ascii_digit() {
            return Some(c as u32);
        }
        return None;
    }
    if let Some(n) = key.strip_prefix('f').and_then(|n| n.parse::<u32>().ok()) {
        return (1..=24).contains(&n).then(|| 0x70 + n - 1);
    }
    if let Some(n) = key.strip_prefix("numpad").and_then(|n| n.parse::<u32>().ok()) {
        return (n <= 9).then(|| 0x60 + n);
    }
    Some(match key {
        "space" => 0x20,
        "tab" => 0x09,
        "enter" | "return" => 0x0D,
        "esc" | "escape" => 0x1B,
        "pageup" => 0x21,
        "pagedown" => 0x22,
        "end" => 0x23,
        "home" => 0x24,
        "left" => 0x25,
        "up" => 0x26,
        "right" => 0x27,
        "down" => 0x28,
        "insert" => 0x2D,
        "delete" | "del" => 0x2E,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
# comment
[general]
default = 1000      ; inline comment
delay_ms = 2000
enforce = yes
sound = voice
sound_auto = true
pause_in_game = true
game_rate = 1000
game_folders = \\steamapps\\common\\ | d:\\games\\

[presets]
fast = 2000
eco = 500

[apps]
Discovery.exe = fast
notepad = eco

[hotkeys]
Ctrl+Shift+P = 2000
ctrl+shift+o = down
Ctrl+Alt+T = toggle 1000 fast
Ctrl+Shift+A = auto
";

    #[test]
    fn parses_a_full_file() {
        let cfg = Config::parse(SAMPLE);
        assert!(cfg.warnings.is_empty(), "{:?}", cfg.warnings);
        assert_eq!(cfg.default, Fallback::Rate(1000));
        assert_eq!(cfg.delay_ms, 2000);
        assert!(cfg.enforce);
        assert_eq!(cfg.sound, SoundMode::Voice);
        assert!(cfg.sound_auto);
        assert_eq!(cfg.apps.get("discovery.exe"), Some(&2000));
        assert_eq!(cfg.apps.get("notepad.exe"), Some(&500));
        assert_eq!(cfg.hotkeys.len(), 4);
        assert_eq!(cfg.hotkeys[0].action, Action::Set(2000));
        assert_eq!(cfg.hotkeys[0].mods, MOD_CONTROL | MOD_SHIFT);
        assert_eq!(cfg.hotkeys[0].vk, 'P' as u32);
        assert_eq!(cfg.hotkeys[1].action, Action::Down);
        assert_eq!(cfg.hotkeys[2].action, Action::Toggle(1000, 2000));
        assert_eq!(cfg.hotkeys[3].action, Action::Auto);
    }

    #[test]
    fn empty_file_is_inert() {
        let cfg = Config::parse("");
        assert!(!cfg.has_rules());
        assert!(cfg.hotkeys.is_empty());
        assert!(cfg.warnings.is_empty());
        assert_eq!(cfg.wanted_for("anything.exe"), None);
    }

    #[test]
    fn wanted_for_uses_app_rule_then_default() {
        let cfg = Config::parse("[general]\ndefault = 500\n[apps]\ngame.exe = 2000\n");
        assert_eq!(cfg.wanted_for("game.exe"), Some(2000));
        assert_eq!(cfg.wanted_for("browser.exe"), Some(500));
        let keep = Config::parse("[apps]\ngame.exe = 2000\n");
        assert_eq!(keep.wanted_for("browser.exe"), None);
        assert!(keep.has_rules());
    }

    #[test]
    fn game_detection_by_folder() {
        let cfg = Config::parse("[general]\ngame_rate = 1000\n");
        assert!(cfg.is_game_path(r"d:\steamlibrary\steamapps\common\hades\hades.exe", "hades.exe"));
        assert!(cfg.is_game_path(
            r"c:\program files\epic games\fortnite\fortniteclient.exe",
            "fortniteclient.exe"
        ));
        assert!(!cfg.is_game_path(
            r"c:\program files (x86)\epic games\launcher\portal\epicgameslauncher.exe",
            "epicgameslauncher.exe"
        ));
        assert!(!cfg.is_game_path(r"c:\program files\mozilla firefox\firefox.exe", "firefox.exe"));
        // Off unless a game rate is set.
        let off = Config::parse("");
        assert!(!off.is_game_path(r"d:\steamapps\common\x\x.exe", "x.exe"));
        // A custom folder list replaces the built-in one.
        let custom = Config::parse("[general]\ngame_rate = 500\ngame_folders = D:\\Games\\ | /Emulators/\n");
        assert!(custom.is_game_path(r"d:\games\foo\foo.exe", "foo.exe"));
        assert!(custom.is_game_path(r"c:\emulators\bar\bar.exe", "bar.exe"));
        assert!(!custom.is_game_path(r"d:\steamapps\common\x\x.exe", "x.exe"));
    }

    #[test]
    fn detected_games_get_the_game_rate() {
        let cfg = Config::parse(
            "[general]\ndefault = 500\ngame_rate = 1000\n[apps]\nthefinals.exe = 2000\n",
        );
        assert_eq!(cfg.wanted_for("thefinals.exe"), Some(2000));
        assert_eq!(cfg.wanted_for("hades.exe"), Some(1000));
        assert_eq!(cfg.default_rate(), Some(500));
        assert!(Config::parse("[general]\ngame_rate = 1000\n").has_rules());
    }

    #[test]
    fn default_rate_follows_the_default_setting() {
        assert_eq!(Config::parse("[general]\ndefault = 2k\n").default_rate(), Some(2000));
        assert_eq!(Config::parse("[general]\ndefault = keep\n").default_rate(), None);
        assert_eq!(Config::parse("").default_rate(), None);
    }

    #[test]
    fn rate_spellings() {
        let none = HashMap::new();
        assert_eq!(parse_rate("1000", &none), Some(1000));
        assert_eq!(parse_rate("2000 Hz", &none), Some(2000));
        assert_eq!(parse_rate("2k", &none), Some(2000));
        assert_eq!(parse_rate("8 kHz", &none), Some(8000));
        assert_eq!(parse_rate("1500", &none), None);
        assert_eq!(parse_rate("fast", &none), None);
    }

    #[test]
    fn bad_lines_become_warnings_and_do_not_stop_the_rest() {
        let cfg = Config::parse(
            "[apps]\ngood.exe = 2000\nbad.exe = 1234\n[hotkeys]\nP = 2000\nCtrl+Shift+Q = nonsense\n[wat]\nx = 1\n",
        );
        assert_eq!(cfg.apps.get("good.exe"), Some(&2000));
        assert!(!cfg.apps.contains_key("bad.exe"));
        assert!(cfg.hotkeys.is_empty());
        assert_eq!(cfg.warnings.len(), 4, "{:?}", cfg.warnings);
    }

    #[test]
    fn hotkey_keys() {
        assert_eq!(parse_hotkey("Ctrl+F5"), Ok((MOD_CONTROL, 0x74)));
        assert_eq!(parse_hotkey("Alt+Numpad3"), Ok((MOD_ALT, 0x63)));
        assert_eq!(parse_hotkey("Win+Shift+Up"), Ok((MOD_WIN | MOD_SHIFT, 0x26)));
        assert!(parse_hotkey("F5").is_err(), "needs a modifier");
        assert!(parse_hotkey("Ctrl+A+B").is_err());
        assert!(parse_hotkey("Ctrl+Shift").is_err());
    }

    #[test]
    fn ini_round_trips() {
        let cfg = Config::parse(SAMPLE);
        let again = Config::parse(&cfg.to_ini());
        assert!(again.warnings.is_empty(), "{:?}", again.warnings);
        assert_eq!(again.default, cfg.default);
        assert_eq!(again.delay_ms, cfg.delay_ms);
        assert_eq!(again.enforce, cfg.enforce);
        assert_eq!(again.sound, cfg.sound);
        assert_eq!(again.sound_auto, cfg.sound_auto);
        assert!(again.pause_in_game && cfg.pause_in_game);
        assert_eq!(again.game_rate, cfg.game_rate);
        assert_eq!(again.game_folders, cfg.game_folders);
        assert_eq!(again.apps, cfg.apps);
        assert_eq!(again.presets, cfg.presets);
        assert_eq!(again.hotkeys.len(), cfg.hotkeys.len());
        for (a, b) in again.hotkeys.iter().zip(&cfg.hotkeys) {
            assert_eq!((a.mods, a.vk, &a.action), (b.mods, b.vk, &b.action));
        }
    }

    #[test]
    fn hotkey_names_round_trip() {
        for combo in ["Ctrl+Shift+P", "Alt+F12", "Ctrl+Numpad7", "Win+Left", "Ctrl+5"] {
            let (mods, vk) = parse_hotkey(combo).unwrap();
            assert_eq!(fmt_hotkey(mods, vk).as_deref(), Some(combo));
        }
        assert_eq!(fmt_hotkey(MOD_CONTROL, 0xFF), None);
    }

    #[test]
    fn exe_names_are_normalised() {
        assert_eq!(normalize_exe("Discovery.exe"), "discovery.exe");
        assert_eq!(normalize_exe("  Notepad "), "notepad.exe");
    }
}
