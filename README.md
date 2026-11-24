# Ritmo

A pattern-based music app for making beats with only a laptop. Rust, egui, cpal.

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

## Keys

- `Z S X D C V G B H N J M` = C3 octave, `Q 2 W 3 E R 5 T 6 Y 7 U I 9 O 0 P` = C4 octave, played on the selected channel (outlined in the Channel Rack; the status bar names it).
- **Space** = play/stop. **F5 / F6 / F7 / F9** = Playlist / Channel Rack / Piano Roll / Mixer.
- **Export** writes `~/Music/ritmo-export.wav` (the song twice plus a 2 s tail).

Linux needs the ALSA dev package (`alsa-lib` on Arch, `libasound2-dev` on Debian/Ubuntu).

MIT licensed; see `LICENSE`.
