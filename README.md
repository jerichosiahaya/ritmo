# Ritmo

A complete DAW for Linux that runs on just a laptop, with sample chopping built in. Patterns and a playlist, a piano roll, a mixer and built-in instruments for any genre, plus a chop sampler: drop in any audio, chop it onto 16 pads, flip it and sequence it. Rust, egui, cpal. Early development; see [ROADMAP.md](ROADMAP.md).

```sh
cargo run --release
```

## Layout

- **Toolbar:** play/stop, an LCD with bar:step and tempo, BPM, swing and master knobs, buttons to show and hide windows, and Export.
- **Browser (left):** instruments, chords and patterns. Click an instrument to edit it in the piano roll (drums: to play it from the keyboard). Click a chord to put it on the selected Playlist bar, or drag it onto any bar. Click a pattern to edit its steps; double-click opens it in the piano roll.
- **Playlist (F5):** tracks of pattern clips. Click an empty bar to place the track's pattern there; click a clip to select a bar, double-click to edit it, right-click to put a chord on that bar or cut the bar out. Click a track name to mute it. Chord bars show the chord's name.
- **Channel Rack (F6):** for each channel, an on/off LED, pan and volume knobs, and step buttons for drums (in the pattern picked at the top); melodic channels show what they play across the song.
- **Piano Roll (F7):** one channel's notes in one pattern, on a 16th grid. Left-drag draws (cells next to a note join it), right-drag erases, ▲/▼ change octave, click a key to hear it.
- **Mixer (F9):** a strip per mixer track plus Master, with faders and peak meters, and the lo-fi master insert (Tone, Crush, Wobble, Vinyl, Reverb).

Knobs: drag up/down, scroll, or use the arrow keys; double-click resets.

## Files

- **File** menu: New (empty or the demo song), Open, Save, Save as. Projects are `.ritmo` files (readable JSON). `ritmo song.ritmo` opens a file from the terminal.
- The title bar shows `•` when there are unsaved changes; New, Open and closing the window ask before discarding them.
- Ritmo autosaves every minute to `~/.local/share/ritmo/autosave.ritmo` (or `$XDG_DATA_HOME/ritmo/`). Open that file if Ritmo ever closes unexpectedly.
- Open and Save use your desktop's file dialog through xdg-desktop-portal (install `xdg-desktop-portal-gtk`, `-gnome` or `-kde`), or `zenity` if no portal is running.

## Keys

- `Z S X D C V G B H N J M` = C3 octave, `Q 2 W 3 E R 5 T 6 Y 7 U I 9 O 0 P` = C4 octave, played on the selected channel (outlined in the Channel Rack; the status bar names it).
- **Space** = play/stop. **F5 / F6 / F7 / F9** = Playlist / Channel Rack / Piano Roll / Mixer.
- **Ctrl+N / Ctrl+O / Ctrl+S / Ctrl+Shift+S** = new / open / save / save as.
- **Export** writes `~/Music/<project name>.wav` (or `ritmo-export.wav` before the first save): the song twice plus a 2 s tail.

Linux needs the ALSA dev package (`alsa-lib` on Arch, `libasound2-dev` on Debian/Ubuntu).

MIT licensed; see `LICENSE`.
