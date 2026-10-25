# Changelog

All notable changes to this project are documented here.
Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow [SemVer](https://semver.org/).

## [Unreleased]

### Added
- **Per-game polling rates, hotkeys and sound feedback**, configured in `mousebatt.ini`
  next to the exe (see `mousebatt.example.ini`; "Edit mousebatt.ini…" in the menu creates it):
  - `[apps]` rules and a `default` rate. A listed program counts as running from the first
    time its window gets the focus until it exits; its rate holds whatever has the focus, so
    alt-tabbing writes nothing to the mouse. Driven by Windows focus-change events
    (`SetWinEventHook`, out of context): no timer, no scanning, nothing injected into other
    programs. One process snapshot at startup / reload finds games already running. The hook
    only exists while the file has rules.
  - Global hotkeys (`RegisterHotKey`, no keyboard hook): set a rate, step up/down, toggle two
    rates, or apply the rules now. A manual pick holds until a listed program starts or closes.
  - Every change is read back from the mouse and re-sent if it didn't take (3 tries), with
    retries after 3 s / 10 s / 30 s when the mouse is asleep. `enforce = true` also puts the
    wanted rate back when the periodic check finds another one.
  - Optional sound once the mouse has confirmed the rate: a soft bell note or the spoken
    number (built-in English clips, overridable with `sounds\<hz>.wav`).
- **Games recognised by their install folder**: `game_rate` (and optional `game_folders`) give
  every program installed under a Steam / Epic / GOG / Xbox / Riot / Ubisoft / EA folder that
  rate when it takes the focus, so only the exceptions need to be listed in `[apps]`.
  Launchers are ignored; a program listed in `[apps]` always wins. No scanning: the install
  path comes with the focus event that is already handled.
- `pause_in_game` (Settings > Options): while a game runs, the periodic and device-change reads
  of the mouse are skipped, so nothing touches the mouse during play; one read catches up when
  the last game ends. Rate changes are never skipped.
- Tray menu: "Icon shows > Battery level / Polling rate / Both" (1K, 2K, 4K, 8K, 500, ...;
  "Both" stacks battery % over the rate in smaller digits) and a clearer "Switch rate
  automatically when a game runs" switch.
- **Settings window** (tray menu > Settings…): desktop rate, games with their rates (picked from
  the programs running now, or typed), any number of hotkeys (a list with + Add / − Remove, the
  keys entered by pressing them), sound and enforce options. Save rewrites `mousebatt.ini` (comments of a hand-edited file are not kept;
  presets and the delay are) and reloads it; the real hotkeys are suspended while it is open.
  A small `build.rs` embeds `mousebatt.manifest` so the window uses the modern control style.
- The poll worker clears its in-flight flag before posting its result, so a rate requested
  while a poll was running can no longer be dropped.
- **Battery icon** submenu in the right-click menu: show a small battery glyph, filled to
  the charge level, above or below the number, or hide it (the default). The choice is
  saved under `HKCU\Software\mousebatt`.

### Changed
- The icon is drawn at the tray's actual size (16 px at 100% scaling, 20/24 px when
  scaled; the process is now DPI aware) instead of a 32 px bitmap Windows shrinks.
- Digits are rendered like the taskbar clock: Segoe UI with the system's font smoothing
  (ClearType by default), blended against the taskbar colour next to the tray.
- On a light taskbar the icon uses dark colours (Windows light palette) instead of white,
  and it redraws when switching between light and dark mode.

## [0.2.0] - 2026-09-12

### Added
- **Polling rate** submenu in the right-click menu; the current rate is checked and also
  shown in the tooltip. Only rates the current link supports are listed:
  - Pulsar: 125 Hz–8 kHz (1 kHz max on cable, up to 8 kHz on the 8K dongle)
  - VAXEE: 500 Hz–4 kHz on the VXD02 4K dongle (8 kHz on the 8K receiver), 1 kHz max on
    cable or in a "Standard" tracking mode, matching the VAXEE Control Center

## [0.1.0] - 2026-08-23

### Added
- Tray icon showing battery percentage for Pulsar (X3 family, 8K Dongle Gen.2) and VAXEE (XE-S, 4K dongle) mice
- Colour states: normal / charging / low (≤20%) / stale
- Tooltip with model, percentage, charging state and voltage (Pulsar)
- Re-read on device plug/unplug and on resume from sleep; left-click to refresh
- "Start with Windows" toggle in the right-click menu
- Single ~290 KB exe with no dependencies beyond `windows-sys`

[0.2.0]: https://github.com/ryanlewis/mousebatt/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/ryanlewis/mousebatt/releases/tag/v0.1.0
