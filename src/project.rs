//! Project model: everything a song is made of, in growable collections referenced by
//! stable IDs. The UI edits a `Project`; `compile()` turns it into the engine's `Snapshot`.
//!
//! - Channels (Channel Rack) hold an instrument and route into a mixer track.
//! - Patterns hold notes; each note names the channel it plays on. Drum steps are short notes.
//! - Clips place a pattern (or part of it, from `offset`) on a playlist track.
//! - Time is in ticks, `PPQ` per quarter note.
//!
//! IDs come from one counter per project and are never reused, even after a delete.

use crate::engine::{ChannelMix, ChopMix, FX_NEUTRAL, Fx, MAX_CHANNELS, MAX_MIXER, MixerMix, SampleData, SeqNote, SliceMix, Snapshot, pan_gains};
pub use crate::engine::{CHOP_BASE, MAX_SLICES};
pub use crate::engine::{Instrument, PPQ, STEP, Tick};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Longest song the playlist allows.
pub const MAX_BARS: Tick = 999;
/// Pitch the step sequencer writes (drum instruments ignore pitch).
pub const STEP_PITCH: u8 = 60;
/// Velocity for new notes (1..=127, like MIDI). 100 plays an instrument at its normal level.
pub const DEFAULT_VEL: u8 = 100;
/// Velocity of stamped chord notes: softer than a melody on the same electric piano.
pub const CHORD_VEL: u8 = 53;

pub struct Chord {
    pub name: &'static str,
    pub bass: u8,
    pub notes: &'static [u8],
}

/// Index 0 is "no chord".
pub const CHORDS: &[Chord] = &[
    Chord { name: "—", bass: 0, notes: &[] },
    Chord { name: "Dm9", bass: 38, notes: &[53, 57, 60, 64] },
    Chord { name: "G13", bass: 43, notes: &[53, 59, 64, 69] },
    Chord { name: "Cmaj9", bass: 36, notes: &[52, 55, 59, 62] },
    Chord { name: "Am9", bass: 45, notes: &[55, 60, 64, 71] },
    Chord { name: "Fmaj7", bass: 41, notes: &[53, 57, 60, 64] },
    Chord { name: "Em7", bass: 40, notes: &[55, 59, 62, 64] },
    Chord { name: "Bbmaj7", bass: 46, notes: &[53, 57, 58, 62] },
    Chord { name: "A7", bass: 45, notes: &[55, 61, 64, 67] },
];

macro_rules! ids {
    ($($(#[$doc:meta])* $name:ident),* $(,)?) => {$(
        $(#[$doc])*
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
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

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Channel {
    pub id: ChannelId,
    pub name: String,
    pub instrument: Instrument,
    pub vol: f32,
    /// -1 = left, 1 = right.
    pub pan: f32,
    pub mute: bool,
    pub mixer: MixerId,
    /// The sample and slices of an `Instrument::Chop` channel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chop: Option<Chop>,
}

/// A chop channel's audio and how it is cut into slices (one per pad).
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Chop {
    /// The audio file, as chosen. If it is missing, the pads stay silent.
    pub path: PathBuf,
    /// The sample's length in frames when it was chopped.
    pub len: u32,
    /// End to end, in order; slice `k` plays on pad `k + 1` (pitch `CHOP_BASE + k`).
    pub slices: Vec<Slice>,
    /// Play each slice to its end whatever the note length.
    pub one_shot: bool,
    /// A new hit fades out the chops still sounding on this channel.
    pub cut_itself: bool,
}

#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct Slice {
    /// Frames of the sample, `start..end`.
    pub start: u32,
    pub end: u32,
    /// Semitones; also changes speed, like a turntable.
    pub pitch: f32,
    pub reverse: bool,
    pub gain: f32,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct MixerTrack {
    pub id: MixerId,
    pub name: String,
    pub vol: f32,
    pub pan: f32,
    pub mute: bool,
}

#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
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

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Pattern {
    pub id: PatternId,
    pub name: String,
    /// Length in ticks; every note ends at or before it.
    pub len: Tick,
    pub notes: Vec<Note>,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Track {
    pub id: TrackId,
    pub name: String,
    pub mute: bool,
}

/// Plays pattern ticks `offset .. offset + len` from `start` on the playlist.
/// Past the pattern's end the clip is silent (no looping).
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
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

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
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
            master: FX_NEUTRAL,
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

    /// None if the mixer already has `MAX_MIXER` tracks.
    pub fn add_mixer_track(&mut self, name: &str) -> Option<MixerId> {
        if self.mixer.len() >= MAX_MIXER {
            return None;
        }
        let id = MixerId(self.fresh_id());
        self.mixer.push(MixerTrack { id, name: name.into(), vol: 0.8, pan: 0.0, mute: false });
        Some(id)
    }

    /// None if `mixer` does not exist or there are already `MAX_CHANNELS` channels.
    pub fn add_channel(&mut self, name: &str, instrument: Instrument, mixer: MixerId) -> Option<ChannelId> {
        self.mixer_track(mixer)?;
        if self.channels.len() >= MAX_CHANNELS {
            return None;
        }
        let id = ChannelId(self.fresh_id());
        self.channels.push(Channel { id, name: name.into(), instrument, vol: 0.8, pan: 0.0, mute: false, mixer, chop: None });
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
        self.add_clip_at(track, pattern, start, len, 0)
    }

    /// Like `add_clip`, playing the pattern from `offset`.
    pub fn add_clip_at(&mut self, track: TrackId, pattern: PatternId, start: Tick, len: Tick, offset: Tick) -> Option<ClipId> {
        if self.track(track).is_none() || self.pattern(pattern).is_none() || len == 0 || start + len > MAX_BARS * self.bar_len() {
            return None;
        }
        let id = ClipId(self.fresh_id());
        self.clips.push(Clip { id, track, pattern, start, len, offset });
        Some(id)
    }

    #[allow(dead_code)] // Note-level editing arrives with the piano-roll upgrade (week 8).
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

    /// Builds what the engine plays: indices instead of IDs, volume and pan as gains, and every
    /// clip's notes placed in song time, sorted by start. Notes starting outside a clip's part of
    /// the pattern are skipped; notes running past the clip's end are cut there. Without a loop
    /// range, playback loops the song rounded up to whole bars.
    #[cfg(test)]
    pub fn compile(&self) -> Snapshot {
        self.compile_with(&|_| None)
    }

    /// `compile`, with chop channels' audio from `samples` (decoded audio by file path); `compile`
    /// (tests only) leaves them without audio.
    pub fn compile_with(&self, samples: &dyn Fn(&Path) -> Option<Arc<SampleData>>) -> Snapshot {
        let mut chops = Vec::new();
        let gains = |vol: f32, pan: f32| {
            let (l, r) = pan_gains(pan);
            [vol * l, vol * r]
        };
        let channels: Vec<ChannelMix> = self
            .channels
            .iter()
            .take(MAX_CHANNELS)
            .map(|c| ChannelMix {
                instrument: c.instrument,
                gain: gains(c.vol, c.pan),
                mute: c.mute,
                mixer: self.mixer.iter().take(MAX_MIXER).position(|m| m.id == c.mixer).unwrap_or(0),
                chop: c.chop.as_ref().map(|chop| {
                    let slices = chop.slices.iter().map(|s| SliceMix { start: s.start, end: s.end, speed: 2f32.powf(s.pitch / 12.0), reverse: s.reverse, gain: s.gain }).collect();
                    chops.push(ChopMix { sample: samples(&chop.path), slices, one_shot: chop.one_shot, cut_itself: chop.cut_itself });
                    chops.len() - 1
                }),
            })
            .collect();
        let mixer = self.mixer.iter().take(MAX_MIXER).map(|m| MixerMix { gain: gains(m.vol, m.pan), mute: m.mute }).collect();
        let index = |id: ChannelId| self.channels.iter().take(MAX_CHANNELS).position(|c| c.id == id);

        let mut notes = Vec::new();
        for clip in &self.clips {
            let (Some(track), Some(pattern)) = (self.track(clip.track), self.pattern(clip.pattern)) else { continue };
            if track.mute {
                continue;
            }
            for n in pattern.notes.iter().filter(|n| (clip.offset..clip.offset + clip.len).contains(&n.start)) {
                let Some(channel) = index(n.channel) else { continue };
                let start = clip.start + n.start - clip.offset;
                notes.push(SeqNote { start, end: (start + n.len).min(clip.end()), channel: channel as u16, pitch: n.pitch, vel: n.vel });
            }
        }
        notes.sort_by_key(|n| (n.start, n.channel, n.pitch));

        let bar = self.bar_len();
        let (loop_start, loop_end) = self.loop_range.unwrap_or((0, self.end().div_ceil(bar).max(1) * bar));
        Snapshot { bpm: self.bpm, swing: self.swing, fx: self.master, channels, mixer, notes, chops, loop_start, loop_end }
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
        if self.channels.len() > MAX_CHANNELS || self.mixer.len() > MAX_MIXER {
            return Err(format!("{} channels and {} mixer tracks; the limits are {MAX_CHANNELS} and {MAX_MIXER}", self.channels.len(), self.mixer.len()));
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
            if c.chop.is_some() != (c.instrument == Instrument::Chop) {
                return Err(format!("channel {:?}: only chop channels have chop settings, and every chop channel has them", c.name));
            }
            if let Some(chop) = &c.chop {
                let ok = |s: &Slice| s.start < s.end && s.end <= chop.len && (-48.0..=48.0).contains(&s.pitch) && (0.0..=4.0).contains(&s.gain);
                if chop.slices.is_empty() || chop.slices.len() > MAX_SLICES || !chop.slices.iter().all(ok) || chop.slices.windows(2).any(|w| w[0].end != w[1].start) {
                    return Err(format!("channel {:?} has invalid chops", c.name));
                }
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

/// Editing operations the windows use. Each keeps the project valid.
impl Project {
    pub fn channel_index(&self, id: ChannelId) -> Option<usize> {
        self.channels.iter().position(|c| c.id == id)
    }

    /// First channel playing `instrument` that has notes in `pattern`, else the first such channel.
    pub fn channel_for(&self, pattern: PatternId, instrument: Instrument) -> Option<ChannelId> {
        let mut of_kind = self.channels.iter().filter(|c| c.instrument == instrument);
        let used = |c: &&Channel| self.pattern(pattern).is_some_and(|p| p.notes.iter().any(|n| n.channel == c.id));
        of_kind.clone().find(used).or_else(|| of_kind.next()).map(|c| c.id)
    }

    /// Whether `channel` has a note starting on 16th `step` of `pattern`.
    pub fn step(&self, pattern: PatternId, channel: ChannelId, step: Tick) -> bool {
        self.pattern(pattern).is_some_and(|p| p.notes.iter().any(|n| n.channel == channel && n.start == step * STEP))
    }

    /// Step sequencer click: adds a 16th note on `step`, or removes the notes starting there.
    pub fn toggle_step(&mut self, pattern: PatternId, channel: ChannelId, step: Tick) {
        if self.step(pattern, channel, step) {
            if let Some(p) = self.pattern_mut(pattern) {
                remove(&mut p.notes, |n| n.channel == channel && n.start == step * STEP);
            }
        } else {
            self.add_note(pattern, channel, STEP_PITCH, step * STEP, STEP, DEFAULT_VEL);
        }
    }

    /// Whether a note of `channel` at `pitch` sounds during 16th `step` of `pattern`.
    pub fn cell(&self, pattern: PatternId, channel: ChannelId, pitch: u8, step: Tick) -> bool {
        let t = step * STEP;
        self.pattern(pattern).is_some_and(|p| p.notes.iter().any(|n| n.channel == channel && n.pitch == pitch && n.start <= t && t < n.end()))
    }

    /// Piano-roll painting on the 16th grid. Turning a cell on joins it with notes that end or
    /// start right next to it; turning it off splits the note it was part of.
    pub fn set_cell(&mut self, pattern: PatternId, channel: ChannelId, pitch: u8, step: Tick, on: bool) {
        let (t, next) = (step * STEP, step * STEP + STEP);
        let Some(p) = self.pattern_mut(pattern) else { return };
        let same = |n: &Note| n.channel == channel && n.pitch == pitch;
        let covering = p.notes.iter().position(|n| same(n) && n.start <= t && t < n.end());
        match (on, covering) {
            (true, None) => {
                let left = p.notes.iter().position(|n| same(n) && n.end() == t).map(|i| p.notes.remove(i));
                let right = p.notes.iter().position(|n| same(n) && n.start == next).map(|i| p.notes.remove(i));
                let start = left.map_or(t, |n| n.start);
                let end = right.map_or(next, |n| n.end());
                let vel = left.or(right).map_or(DEFAULT_VEL, |n| n.vel);
                self.add_note(pattern, channel, pitch, start, end - start, vel);
            }
            (false, Some(i)) => {
                let n = p.notes.remove(i);
                if n.start < t {
                    self.add_note(pattern, channel, pitch, n.start, t - n.start, n.vel);
                }
                if next < n.end() {
                    self.add_note(pattern, channel, pitch, next, n.end() - next, n.vel);
                }
            }
            _ => {}
        }
    }

    /// Writes `CHORDS[chord]` over `start..start + len` of `pattern`: keys on an electric-piano
    /// channel and the root on a bass channel (see `channel_for`), replacing the notes those
    /// channels started there. Chord 0 just clears. False if the project has no piano channel.
    pub fn stamp_chord(&mut self, pattern: PatternId, start: Tick, len: Tick, chord: usize) -> bool {
        let (Some(keys), bass) = (self.channel_for(pattern, Instrument::Rhodes), self.channel_for(pattern, Instrument::Bass)) else { return false };
        let Some(p) = self.pattern_mut(pattern) else { return false };
        remove(&mut p.notes, |n| (n.channel == keys || Some(n.channel) == bass) && (start..start + len).contains(&n.start));
        let c = &CHORDS[chord.min(CHORDS.len() - 1)];
        for &n in c.notes {
            self.add_note(pattern, keys, n, start, len, CHORD_VEL);
        }
        if let (Some(bass), true) = (bass, c.bass > 0) {
            self.add_note(pattern, bass, c.bass, start, len, DEFAULT_VEL);
        }
        true
    }

    /// Index into `CHORDS` of the chord the electric-piano notes starting at `start` spell, or 0.
    pub fn chord_at(&self, pattern: PatternId, start: Tick) -> usize {
        let Some(p) = self.pattern(pattern) else { return 0 };
        let piano = |c: ChannelId| self.channel(c).is_some_and(|c| c.instrument == Instrument::Rhodes);
        let mut got: Vec<u8> = p.notes.iter().filter(|n| n.start == start && piano(n.channel)).map(|n| n.pitch).collect();
        got.sort_unstable();
        CHORDS.iter().position(|c| {
            let mut want = c.notes.to_vec();
            want.sort_unstable();
            !want.is_empty() && want == got
        })
        .unwrap_or(0)
    }

    /// The clip on `track` that plays at `tick`.
    pub fn clip_at(&self, track: TrackId, tick: Tick) -> Option<&Clip> {
        self.clips.iter().find(|c| c.track == track && c.start <= tick && tick < c.end())
    }

    /// Where `pattern` is playing at song time `tick`, through any clip on an unmuted track.
    pub fn pattern_pos(&self, pattern: PatternId, tick: Tick) -> Option<Tick> {
        let playing = |c: &&Clip| c.pattern == pattern && c.start <= tick && tick < c.end() && self.track(c.track).is_some_and(|t| !t.mute);
        self.clips.iter().find(playing).map(|c| tick - c.start + c.offset)
    }

    /// Playlist click on bar `bar` of `track`: if clips cover it, cuts that bar out of them;
    /// otherwise places one bar of the track's nearest pattern (or `fallback` on an empty track),
    /// taking the matching bar of a longer pattern, and joins it with neighbours that continue it.
    pub fn toggle_bar(&mut self, track: TrackId, bar: Tick, fallback: PatternId) {
        let bl = self.bar_len();
        let (a, b) = (bar * bl, bar * bl + bl);
        let hit: Vec<Clip> = self.clips.iter().filter(|c| c.track == track && c.start < b && a < c.end()).copied().collect();
        if !hit.is_empty() {
            for c in hit {
                self.remove_clip(c.id);
                if c.start < a {
                    self.add_clip_at(track, c.pattern, c.start, a - c.start, c.offset);
                }
                if b < c.end() {
                    self.add_clip_at(track, c.pattern, b, c.end() - b, c.offset + (b - c.start));
                }
            }
            return;
        }
        let nearest = self.clips.iter().filter(|c| c.track == track).min_by_key(|c| if c.end() <= a { a - c.end() } else { c.start - b });
        let pattern = nearest.map_or(fallback, |c| c.pattern);
        let Some(len) = self.pattern(pattern).map(|p| p.len) else { return };
        let offset = if len > bl && len % bl == 0 { a % len } else { 0 };
        self.add_clip_at(track, pattern, a, bl, offset);
        self.join_clips(track);
    }

    /// Joins clips on `track` that sit end to start and continue the same pattern.
    fn join_clips(&mut self, track: TrackId) {
        loop {
            let on_track = || self.clips.iter().filter(|c| c.track == track);
            let pair = on_track().find_map(|x| on_track().find(|y| y.pattern == x.pattern && x.end() == y.start && x.offset + x.len == y.offset).map(|y| (x.id, *y)));
            let Some((keep, next)) = pair else { return };
            self.remove_clip(next.id);
            if let Some(c) = self.clips.iter_mut().find(|c| c.id == keep) {
                c.len += next.len;
            }
        }
    }

    /// The built-in instruments (Kick, Snare, Hat, Keys, Bass, Lead) on mixer tracks Drums, Keys,
    /// Bass and Lead.
    fn add_instruments(&mut self) -> Vec<ChannelId> {
        let mix: Vec<MixerId> = ["Drums", "Keys", "Bass", "Lead"].iter().filter_map(|n| self.add_mixer_track(n)).collect();
        let channels = [("Kick", Instrument::Kick, 0), ("Snare", Instrument::Snare, 0), ("Hat", Instrument::Hat, 0), ("Keys", Instrument::Rhodes, 1), ("Bass", Instrument::Bass, 2), ("Lead", Instrument::Rhodes, 3)];
        channels.iter().filter_map(|&(name, inst, m)| self.add_channel(name, inst, mix[m])).collect()
    }

    /// A new, empty song: the built-in instruments, one empty one-bar pattern and three empty
    /// playlist tracks.
    pub fn blank() -> Project {
        let mut p = Project { bpm: 90.0, ..Project::default() };
        p.add_instruments();
        p.add_pattern("Pattern 1", p.bar_len());
        for i in 1..=3 {
            p.add_track(&format!("Track {i}"));
        }
        p
    }

    /// A short lo-fi loop: drums, four chords with bass, and a melody, over four bars.
    pub fn demo() -> Project {
        let mut p = Project { bpm: 80.0, swing: 0.6, master: Fx { tone: 0.55, crush: 0.2, wobble: 0.35, vinyl: 0.4, reverb: 0.3, volume: 0.8 }, ..Project::default() };
        let bar = p.bar_len();
        let ch = p.add_instruments();

        let drums = p.add_pattern("Pattern 1", bar);
        for (c, row) in ch.iter().zip(["x......x..x.....", "....x.......x...", "x.x.x.x.x.x.x.x."]) {
            for (step, _) in row.char_indices().filter(|&(_, x)| x == 'x') {
                p.toggle_step(drums, *c, step as Tick);
            }
        }
        let chords = p.add_pattern("Chords", 4 * bar);
        for (i, chord) in [1, 2, 3, 4].into_iter().enumerate() {
            p.stamp_chord(chords, i as Tick * bar, bar, chord);
        }
        let melody = p.add_pattern("Melody", 4 * bar);
        for (pitch, start, len) in [(72, 20, 2), (69, 24, 2), (67, 28, 4), (72, 52, 2), (74, 56, 2), (76, 60, 4)] {
            p.add_note(melody, ch[5], pitch, start * STEP, len * STEP, DEFAULT_VEL);
        }

        let (t_chords, t_drums, t_melody) = (p.add_track("Chords"), p.add_track("Drums"), p.add_track("Melody"));
        p.add_clip(t_chords, chords, 0, 4 * bar);
        for b in 0..4 {
            p.add_clip(t_drums, drums, b * bar, bar);
        }
        p.add_clip(t_melody, melody, 0, 4 * bar);
        p
    }
}

/// Managing patterns and tracks.
impl Project {
    /// The first of `names` not in `taken`.
    fn free_name<'a>(mut names: impl Iterator<Item = String>, taken: impl Iterator<Item = &'a str> + Clone) -> String {
        names.find(|name| !taken.clone().any(|t| t == name)).expect("names never run out")
    }

    /// A new empty one-bar pattern, named "Pattern N" with the lowest free N.
    pub fn new_pattern(&mut self) -> PatternId {
        let name = Self::free_name((1..).map(|n| format!("Pattern {n}")), self.patterns.iter().map(|p| p.name.as_str()));
        self.add_pattern(&name, self.bar_len())
    }

    /// A copy of a pattern (its notes get new IDs), named "<name> copy".
    pub fn clone_pattern(&mut self, id: PatternId) -> Option<PatternId> {
        let p = self.pattern(id)?.clone();
        let base = format!("{} copy", p.name);
        let names = std::iter::once(base.clone()).chain((2..).map(|n| format!("{base} {n}")));
        let name = Self::free_name(names, self.patterns.iter().map(|p| p.name.as_str()));
        let copy = self.add_pattern(&name, p.len);
        for n in &p.notes {
            self.add_note(copy, n.channel, n.pitch, n.start, n.len, n.vel);
        }
        Some(copy)
    }

    /// A new empty playlist track, named "Track N".
    pub fn new_track(&mut self) -> TrackId {
        let name = Self::free_name((1..).map(|n| format!("Track {n}")), self.tracks.iter().map(|t| t.name.as_str()));
        self.add_track(&name)
    }
}

/// Chop sampler operations. A chop channel's slices sit end to end, so "chop points" (the slice
/// starts) describe them; per-slice settings stay with the slice's position.
impl Project {
    /// The chop settings of a chop channel.
    pub fn chop(&self, channel: ChannelId) -> Option<&Chop> {
        self.channel(channel).and_then(|c| c.chop.as_ref())
    }

    fn chop_mut(&mut self, channel: ChannelId) -> Option<&mut Chop> {
        self.channels.iter_mut().find(|c| c.id == channel).and_then(|c| c.chop.as_mut())
    }

    /// Adds a chop channel for the audio at `path` (`len` frames), as one slice covering it all.
    /// It routes to a "Chops" mixer track, added if there is room.
    pub fn add_chop_channel(&mut self, name: &str, path: PathBuf, len: u32) -> Option<ChannelId> {
        let mixer = match self.mixer.iter().find(|m| m.name == "Chops") {
            Some(m) => m.id,
            None => self.add_mixer_track("Chops").or(self.mixer.first().map(|m| m.id))?,
        };
        let id = self.add_channel(name, Instrument::Chop, mixer)?;
        let chop = Chop { path, len, slices: Vec::new(), one_shot: true, cut_itself: true };
        self.channels.last_mut()?.chop = Some(chop);
        self.set_chop_points(id, &[0]);
        Some(id)
    }

    /// Where each slice starts.
    pub fn chop_points(&self, channel: ChannelId) -> Vec<u32> {
        self.chop(channel).map_or(Vec::new(), |c| c.slices.iter().map(|s| s.start).collect())
    }

    /// Re-chops at `points` (sorted, deduplicated, inside the sample, at most `MAX_SLICES`).
    /// Slice `k` keeps the pitch, reverse and level slice `k` had.
    pub fn set_chop_points(&mut self, channel: ChannelId, points: &[u32]) {
        let Some(c) = self.chop_mut(channel) else { return };
        let mut pts: Vec<u32> = points.iter().copied().filter(|&p| p < c.len).collect();
        pts.sort_unstable();
        pts.dedup();
        pts.truncate(MAX_SLICES);
        let old = std::mem::take(&mut c.slices);
        for (k, &start) in pts.iter().enumerate() {
            let end = pts.get(k + 1).copied().unwrap_or(c.len);
            let keep = old.get(k).copied().unwrap_or(Slice { start, end, pitch: 0.0, reverse: false, gain: 1.0 });
            c.slices.push(Slice { start, end, ..keep });
        }
    }

    /// Adds a chop point at `at`, splitting the slice there. False if there are already
    /// `MAX_SLICES` slices or a point is already there.
    pub fn add_chop_point(&mut self, channel: ChannelId, at: u32) -> bool {
        let mut pts = self.chop_points(channel);
        if pts.len() >= MAX_SLICES || pts.contains(&at) || self.chop(channel).is_none_or(|c| at >= c.len) {
            return false;
        }
        // Keep the settings of later slices with them: insert an empty slot at the new index.
        let k = pts.partition_point(|&p| p < at);
        pts.insert(k, at);
        if let Some(c) = self.chop_mut(channel) {
            let fresh = Slice { start: at, end: at, pitch: 0.0, reverse: false, gain: 1.0 };
            c.slices.insert(k, fresh);
        }
        self.set_chop_points(channel, &pts);
        true
    }

    /// Removes chop point `k`, joining its slice to the one before (the first point moves the
    /// start of the sample instead of disappearing when it is the only one).
    pub fn remove_chop_point(&mut self, channel: ChannelId, k: usize) {
        let mut pts = self.chop_points(channel);
        if k >= pts.len() || pts.len() == 1 {
            return;
        }
        pts.remove(k);
        if let Some(c) = self.chop_mut(channel) {
            c.slices.remove(k);
        }
        self.set_chop_points(channel, &pts);
    }

    /// Drags chop point `k` to `to`, kept between its neighbours.
    pub fn move_chop_point(&mut self, channel: ChannelId, k: usize, to: u32) {
        let pts = self.chop_points(channel);
        let Some(c) = self.chop_mut(channel) else { return };
        if k >= pts.len() {
            return;
        }
        let lo = if k == 0 { 0 } else { pts[k - 1] + 1 };
        let hi = pts.get(k + 1).map_or(c.len, |&p| p).saturating_sub(1);
        let to = to.clamp(lo, hi.max(lo));
        c.slices[k].start = to;
        if k > 0 {
            c.slices[k - 1].end = to;
        }
    }

    /// Slice `k` of a chop channel, to change its pitch, reverse or level.
    pub fn slice_mut(&mut self, channel: ChannelId, k: usize) -> Option<&mut Slice> {
        self.chop_mut(channel).and_then(|c| c.slices.get_mut(k))
    }
}

/// Removes the items matching `f` and returns how many were removed.
fn remove<T>(v: &mut Vec<T>, f: impl Fn(&T) -> bool) -> usize {
    let before = v.len();
    v.retain(|x| !f(x));
    before - v.len()
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
        let m = p.add_mixer_track("Main").unwrap();
        let ch = p.add_channel("Kick", Instrument::Kick, m).unwrap();
        let pat = p.add_pattern("Beat", BAR);
        p.add_note(pat, ch, STEP_PITCH, 0, STEP, DEFAULT_VEL).unwrap();
        let tr = p.add_track("Drums");
        let clip = p.add_clip(tr, pat, 0, BAR).unwrap();
        (p, ch, pat, tr, clip)
    }

    fn track_named(p: &Project, name: &str) -> TrackId {
        p.tracks.iter().find(|t| t.name == name).unwrap().id
    }

    fn clips_on(p: &Project, track: TrackId) -> Vec<(Tick, Tick, Tick)> {
        let mut v: Vec<_> = p.clips.iter().filter(|c| c.track == track).map(|c| (c.start, c.len, c.offset)).collect();
        v.sort();
        v
    }

    #[test]
    fn chord_table_is_sane() {
        for c in &CHORDS[1..] {
            assert!(c.notes.iter().all(|&n| (36..=96).contains(&n)), "{}", c.name);
            assert!((24..60).contains(&c.bass), "{}", c.name);
        }
    }

    #[test]
    fn demo_validates_and_compiles() {
        let p = Project::demo();
        p.validate().unwrap();
        assert_eq!((p.mixer.len(), p.channels.len(), p.patterns.len(), p.tracks.len(), p.clips.len()), (4, 6, 3, 3, 6));
        let chords = named(&p, "Chords").id;
        let found: Vec<usize> = (0..4).map(|b| p.chord_at(chords, b * BAR)).collect();
        assert_eq!(found, [1, 2, 3, 4]);
        let s = p.compile();
        // 13 drum hits x 4 bars, 4 chords of 5 notes, 6 melody notes; 4 bars long.
        assert_eq!(s.notes.len(), 13 * 4 + 20 + 6);
        assert_eq!((s.loop_start, s.loop_end, s.bpm), (0, 4 * BAR, 80.0));
        assert!(s.notes.windows(2).all(|w| w[0].start <= w[1].start));
        assert!(s.notes.iter().filter(|n| n.channel == 3).all(|n| n.vel == CHORD_VEL));
    }

    #[test]
    fn blank_project_is_valid_and_silent() {
        let p = Project::blank();
        p.validate().unwrap();
        assert_eq!((p.channels.len(), p.mixer.len(), p.patterns.len(), p.tracks.len(), p.clips.len()), (6, 4, 1, 3, 0));
        let s = p.compile();
        assert!(s.notes.is_empty());
        assert_eq!((s.loop_start, s.loop_end), (0, BAR));
    }

    #[test]
    fn chop_channels_and_points() {
        let mut p = Project::demo();
        let ch = p.add_chop_channel("Break", PathBuf::from("/samples/break.wav"), 1000).unwrap();
        p.validate().unwrap();
        assert_eq!(p.chop_points(ch), [0]);
        assert_eq!(p.mixer_track(p.channel(ch).unwrap().mixer).unwrap().name, "Chops");
        let slices = |p: &Project| p.chop(ch).unwrap().slices.iter().map(|s| (s.start, s.end, s.pitch)).collect::<Vec<_>>();

        // Points outside the sample and duplicates are dropped; slices sit end to end.
        p.set_chop_points(ch, &[750, 0, 250, 250, 500, 5000]);
        assert_eq!(slices(&p), [(0, 250, 0.0), (250, 500, 0.0), (500, 750, 0.0), (750, 1000, 0.0)]);
        p.slice_mut(ch, 1).unwrap().pitch = 5.0;
        // Adding a point keeps each slice's settings with it.
        assert!(p.add_chop_point(ch, 100));
        assert!(!p.add_chop_point(ch, 100) && !p.add_chop_point(ch, 1000));
        assert_eq!(slices(&p), [(0, 100, 0.0), (100, 250, 0.0), (250, 500, 5.0), (500, 750, 0.0), (750, 1000, 0.0)]);
        p.remove_chop_point(ch, 1);
        assert_eq!(slices(&p), [(0, 250, 0.0), (250, 500, 5.0), (500, 750, 0.0), (750, 1000, 0.0)]);
        // Dragging stays between the neighbours.
        p.move_chop_point(ch, 2, 100);
        p.move_chop_point(ch, 3, 2000);
        assert_eq!(slices(&p), [(0, 250, 0.0), (250, 251, 5.0), (251, 999, 0.0), (999, 1000, 0.0)]);
        p.validate().unwrap();

        // At most MAX_SLICES pads; the last point can't be removed.
        p.set_chop_points(ch, &(0..40).map(|k| k * 20).collect::<Vec<_>>());
        assert_eq!(p.chop_points(ch).len(), MAX_SLICES);
        assert!(!p.add_chop_point(ch, 999));
        p.set_chop_points(ch, &[0]);
        p.remove_chop_point(ch, 0);
        assert_eq!(p.chop_points(ch), [0]);
        p.validate().unwrap();
        // Not a chop channel: nothing happens.
        let kick = channel_named(&p, "Kick");
        p.set_chop_points(kick, &[0, 10]);
        assert!(p.chop(kick).is_none() && p.chop_points(kick).is_empty());
    }

    #[test]
    fn chops_compile_with_their_sample() {
        let mut p = Project::demo();
        let ch = p.add_chop_channel("Break", PathBuf::from("/samples/break.wav"), 1000).unwrap();
        p.set_chop_points(ch, &[0, 500]);
        p.slice_mut(ch, 1).unwrap().pitch = 12.0;
        p.slice_mut(ch, 1).unwrap().reverse = true;
        let beat = named(&p, "Pattern 1").id;
        p.add_note(beat, ch, CHOP_BASE + 1, 0, STEP, 100).unwrap();
        let data = Arc::new(SampleData { sr: 44100, frames: vec![[0.0; 2]; 1000] });
        let s = p.compile_with(&|path| (path == Path::new("/samples/break.wav")).then(|| data.clone()));
        let i = p.channel_index(ch).unwrap();
        assert_eq!(s.channels[i].chop, Some(0));
        let c = &s.chops[0];
        assert!(Arc::ptr_eq(c.sample.as_ref().unwrap(), &data));
        assert_eq!((c.slices[1].start, c.slices[1].end, c.slices[1].reverse), (500, 1000, true));
        assert!((c.slices[1].speed - 2.0).abs() < 1e-6);
        assert!(s.notes.iter().any(|n| n.channel as usize == i && n.pitch == CHOP_BASE + 1));
        // Without the audio the channel is still there, silent.
        assert!(p.compile().chops[0].sample.is_none());
        assert!(s.channels.iter().filter(|c| c.instrument != Instrument::Chop).all(|c| c.chop.is_none()));
    }

    #[test]
    fn validate_checks_chops() {
        let mut p = Project::demo();
        let ch = p.add_chop_channel("Break", PathBuf::from("b.wav"), 1000).unwrap();
        p.set_chop_points(ch, &[0, 500]);
        p.validate().unwrap();
        let i = p.channel_index(ch).unwrap();
        let broken = |f: &dyn Fn(&mut Project)| {
            let mut q = p.clone();
            f(&mut q);
            q.validate().unwrap_err()
        };
        broken(&|q| q.channels[i].chop.as_mut().unwrap().slices[0].end = 400); // gap
        broken(&|q| q.channels[i].chop.as_mut().unwrap().slices[1].end = 2000); // past the end
        broken(&|q| q.channels[i].chop.as_mut().unwrap().slices.clear());
        broken(&|q| q.channels[i].chop.as_mut().unwrap().slices[0].pitch = 100.0);
        broken(&|q| q.channels[i].chop = None);
        broken(&|q| q.channels[0].chop = q.channels[i].chop.clone());
    }

    #[test]
    fn patterns_new_clone_delete() {
        let mut p = Project::demo();
        let a = p.new_pattern();
        let b = p.new_pattern();
        let names: Vec<&str> = p.patterns.iter().map(|x| x.name.as_str()).collect();
        assert_eq!(names, ["Pattern 1", "Chords", "Melody", "Pattern 2", "Pattern 3"]);
        assert!(p.pattern(a).unwrap().notes.is_empty() && p.pattern(b).unwrap().len == BAR);

        let melody = named(&p, "Melody").id;
        let copy = p.clone_pattern(melody).unwrap();
        let (orig, dup) = (p.pattern(melody).unwrap(), p.pattern(copy).unwrap());
        assert_eq!(dup.name, "Melody copy");
        assert_eq!((dup.len, dup.notes.len()), (orig.len, orig.notes.len()));
        let same = |x: &Note, y: &Note| (x.channel, x.pitch, x.start, x.len, x.vel) == (y.channel, y.pitch, y.start, y.len, y.vel);
        assert!(orig.notes.iter().zip(&dup.notes).all(|(x, y)| same(x, y) && x.id != y.id));
        let copy2 = p.clone_pattern(melody).unwrap();
        assert_eq!(p.pattern(copy2).unwrap().name, "Melody copy 2");
        assert!(p.clone_pattern(PatternId(9999)).is_none());
        p.validate().unwrap();

        // Deleting a pattern takes its clips off the playlist.
        assert!(p.remove_pattern(melody));
        assert!(p.clips.iter().all(|c| c.pattern != melody));
        p.validate().unwrap();

        let t = p.new_track();
        assert_eq!(p.track(t).unwrap().name, "Track 1");
        let t2 = p.new_track();
        assert_eq!(p.track(t2).unwrap().name, "Track 2");
        let mut q = Project::default();
        let first = q.new_pattern();
        assert_eq!(q.pattern(first).unwrap().name, "Pattern 1");
    }

    #[test]
    fn steps_toggle() {
        let (mut p, ch, pat, ..) = tiny();
        assert!(p.step(pat, ch, 0) && !p.step(pat, ch, 3));
        p.toggle_step(pat, ch, 3);
        assert!(p.step(pat, ch, 3));
        assert!(p.pattern(pat).unwrap().notes.iter().any(|n| (n.start, n.len, n.pitch) == (3 * STEP, STEP, STEP_PITCH)));
        p.toggle_step(pat, ch, 3);
        p.toggle_step(pat, ch, 0);
        assert!(p.pattern(pat).unwrap().notes.is_empty());
        p.validate().unwrap();
    }

    #[test]
    fn cells_join_and_split() {
        let (mut p, ch, pat, ..) = tiny();
        let notes = |p: &Project| {
            let mut v: Vec<(Tick, Tick, u8)> = p.pattern(pat).unwrap().notes.iter().filter(|n| n.pitch == 72).map(|n| (n.start / STEP, n.len / STEP, n.vel)).collect();
            v.sort();
            v
        };
        p.set_cell(pat, ch, 72, 0, true);
        p.pattern_mut(pat).unwrap().notes.iter_mut().filter(|n| n.pitch == 72).for_each(|n| n.vel = 77);
        p.set_cell(pat, ch, 72, 2, true);
        p.set_cell(pat, ch, 72, 1, true); // joins 0 and 2, keeping the velocity
        assert_eq!(notes(&p), [(0, 3, 77)]);
        p.set_cell(pat, ch, 72, 1, true); // already on
        assert_eq!(notes(&p), [(0, 3, 77)]);
        p.set_cell(pat, ch, 72, 1, false); // splits
        assert_eq!(notes(&p), [(0, 1, 77), (2, 1, 77)]);
        assert!(p.cell(pat, ch, 72, 2) && !p.cell(pat, ch, 72, 1));
        p.set_cell(pat, ch, 72, 0, false);
        p.set_cell(pat, ch, 72, 2, false);
        p.set_cell(pat, ch, 72, 5, false); // already off
        assert_eq!(notes(&p), []);
        p.validate().unwrap();
    }

    #[test]
    fn chords_stamp_and_detect() {
        let mut p = Project::demo();
        let (chords, melody) = (named(&p, "Chords").id, named(&p, "Melody").id);
        let melody_before = named(&p, "Melody").notes.clone();
        assert!(p.stamp_chord(chords, BAR, BAR, 6)); // Em7 on bar 2
        assert_eq!(p.chord_at(chords, BAR), 6);
        let bass = channel_named(&p, "Bass");
        let bass_notes: Vec<u8> = named(&p, "Chords").notes.iter().filter(|n| n.channel == bass && n.start == BAR).map(|n| n.pitch).collect();
        assert_eq!(bass_notes, [CHORDS[6].bass]);
        assert!(p.stamp_chord(chords, BAR, BAR, 0)); // clear
        assert_eq!(p.chord_at(chords, BAR), 0);
        assert!(!named(&p, "Chords").notes.iter().any(|n| (BAR..2 * BAR).contains(&n.start)));
        assert_eq!((p.chord_at(chords, 0), p.chord_at(chords, 2 * BAR)), (1, 3));
        assert_eq!(p.pattern(melody).unwrap().notes, melody_before);
        p.validate().unwrap();

        let (mut q, _, pat, ..) = tiny(); // drums only: nowhere to put a chord
        assert!(!q.stamp_chord(pat, 0, BAR, 1));
    }

    #[test]
    fn bars_toggle_on_the_playlist() {
        let mut p = Project::demo();
        let (melody, drums, beat) = (track_named(&p, "Melody"), track_named(&p, "Drums"), named(&p, "Pattern 1").id);
        // Cutting bar 2 out of the 4-bar melody clip leaves bars 1 and 3-4, at the right offsets.
        p.toggle_bar(melody, 1, beat);
        assert_eq!(clips_on(&p, melody), [(0, BAR, 0), (2 * BAR, 2 * BAR, 2 * BAR)]);
        // Putting it back takes bar 2 of the melody and joins all three again.
        p.toggle_bar(melody, 1, beat);
        assert_eq!(clips_on(&p, melody), [(0, 4 * BAR, 0)]);
        // One-bar drum clips repeat the pattern; they are not joined.
        p.toggle_bar(drums, 0, beat);
        p.toggle_bar(drums, 0, beat);
        assert_eq!(clips_on(&p, drums), [(0, BAR, 0), (BAR, BAR, 0), (2 * BAR, BAR, 0), (3 * BAR, BAR, 0)]);
        // Past the end, a track repeats its nearest pattern; an empty track uses the fallback.
        p.toggle_bar(drums, 6, named(&p, "Chords").id);
        assert_eq!(p.clip_at(drums, 6 * BAR).unwrap().pattern, beat);
        let empty = p.add_track("Empty");
        p.toggle_bar(empty, 2, beat);
        assert_eq!(clips_on(&p, empty), [(2 * BAR, BAR, 0)]);
        assert_eq!(p.compile().loop_end, 7 * BAR);
        p.validate().unwrap();
    }

    #[test]
    fn pattern_position_follows_clips() {
        let mut p = Project::demo();
        let (beat, melody) = (named(&p, "Pattern 1").id, named(&p, "Melody").id);
        assert_eq!(p.pattern_pos(beat, BAR + 3 * STEP), Some(3 * STEP));
        assert_eq!(p.pattern_pos(melody, 2 * BAR + 5), Some(2 * BAR + 5));
        assert_eq!(p.pattern_pos(beat, 4 * BAR), None);
        p.tracks.iter_mut().for_each(|t| t.mute = true);
        assert_eq!(p.pattern_pos(beat, 0), None);
    }

    #[test]
    fn compile_places_clip_notes_in_song_time() {
        let mut p = Project::default();
        let (m, m2) = (p.add_mixer_track("Main").unwrap(), p.add_mixer_track("Keys").unwrap());
        let kick = p.add_channel("Kick", Instrument::Kick, m).unwrap();
        let keys = p.add_channel("Keys", Instrument::Rhodes, m2).unwrap();
        p.channels[1].pan = -1.0;
        let pat = p.add_pattern("P", 2 * BAR);
        p.add_note(pat, kick, STEP_PITCH, 0, STEP, 100).unwrap();
        p.add_note(pat, keys, 60, BAR - STEP, 2 * STEP, 90).unwrap(); // crosses into the pattern's bar 2
        p.add_note(pat, keys, 64, BAR + STEP, STEP, 80).unwrap();
        let t = p.add_track("T");
        p.add_clip(t, pat, 0, BAR).unwrap(); // the pattern's bar 1, at bar 1
        p.add_clip(t, pat, 2 * BAR, BAR).unwrap(); // the pattern's bar 2, at bar 3
        p.clips[1].offset = BAR;
        let s = p.compile();
        assert_eq!((s.channels.len(), s.mixer.len(), s.channels[1].mixer), (2, 2, 1));
        assert!(s.channels[1].gain[1].abs() < 1e-6 && s.channels[1].gain[0] > 0.8);
        assert_eq!((s.bpm, s.swing, s.fx), (p.bpm, p.swing, p.master));
        let got: Vec<(Tick, Tick, u16, u8, u8)> = s.notes.iter().map(|n| (n.start, n.end, n.channel, n.pitch, n.vel)).collect();
        assert_eq!(
            got,
            [
                (0, STEP, 0, STEP_PITCH, 100),
                // Cut at the end of the first clip, and not repeated in the second (it starts
                // before that clip's offset).
                (BAR - STEP, BAR, 1, 60, 90),
                (2 * BAR + STEP, 2 * BAR + 2 * STEP, 1, 64, 80),
            ]
        );
        assert_eq!((s.loop_start, s.loop_end), (0, 3 * BAR));

        // The loop rounds up to whole bars; a loop range overrides it; muted tracks are silent.
        p.add_clip(t, pat, 3 * BAR, STEP).unwrap();
        assert_eq!(p.compile().loop_end, 4 * BAR);
        p.loop_range = Some((BAR, 2 * BAR));
        p.tracks[0].mute = true;
        let s = p.compile();
        assert!(s.notes.is_empty());
        assert_eq!((s.loop_start, s.loop_end), (BAR, 2 * BAR));
        assert_eq!(Project::default().compile().loop_end, BAR);
    }

    #[test]
    fn channel_and_mixer_limits() {
        let mut p = Project::default();
        let m = p.add_mixer_track("0").unwrap();
        for i in 1..MAX_MIXER {
            p.add_mixer_track(&i.to_string()).unwrap();
        }
        assert!(p.add_mixer_track("too many").is_none());
        for i in 0..MAX_CHANNELS {
            p.add_channel(&i.to_string(), Instrument::Hat, m).unwrap();
        }
        assert!(p.add_channel("too many", Instrument::Hat, m).is_none());
        p.validate().unwrap();
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
