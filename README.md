# Ritmo

A pattern-based music app for making beats with only a laptop. Rust, egui, cpal.

```sh
cargo run --release
```

## Layout

- **Toolbar:** play/stop, an LCD with bar:step and tempo, BPM, swing and master knobs, buttons to show and hide windows, and Export.
- **Browser (left):** instruments, chords and patterns. Click a chord to put it on the selected bar, or drag it onto a Playlist bar.
- **Playlist (F5):** 4 bars with chord clips, drum-pattern clips and melody clips. Click a drum or melody clip to turn it on/off for that bar; right-click a chord clip to choose a chord.
- **Channel Rack (F6):** for each channel, an on/off LED, pan and volume knobs, and 16 step buttons for the drums.
- **Piano Roll (F7):** the lead melody across 4 bars of 16ths. Left-drag draws, right-drag erases, click a key to hear it.
- **Mixer (F9):** Drums, Keys, Bass, Lead and Master strips with faders and peak meters, plus the lo-fi master insert (Tone, Crush, Wobble, Vinyl, Reverb).

Knobs: drag up/down, scroll, or use the arrow keys; double-click resets.

## Keys

- `Z S X D C V G B H N J M` = C3 octave, `Q 2 W 3 E R 5 T 6 Y 7 U I 9 O 0 P` = C4 octave (played on the Lead channel).
- **Space** = play/stop. **F5 / F6 / F7 / F9** = Playlist / Channel Rack / Piano Roll / Mixer.
- **Export** writes `~/Music/ritmo-export.wav` (2 loops plus a 2 s tail).

Linux needs the ALSA dev package (`alsa-lib` on Arch).
