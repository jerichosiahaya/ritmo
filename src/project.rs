//! Project model: everything a song is made of, in growable collections referenced by
//! stable IDs. It replaces the fixed-size `Song` during v0.2 (see ROADMAP.md). For now it
//! lives next to `Song`, and `Project::from(&song)` converts the current song.
//!
//! - Channels (Channel Rack) hold an instrument and route into a mixer track.
//! - Patterns hold notes; each note names the channel it plays on. Drum steps are short notes.
//! - Clips place a pattern (or part of it, from `offset`) on a playlist track.
//! - Time is in ticks, `PPQ` per quarter note.
//!
//! IDs come from one counter per project and are never reused, even after a delete.

// Week 2 of the roadmap moves the engine and UI onto this model; until then only tests use it.
#![allow(dead_code)]

use crate::engine::{BARS, BUS_NAMES, CHANNEL_NAMES, CHORDS, CH_BUS, Fx, LEAD_KEYS, LEAD_LOW, STEPS, Song, TOTAL_STEPS, TRACKS};
use std::collections::HashSet;

pub type Tick = u32;
/// Ticks per quarter note. Divides evenly into 16ths (24) and 8th-note triplets (32).
pub const PPQ: Tick = 96;
/// One 16th note: the step sequencer's grid.
pub const STEP: Tick = PPQ / 4;
/// Longest song the playlist allows.
pub const MAX_BARS: Tick = 999;
/// Pitch the step sequencer writes (drum instruments ignore pitch).
pub const STEP_PITCH: u8 = 60;
/// Velocity for new notes (1..=127, like MIDI).
pub const DEFAULT_VEL: u8 = 100;

macro_rules! ids {
    ($($(#[$doc:meta])* $name:ident),* $(,)?) => {$(
        $(#[$doc])*
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
        pub struct $name(pub u32);
    )*};
}

ids!(
    /// A Channel Rack channel (one instrument).
    ChannelId,
    /// A mixer track; channels route into one.
    MixerId,
    PatternId,
    NoteId,
    /// A playlist lane.
    TrackId,
    /// One placement of a pattern on a playlist track.
    ClipId,
);

/// Built-in instruments the engine can play.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Instrument {
    Kick,
    Snare,
    Hat,
    /// FM electric piano.
    Rhodes,
    /// Sine bass.
    Bass,
}

#[derive(Clone, PartialEq, Debug)]
pub struct Channel {
    pub id: ChannelId,
    pub name: String,
    pub instrument: Instrument,
    pub vol: f32,
    /// -1 = left, 1 = right.
    pub pan: f32,
    pub mute: bool,
    pub mixer: MixerId,
}

#[derive(Clone, PartialEq, Debug)]
pub struct MixerTrack {
    pub id: MixerId,
    pub name: String,
    pub vol: f32,
    pub pan: f32,
    pub mute: bool,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Note {
    pub id: NoteId,
    pub channel: ChannelId,
    pub pitch: u8,
    pub start: Tick,
    pub len: Tick,
    pub vel: u8,
}

impl Note {
    pub fn end(&self) -> Tick {
        self.start + self.len
    }
}

#[derive(Clone, PartialEq, Debug)]
pub struct Pattern {
    pub id: PatternId,
    pub name: String,
    /// Length in ticks; every note ends at or before it.
    pub len: Tick,
    pub notes: Vec<Note>,
}

#[derive(Clone, PartialEq, Debug)]
pub struct Track {
    pub id: TrackId,
    pub name: String,
    pub mute: bool,
}

/// Plays pattern ticks `offset .. offset + len` from `start` on the playlist.
/// Past the pattern's end the clip is silent (no looping).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Clip {
    pub id: ClipId,
    pub track: TrackId,
    pub pattern: PatternId,
    pub start: Tick,
    pub len: Tick,
    pub offset: Tick,
}

impl Clip {
    pub fn end(&self) -> Tick {
        self.start + self.len
    }
}

#[derive(Clone, PartialEq, Debug)]
pub struct Project {
    pub bpm: f32,
    /// Share of each 8th-note pair given to the first 16th: 0.5 = straight, 0.75 = hard swing.
    pub swing: f32,
    pub beats_per_bar: u32,
    /// Master volume and the master insert (lo-fi chain).
    pub master: Fx,
    pub channels: Vec<Channel>,
    pub mixer: Vec<MixerTrack>,
    pub patterns: Vec<Pattern>,
    pub tracks: Vec<Track>,
    pub clips: Vec<Clip>,
    /// Playback loops over `start..end` when set.
    pub loop_range: Option<(Tick, Tick)>,
    /// Last ID handed out.
    next_id: u32,
}

impl Default for Project {
    /// An empty 120 BPM project with a neutral master insert.
    fn default() -> Self {
        Project {
            bpm: 120.0,
            swing: 0.5,
            beats_per_bar: 4,
            master: Fx { tone: 1.0, crush: 0.0, wobble: 0.0, vinyl: 0.0, reverb: 0.0, volume: 0.8 },
            channels: Vec::new(),
            mixer: Vec::new(),
            patterns: Vec::new(),
            tracks: Vec::new(),
            clips: Vec::new(),
            loop_range: None,
            next_id: 0,
        }
    }
}

impl Project {
    fn fresh_id(&mut self) -> u32 {
        self.next_id += 1;
        self.next_id
    }

    pub fn bar_len(&self) -> Tick {
        self.beats_per_bar * PPQ
    }

    /// End of the last clip: the song's length.
    pub fn end(&self) -> Tick {
        self.clips.iter().map(Clip::end).max().unwrap_or(0)
    }

    pub fn add_mixer_track(&mut self, name: &str) -> MixerId {
        let id = MixerId(self.fresh_id());
        self.mixer.push(MixerTrack { id, name: name.into(), vol: 0.8, pan: 0.0, mute: false });
        id
    }

    /// None if `mixer` does not exist.
    pub fn add_channel(&mut self, name: &str, instrument: Instrument, mixer: MixerId) -> Option<ChannelId> {
        self.mixer_track(mixer)?;
        let id = ChannelId(self.fresh_id());
        self.channels.push(Channel { id, name: name.into(), instrument, vol: 0.8, pan: 0.0, mute: false, mixer });
        Some(id)
    }

    pub fn add_pattern(&mut self, name: &str, len: Tick) -> PatternId {
        let id = PatternId(self.fresh_id());
        self.patterns.push(Pattern { id, name: name.into(), len, notes: Vec::new() });
        id
    }

    /// Adds a note, growing the pattern if the note ends past it. None if the pattern or
    /// channel does not exist, or the note is empty or out of MIDI range.
    pub fn add_note(&mut self, pattern: PatternId, channel: ChannelId, pitch: u8, start: Tick, len: Tick, vel: u8) -> Option<NoteId> {
        if self.channel(channel).is_none() || len == 0 || pitch > 127 || !(1..=127).contains(&vel) {
            return None;
        }
        self.pattern(pattern)?;
        let id = NoteId(self.fresh_id());
        let p = self.pattern_mut(pattern)?;
        let note = Note { id, channel, pitch, start, len, vel };
        p.len = p.len.max(note.end());
        p.notes.push(note);
        Some(id)
    }

    pub fn add_track(&mut self, name: &str) -> TrackId {
        let id = TrackId(self.fresh_id());
        self.tracks.push(Track { id, name: name.into(), mute: false });
        id
    }

    /// Places `pattern` from its start. None if the track or pattern does not exist,
    /// the clip is empty, or it ends past `MAX_BARS`.
    pub fn add_clip(&mut self, track: TrackId, pattern: PatternId, start: Tick, len: Tick) -> Option<ClipId> {
        if self.track(track).is_none() || self.pattern(pattern).is_none() || len == 0 || start + len > MAX_BARS * self.bar_len() {
            return None;
        }
        let id = ClipId(self.fresh_id());
        self.clips.push(Clip { id, track, pattern, start, len, offset: 0 });
        Some(id)
    }

    pub fn remove_note(&mut self, pattern: PatternId, note: NoteId) -> bool {
        self.pattern_mut(pattern).is_some_and(|p| remove(&mut p.notes, |n| n.id == note) > 0)
    }

    pub fn remove_clip(&mut self, id: ClipId) -> bool {
        remove(&mut self.clips, |c| c.id == id) > 0
    }

    /// Also removes every clip of the pattern.
    pub fn remove_pattern(&mut self, id: PatternId) -> bool {
        remove(&mut self.clips, |c| c.pattern == id);
        remove(&mut self.patterns, |p| p.id == id) > 0
    }

    /// Also removes every clip on the track.
    pub fn remove_track(&mut self, id: TrackId) -> bool {
        remove(&mut self.clips, |c| c.track == id);
        remove(&mut self.tracks, |t| t.id == id) > 0
    }

    /// Also removes the channel's notes from every pattern.
    pub fn remove_channel(&mut self, id: ChannelId) -> bool {
        for p in &mut self.patterns {
            remove(&mut p.notes, |n| n.channel == id);
        }
        remove(&mut self.channels, |c| c.id == id) > 0
    }

    pub fn channel(&self, id: ChannelId) -> Option<&Channel> {
        self.channels.iter().find(|c| c.id == id)
    }

    pub fn mixer_track(&self, id: MixerId) -> Option<&MixerTrack> {
        self.mixer.iter().find(|m| m.id == id)
    }

    pub fn pattern(&self, id: PatternId) -> Option<&Pattern> {
        self.patterns.iter().find(|p| p.id == id)
    }

    pub fn pattern_mut(&mut self, id: PatternId) -> Option<&mut Pattern> {
        self.patterns.iter_mut().find(|p| p.id == id)
    }

    pub fn track(&self, id: TrackId) -> Option<&Track> {
        self.tracks.iter().find(|t| t.id == id)
    }

    pub fn clip(&self, id: ClipId) -> Option<&Clip> {
        self.clips.iter().find(|c| c.id == id)
    }

    /// Checks every invariant: unique IDs no higher than the counter, references that resolve,
    /// non-empty notes and clips inside their limits. Loading a file will run this.
    pub fn validate(&self) -> Result<(), String> {
        if !(self.bpm.is_finite() && (10.0..=999.0).contains(&self.bpm)) {
            return Err(format!("tempo {} out of range", self.bpm));
        }
        if !(0.5..=0.75).contains(&self.swing) {
            return Err(format!("swing {} out of range", self.swing));
        }
        if !(1..=16).contains(&self.beats_per_bar) {
            return Err(format!("{} beats per bar", self.beats_per_bar));
        }
        let ids = self
            .mixer
            .iter()
            .map(|m| m.id.0)
            .chain(self.channels.iter().map(|c| c.id.0))
            .chain(self.patterns.iter().flat_map(|p| std::iter::once(p.id.0).chain(p.notes.iter().map(|n| n.id.0))))
            .chain(self.tracks.iter().map(|t| t.id.0))
            .chain(self.clips.iter().map(|c| c.id.0));
        let mut seen = HashSet::new();
        for id in ids {
            if id == 0 || id > self.next_id {
                return Err(format!("ID {id} was never handed out (counter at {})", self.next_id));
            }
            if !seen.insert(id) {
                return Err(format!("ID {id} is used twice"));
            }
        }
        for c in &self.channels {
            if self.mixer_track(c.mixer).is_none() {
                return Err(format!("channel {:?} routes to missing mixer track {:?}", c.name, c.mixer));
            }
        }
        for p in &self.patterns {
            for n in &p.notes {
                if self.channel(n.channel).is_none() {
                    return Err(format!("note {:?} in {:?} plays on missing channel {:?}", n.id, p.name, n.channel));
                }
                if n.len == 0 || n.end() > p.len || n.pitch > 127 || !(1..=127).contains(&n.vel) {
                    return Err(format!("note {:?} in {:?} is invalid: {n:?}", n.id, p.name));
                }
            }
        }
        let max_end = MAX_BARS * self.bar_len();
        for c in &self.clips {
            if self.track(c.track).is_none() || self.pattern(c.pattern).is_none() {
                return Err(format!("clip {:?} points to a missing track or pattern", c.id));
            }
            if c.len == 0 || c.end() > max_end {
                return Err(format!("clip {:?} is empty or ends past bar {MAX_BARS}", c.id));
            }
        }
        if let Some((a, b)) = self.loop_range
            && (a >= b || b > max_end)
        {
            return Err(format!("loop range {a}..{b} is invalid"));
        }
        Ok(())
    }
}

/// Removes the items matching `f` and returns how many were removed.
fn remove<T>(v: &mut Vec<T>, f: impl Fn(&T) -> bool) -> usize {
    let before = v.len();
    v.retain(|x| !f(x));
    before - v.len()
}

/// Converts the v0.1 song. Chords become ordinary notes on the Keys and Bass channels;
/// adjacent melody cells become one note, as the engine plays them.
impl From<&Song> for Project {
    fn from(s: &Song) -> Self {
        let mut p = Project { bpm: s.bpm, swing: s.swing, master: s.fx, ..Project::default() };
        let bar = p.bar_len();
        let mixer: Vec<MixerId> = BUS_NAMES
            .iter()
            .zip(s.bus)
            .map(|(name, vol)| {
                let id = p.add_mixer_track(name);
                p.mixer.last_mut().expect("just added").vol = vol;
                id
            })
            .collect();
        let instruments = [Instrument::Kick, Instrument::Snare, Instrument::Hat, Instrument::Rhodes, Instrument::Bass, Instrument::Rhodes];
        let ch: Vec<ChannelId> = (0..CHANNEL_NAMES.len())
            .map(|i| {
                let id = p.add_channel(CHANNEL_NAMES[i], instruments[i], mixer[CH_BUS[i]]).expect("mixer track exists");
                let (c, src) = (p.channels.last_mut().expect("just added"), s.channels[i]);
                (c.vol, c.pan, c.mute) = (src.vol, src.pan, src.mute);
                id
            })
            .collect();
        let (keys, bass, lead) = (ch[3], ch[4], ch[5]);
        let song_len = BARS as Tick * bar;

        let drums = p.add_pattern("Pattern 1", bar);
        for (tr, row) in s.drums.iter().enumerate().take(TRACKS) {
            for step in (0..STEPS).filter(|&st| row[st]) {
                p.add_note(drums, ch[tr], STEP_PITCH, step as Tick * STEP, STEP, DEFAULT_VEL);
            }
        }

        let chords = p.add_pattern("Chords", song_len);
        for (b, &j) in s.chords.iter().enumerate() {
            let c = &CHORDS[j.min(CHORDS.len() - 1)];
            let start = b as Tick * bar;
            for &n in c.notes {
                p.add_note(chords, keys, n, start, bar, DEFAULT_VEL);
            }
            if c.bass > 0 {
                p.add_note(chords, bass, c.bass, start, bar, DEFAULT_VEL);
            }
        }

        let melody = p.add_pattern("Melody", song_len);
        for (k, &row) in s.melody.iter().enumerate().take(LEAD_KEYS) {
            let mut step = 0;
            while step < TOTAL_STEPS {
                if row >> step & 1 == 0 {
                    step += 1;
                    continue;
                }
                let first = step;
                while step < TOTAL_STEPS && row >> step & 1 == 1 {
                    step += 1;
                }
                let (start, len) = (first as Tick * STEP, (step - first) as Tick * STEP);
                p.add_note(melody, lead, LEAD_LOW + k as u8, start, len, DEFAULT_VEL);
            }
        }

        let (chord_track, drum_track, melody_track) = (p.add_track("Chords"), p.add_track("Drums"), p.add_track("Melody"));
        p.add_clip(chord_track, chords, 0, song_len);
        for b in (0..BARS).filter(|&b| s.drum_bars[b]) {
            p.add_clip(drum_track, drums, b as Tick * bar, bar);
        }
        // One clip per run of enabled bars, so a note held across bars is not cut.
        let mut b = 0;
        while b < BARS {
            if !s.lead_bars[b] {
                b += 1;
                continue;
            }
            let first = b;
            while b < BARS && s.lead_bars[b] {
                b += 1;
            }
            let start = first as Tick * bar;
            if p.add_clip(melody_track, melody, start, (b - first) as Tick * bar).is_some() {
                p.clips.last_mut().expect("just added").offset = start;
            }
        }
        p
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BAR: Tick = 4 * PPQ;

    fn named<'a>(p: &'a Project, name: &str) -> &'a Pattern {
        p.patterns.iter().find(|x| x.name == name).unwrap()
    }

    fn channel_named(p: &Project, name: &str) -> ChannelId {
        p.channels.iter().find(|c| c.name == name).unwrap().id
    }

    /// A small project built through the API: one mixer track, channel, pattern, track, clip.
    fn tiny() -> (Project, ChannelId, PatternId, TrackId, ClipId) {
        let mut p = Project::default();
        let m = p.add_mixer_track("Main");
        let ch = p.add_channel("Kick", Instrument::Kick, m).unwrap();
        let pat = p.add_pattern("Beat", BAR);
        p.add_note(pat, ch, STEP_PITCH, 0, STEP, DEFAULT_VEL).unwrap();
        let tr = p.add_track("Drums");
        let clip = p.add_clip(tr, pat, 0, BAR).unwrap();
        (p, ch, pat, tr, clip)
    }

    #[test]
    fn default_song_converts_and_validates() {
        let s = Song::default();
        let p = Project::from(&s);
        p.validate().unwrap();
        assert_eq!((p.mixer.len(), p.channels.len(), p.patterns.len(), p.tracks.len()), (4, 6, 3, 3));
        assert_eq!((p.bpm, p.swing, p.master), (s.bpm, s.swing, s.fx));
        // Drums: 3 kicks, 2 snares, 8 hats in a 1-bar pattern, placed on all 4 bars.
        assert_eq!(named(&p, "Pattern 1").notes.len(), 13);
        assert_eq!(named(&p, "Pattern 1").len, BAR);
        // Chords: Dm9, G13, Cmaj9, Am9, each 4 keys notes plus a bass note, one bar long.
        let chords = named(&p, "Chords");
        assert_eq!(chords.notes.len(), 20);
        assert!(chords.notes.iter().all(|n| n.len == BAR && n.start % BAR == 0));
        // Melody: 6 notes; the first is C5 at step 20 for two 16ths.
        let melody = named(&p, "Melody");
        assert_eq!(melody.notes.len(), 6);
        assert!(melody.notes.iter().any(|n| (n.pitch, n.start, n.len) == (72, 20 * STEP, 2 * STEP)));
        // Clips: 1 chord clip + 4 drum clips + 1 melody clip spanning all 4 bars.
        assert_eq!(p.clips.len(), 6);
        assert_eq!(p.end(), 4 * BAR);
        // Mixer routing and channel settings carried over.
        let bass = p.channel(channel_named(&p, "Bass")).unwrap();
        assert_eq!(p.mixer_track(bass.mixer).unwrap().name, "Bass");
        assert_eq!((bass.vol, bass.instrument), (s.channels[4].vol, Instrument::Bass));
    }

    #[test]
    fn disabled_bars_split_clips() {
        let mut s = Song::default();
        s.drum_bars[0] = false;
        s.lead_bars[1] = false;
        let p = Project::from(&s);
        p.validate().unwrap();
        let track = |name: &str| p.tracks.iter().find(|t| t.name == name).unwrap().id;
        let drums: Vec<Tick> = p.clips.iter().filter(|c| c.track == track("Drums")).map(|c| c.start).collect();
        assert_eq!(drums, [BAR, 2 * BAR, 3 * BAR]);
        let melody: Vec<(Tick, Tick, Tick)> = p.clips.iter().filter(|c| c.track == track("Melody")).map(|c| (c.start, c.len, c.offset)).collect();
        assert_eq!(melody, [(0, BAR, 0), (2 * BAR, 2 * BAR, 2 * BAR)]);
    }

    /// Converting loses nothing: the drum grid, melody cells and chords can be read back.
    #[test]
    fn conversion_keeps_all_song_content() {
        let mut seed = 0x2545_f491_u32;
        let mut rand = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        for _ in 0..50 {
            let mut s = Song::default();
            for row in &mut s.drums {
                for cell in row.iter_mut() {
                    *cell = rand() % 3 == 0;
                }
            }
            for row in &mut s.melody {
                *row = (rand() as u64) << 32 | rand() as u64;
                *row &= (rand() as u64) << 32 | rand() as u64; // sparser
            }
            for c in &mut s.chords {
                *c = rand() as usize % CHORDS.len();
            }
            let p = Project::from(&s);
            p.validate().unwrap();

            let drums = named(&p, "Pattern 1");
            for (tr, row) in s.drums.iter().enumerate() {
                let ch = channel_named(&p, CHANNEL_NAMES[tr]);
                for (st, &on) in row.iter().enumerate() {
                    let hit = drums.notes.iter().any(|n| n.channel == ch && n.start == st as Tick * STEP);
                    assert_eq!(hit, on, "drum {tr} step {st}");
                }
            }

            let mut melody = [0u64; LEAD_KEYS];
            for n in &named(&p, "Melody").notes {
                assert_eq!((n.start % STEP, n.len % STEP), (0, 0));
                for st in n.start / STEP..n.end() / STEP {
                    melody[(n.pitch - LEAD_LOW) as usize] |= 1 << st;
                }
            }
            assert_eq!(melody, s.melody);
            // Each note is a maximal run: no two notes of one pitch touch.
            let notes = &named(&p, "Melody").notes;
            assert!(notes.iter().all(|a| !notes.iter().any(|b| b.pitch == a.pitch && b.start == a.end())));

            let (keys, chords) = (channel_named(&p, "Keys"), named(&p, "Chords"));
            for (b, &j) in s.chords.iter().enumerate() {
                let mut got: Vec<u8> = chords.notes.iter().filter(|n| n.channel == keys && n.start == b as Tick * BAR).map(|n| n.pitch).collect();
                got.sort();
                let mut want = CHORDS[j].notes.to_vec();
                want.sort();
                assert_eq!(got, want, "bar {b}");
            }
        }
    }

    #[test]
    fn ids_are_unique_and_never_reused() {
        let (mut p, ch, pat, _, clip) = tiny();
        let before: Vec<u32> = [ch.0, pat.0, clip.0].into();
        assert!(p.remove_clip(clip));
        let tr2 = p.add_track("Two");
        let clip2 = p.add_clip(tr2, pat, BAR, BAR).unwrap();
        assert!(before.iter().all(|&id| id < tr2.0 && id < clip2.0));
        assert!(clip2.0 > tr2.0);
        p.validate().unwrap();
    }

    #[test]
    fn removals_cascade() {
        let (mut p, ch, pat, tr, _) = tiny();
        let other = p.add_pattern("Other", BAR);
        p.add_clip(tr, other, BAR, BAR).unwrap();

        assert!(p.remove_channel(ch));
        assert!(p.pattern(pat).unwrap().notes.is_empty());
        p.validate().unwrap();

        assert!(p.remove_pattern(pat));
        assert!(p.clips.iter().all(|c| c.pattern == other) && p.clips.len() == 1);
        p.validate().unwrap();

        assert!(p.remove_track(tr));
        assert!(p.clips.is_empty());
        p.validate().unwrap();

        assert!(!p.remove_track(tr) && !p.remove_pattern(pat) && !p.remove_channel(ch));
    }

    #[test]
    fn add_rejects_bad_input_and_grows_patterns() {
        let (mut p, ch, pat, tr, _) = tiny();
        assert!(p.add_note(PatternId(999), ch, 60, 0, STEP, 100).is_none());
        assert!(p.add_note(pat, ChannelId(999), 60, 0, STEP, 100).is_none());
        assert!(p.add_note(pat, ch, 60, 0, 0, 100).is_none());
        assert!(p.add_note(pat, ch, 128, 0, STEP, 100).is_none());
        assert!(p.add_note(pat, ch, 60, 0, STEP, 0).is_none());
        assert!(p.add_channel("X", Instrument::Hat, MixerId(999)).is_none());
        assert!(p.add_clip(tr, pat, MAX_BARS * BAR - BAR, 2 * BAR).is_none());
        assert!(p.add_clip(tr, pat, 0, 0).is_none());
        // A note past the end grows the pattern.
        p.add_note(pat, ch, 60, 2 * BAR, STEP, 100).unwrap();
        assert_eq!(p.pattern(pat).unwrap().len, 2 * BAR + STEP);
        p.validate().unwrap();
    }

    #[test]
    fn validate_catches_broken_projects() {
        let (p, ch, ..) = tiny();
        let broken = |f: &dyn Fn(&mut Project)| {
            let mut q = p.clone();
            f(&mut q);
            q.validate().unwrap_err()
        };
        broken(&|q| q.clips[0].pattern = PatternId(999));
        broken(&|q| q.clips[0].len = 0);
        broken(&|q| q.clips[0].start = MAX_BARS * BAR);
        broken(&|q| q.patterns[0].notes[0].channel = ChannelId(999));
        broken(&|q| q.patterns[0].notes[0].len = 0);
        broken(&|q| q.patterns[0].notes[0].start = BAR);
        broken(&|q| q.channels[0].mixer = MixerId(999));
        broken(&|q| q.tracks[0].id = TrackId(ch.0));
        broken(&|q| q.clips[0].id = ClipId(q.next_id + 1));
        broken(&|q| q.loop_range = Some((BAR, BAR)));
        broken(&|q| q.bpm = f32::NAN);
        broken(&|q| q.swing = 0.9);
        p.validate().unwrap();
    }
}
