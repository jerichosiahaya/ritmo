# AGENTS.md

Guidance for coding agents working on Ritmo, a pattern-based music app (a small DAW) in Rust.

## Commands

```sh
cargo build            # debug build
cargo run --release    # run the app (release build gives clean audio)
cargo test             # unit tests (engine DSP, sequencer, widgets)
cargo clippy           # must stay at zero warnings
```

Linux needs the ALSA dev package (`alsa-lib` on Arch, `libasound2-dev` on Debian/Ubuntu).

## Layout

- `src/engine.rs`: everything audio, with no I/O and no UI. It holds the synths (drums, FM Rhodes, sine bass), the step and melody sequencer, the mixer (channels → buses → master) and the lo-fi master chain. `render_offline` and `export_wav` use the same engine as live playback.
- `src/main.rs`: app state (`App`), the cpal audio stream, toolbar, browser panel, keyboard shortcuts.
- `src/windows.rs`: the floating windows (Playlist, Channel Rack, Piano Roll, Mixer).
- `src/widgets.rs`: theme colors and custom widgets (knob, LED, step button, meter, LCD).

## How data flows

- `Song` is a plain `Copy` struct holding the whole project. The UI edits its own copy and sends `Cmd::Song(song)` by value whenever it changes.
- The audio thread owns the `Engine` and drains an `mpsc` channel at the start of each callback.
- The audio thread reports back only through atomics: `pos` (playing step + 1, or 0 when stopped) and `meters` (f32 peak bits, merged with `fetch_max`).

## Rules

- **Audio thread:** never allocate, lock, log or do I/O inside the cpal callback or in `Engine::next_frame`. Allocate buffers in `Engine::new`. Keep `Song` `Copy`: use fixed-size arrays (for example the `u64` bitmasks in `melody`), not `Vec`.
- **Engine changes need a test** in `engine.rs` (render a few thousand frames and check levels, timing or state). Tests use low sample rates (8–22 kHz) to stay fast.
- **Sequencer timing:** `BARS` × `STEPS` 16th steps; swing shortens odd steps and keeps each pair the same length (`step_len`).
- **UI:** egui 0.37 / eframe 0.37 (`App::ui`, `egui::Panel`, `Ui::scope_id`). Use the colors in `widgets.rs` instead of new hard-coded ones. New controls need hover text, and custom widgets set `widget_info` for accessibility.
- **Original look:** the layout follows common pattern-DAW conventions; do not copy other products' logos, skins or branding.
- Keep changes small and in the existing structure. Do not add dependencies for things a few lines can do.

## Checking UI changes

`cargo test` does not cover drawing. After a UI change, run the app and look at it, or take a window screenshot (on niri: `niri msg action screenshot-window --id <id> --path <abs path>`).
