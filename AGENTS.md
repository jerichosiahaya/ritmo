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

- `src/engine.rs`: everything audio, with no I/O and no UI. It plays a `Snapshot` (channels, mixer tracks and notes placed in song time, in ticks) with the synths (drums, FM Rhodes, sine bass), the mixer (channels → mixer tracks → master) and the lo-fi master chain. `render_offline` and `export_wav` use the same engine as live playback.
- `src/project.rs`: the v0.2 project model (channels, mixer tracks, patterns with notes, playlist tracks and clips, all with stable IDs; time in ticks, `PPQ` = 96). `compile()` builds the engine's `Snapshot`; `validate()` checks every invariant (debug builds run it after every edit). Editing operations the windows use (`toggle_step`, `set_cell`, `stamp_chord`, `toggle_bar`, …) live here with tests, not in the UI. `Project::demo()` is the startup song. Keep `validate()` and its tests in step with any new field.
- `src/file.rs`: `.ritmo` project files: JSON with a `format` and `version`, written to a temp file and renamed into place, upgraded by `migrate` and checked with `validate()` on open; autosave goes to `$XDG_DATA_HOME/ritmo/autosave.ritmo`. `tests/files/` holds a fixture per format version.
- `src/sample.rs`: audio files for the chop sampler: `decode` (symphonia: WAV, FLAC, MP3, OGG, AIFF), `detect_hits` (level-jump onset detection), `even_points`, and `overview` for waveform drawing. Never called on the audio thread; the app decodes on a background thread and caches samples by path.
- `src/rt.rs`: the real-time guard. In debug and test builds it counts heap use inside `rt::enter()`/`rt::exit()` (the audio callback); the status bar shows a warning if it ever happens.
- `src/main.rs`: app state (`App`), the cpal audio stream, toolbar and File menu (new/open/save, the "Save changes?" prompt, autosave, system file dialogs via `rfd` on a background thread), browser panel, keyboard shortcuts.
- `src/windows.rs`: the floating windows (Playlist, Channel Rack, Piano Roll, Sampler, Mixer).
- `src/widgets.rs`: theme colors and custom widgets (knob, LED, step button, meter, LCD).

## How data flows

- The UI edits `app.project` and, whenever it changed since the last frame, compiles a `Snapshot` and sends it whole as `Cmd::Snapshot(Box<Snapshot>)`. Windows refer to channels, patterns and tracks by ID; the engine sees indices.
- The audio thread owns the `Engine` and drains the bounded queue from `engine::command_channel()` at the start of each callback. The UI sends with `try_send` and never blocks. Don't switch to an unbounded `mpsc::channel`: its receiver frees memory on the audio thread.
- Chop samples reach the engine as `Arc<SampleData>` inside the snapshot (`compile_with` looks them up in the app's cache). The engine only reads them; the last reference is always dropped on the UI thread with the retired snapshot. A chop voice whose sample changes in a new snapshot stops at once.
- The engine never drops a snapshot: it sends the one it replaces back through `engine::trash_channel()`, and the UI drops it each frame.
- The audio thread reports back only through atomics: `pos` (playing tick + 1, or 0 when stopped) and `meters` (f32 peak bits, merged with `fetch_max`).

## Rules

- **Audio thread:** never allocate, free, lock, log or do I/O inside the cpal callback or in `Engine::next_frame`. Allocate buffers in `Engine::new` (per-channel state is sized by `MAX_CHANNELS`/`MAX_MIXER`). Anything with heap data reaches the engine inside a `Snapshot` and leaves through the trash queue.
- The test `audio_path_never_allocates_or_frees` counts heap operations on the audio path; keep it passing.
- **Engine changes need a test** in `engine.rs` (render a few thousand frames and check levels, timing or state). Tests use low sample rates (8–22 kHz) to stay fast.
- **Project files:** every field in `Project` is saved. If you add, rename or change the meaning of one, bump `file::VERSION`, add an upgrade step to `file::migrate`, and run `cargo test -- --ignored write_fixture` to add the new fixture. Never edit or delete old fixtures: `old_files_still_open` must keep passing.
- **Chops:** pad `k` (from 0) is pitch `CHOP_BASE + k` (48, `Z`); at most `MAX_SLICES` (16) slices, end to end. Keep chop operations in `project.rs` with tests, like the other edits.
- **Sequencer timing:** ticks, `PPQ` = 96 per quarter note (`STEP` = 24 is a 16th). Swing lengthens the first 16th of each 8th-note pair and shortens the second, keeping the pair's length (`tick_len`). A note at tick `t` starts when the engine reaches `t`; notes end at their `end` tick; wrapping at the loop end releases held notes.
- **UI:** egui 0.37 / eframe 0.37 (`App::ui`, `egui::Panel`, `Ui::scope_id`). Use the colors in `widgets.rs` instead of new hard-coded ones. New controls need hover text, and custom widgets set `widget_info` for accessibility.
- **Original look:** the layout follows common pattern-DAW conventions; do not copy other products' logos, skins or branding.
- Keep changes small and in the existing structure. Do not add dependencies for things a few lines can do.

## Checking UI changes

`cargo test` does not cover drawing. After a UI change, run the app and look at it, or take a window screenshot (on niri: `niri msg action screenshot-window --id <id> --path <abs path>`).
