# Ritmo

A complete DAW for Linux that runs on just a laptop, with sample chopping built in. Patterns and a playlist, a piano roll, a mixer and built-in instruments for any genre, plus a chop sampler: drop in any audio, chop it onto 16 pads, flip it and sequence it. Rust, egui, cpal. Early development; see [ROADMAP.md](ROADMAP.md).

## Install

- **AppImage** (any distribution with glibc 2.35 or newer: Ubuntu 22.04+, Fedora, Arch, Debian 12+, …): download `Ritmo-<version>-x86_64.AppImage` from the [releases](https://github.com/jerichosiahaya/ritmo/releases), make it executable and run it.
- **Arch Linux (AUR):** `ritmo` (built from source with [`packaging/aur/PKGBUILD`](packaging/aur/PKGBUILD)).
- **From source:** install Rust and the ALSA headers (`alsa-lib` on Arch, `alsa-lib-devel` on Fedora, `libasound2-dev` on Debian and Ubuntu), then

  ```sh
  cargo run --release
  ```

  To install it with its menu entry, icon, file type and demo projects (to `/usr/local` by default):

  ```sh
  cargo build --release && sudo packaging/install.sh
  ```

`ritmo song.ritmo` opens a project; `ritmo --version` prints the version. **File ▸ Open a demo** has three songs to start from: a lo-fi loop, a boom-bap beat made by chopping a break, and a house track.

## Layout

- **Toolbar:** play/stop, an LCD with bar:step and tempo, BPM, swing and master knobs, buttons to show and hide windows, and Export.
- **Browser (left):** instruments, chords and patterns. Click an instrument to edit it in the piano roll (drums: to play it from the keyboard). Click a chord to put it on the selected Playlist bar, or drag it onto any bar. Only that bar changes: if its part of the pattern also plays elsewhere, the bar gets a copy of its own first. Click a pattern to edit its steps; double-click opens it in the piano roll.
- **Playlist (F5):** tracks of pattern clips, as long as the song needs (up to 999 bars). Click an empty spot to place the selected pattern. Drag a clip to move it (to another track too), Ctrl+drag to copy it, drag either edge to resize it: a clip longer than its pattern repeats it. Ctrl+click selects several clips to move together; Delete removes the selection. Double-click a clip to edit its pattern; right-click to delete it, cut a bar out or put a chord on a bar. Clips snap to bars, beats or steps (hold Alt to place freely); a clip dropped on others cuts them away. Click the ruler to play from there (a hollow marker shows where; Stop returns to it, and clicking while playing jumps). Drag on the ruler to loop part of the song, which then plays from the loop's start; double-click it to clear the loop. Ctrl+scroll or −/+ zooms. Click a track name to mute it. Chord bars show the chord's name.
- **Channel Rack (F6):** for each channel, an on/off LED, pan and volume knobs, and step buttons for drums (in the pattern picked at the top); melodic channels show what they play across the song.
- **Piano Roll (F7):** one channel's notes in one pattern. Click to add a note (it gets the length of the last note you drew, resized or clicked); drag a note to move it, drag its right end to change its length, Ctrl+drag to copy it; Ctrl+drag on empty space selects a box, Ctrl+click adds to the selection; right-click or right-drag deletes. Notes snap to 1/4, 1/8, 1/16 or 1/32 (hold Alt to place freely). The lane underneath sets velocity: drag over a note's stem. Delete removes the selection, arrows move it (Shift+↑/↓ by an octave), Ctrl+A selects all, Ctrl+C / Ctrl+X / Ctrl+V copy, cut and paste (each paste goes after the last, a whole number of bars on), Ctrl+D duplicates. ▲/▼ show another octave; hold a key to hear it. Notes past the end of the pattern make it longer, in whole bars.
- **Sampler (F8):** chop any audio onto 16 pads. Load a WAV, FLAC, MP3, OGG or AIFF file (or drop it on the window): it becomes a chop channel, chopped at its hits. Re-chop at the hits (with a sensitivity slider), into 8 or 16 equal slices, or by tapping Enter while it plays. Drag chop points on the waveform, double-click to add one, right-click to remove one. Each pad has pitch (which changes speed, like a turntable), reverse and level; One-shot plays whole slices, Cut itself stops the previous chop. Chops are notes, so you sequence them in the Piano Roll (one row per pad) and Playlist like any instrument.
- **Mixer (F9):** a strip per mixer track plus Master, with faders and peak meters, and the lo-fi master insert (Tone, Crush, Wobble, Vinyl, Reverb).

Knobs: drag up/down, scroll, or use the arrow keys; double-click resets.

**Undo and redo** cover every edit (↶ ↷ in the toolbar, Ctrl+Z, Ctrl+Shift+Z or Ctrl+Y); a drag or a scroll is one step. **Patterns:** + New pattern in the browser or + in the Channel Rack; right-click a pattern to rename, clone or delete it. **Tracks:** + Track in the Playlist; right-click a track name to rename or delete it. **Channels:** right-click a channel's name in the Channel Rack to rename or delete it.

## Files

- **File** menu: New (empty or the demo song), Open, Save, Save as. Projects are `.ritmo` files (readable JSON). `ritmo song.ritmo` opens a file from the terminal.
- The title bar shows `•` when there are unsaved changes; New, Open and closing the window ask before discarding them.
- Ritmo autosaves every minute to `~/.local/share/ritmo/autosave.ritmo` (or `$XDG_DATA_HOME/ritmo/`). Open that file if Ritmo ever closes unexpectedly.
- Chop samples are referenced by their path, not copied into the project: keep the audio files where they are (copying them into the project folder is planned).
- Open and Save use your desktop's file dialog through xdg-desktop-portal (install `xdg-desktop-portal-gtk`, `-gnome` or `-kde`), or `zenity` if no portal is running.

## Keys

- `Z S X D C V G B H N J M` = C3 octave, `Q 2 W 3 E R 5 T 6 Y 7 U I 9 O 0 P` = C4 octave, played on the selected channel (outlined in the Channel Rack; the status bar names it).
- On a chop channel the same keys play pads 1–16: `Z S X D C V G B H N J M Q 2 W 3`. While tap-chopping, **Enter** taps and **Esc** finishes.
- **Space** = play/stop. **F5 / F6 / F7 / F8 / F9** = Playlist / Channel Rack / Piano Roll / Sampler / Mixer.
- **− / =** = typing keyboard an octave down / up (the status bar shows where it is).
- **Home** = play from the start again (of the loop, if there is one).
- **Delete** = remove the selected notes or clips (whichever window you clicked last). **Esc** = cancel a drag, or clear the selection.
- **♩ Click** in the toolbar = metronome (a click on every beat while playing, accented on the bar; never exported). **⚙ Audio** = output device and buffer size; settings are kept in `~/.config/ritmo/settings.json`.
- **Ctrl+N / Ctrl+O / Ctrl+S / Ctrl+Shift+S** = new / open / save / save as. **Ctrl+Z / Ctrl+Shift+Z (Ctrl+Y)** = undo / redo.
- **Export** writes `~/Music/<project name>.wav` (or `ritmo-export.wav` before the first save): the whole song once (ignoring the loop range) plus a 2 s tail.

Linux needs the ALSA dev package (`alsa-lib` on Arch, `libasound2-dev` on Debian/Ubuntu).

MIT licensed; see `LICENSE`.
