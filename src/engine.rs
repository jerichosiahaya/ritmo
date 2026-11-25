//! Audio engine: synthesized drums, FM Rhodes, sine bass, a note sequencer, the mixer and the
//! lo-fi master chain. Pure Rust with no I/O, so the same engine plays live and renders exports.
//!
//! The engine plays a [`Snapshot`]: an immutable, engine-ready copy of the project that the UI
//! builds (`Project::compile`) and sends whole. The engine sends each snapshot it replaces back
//! to the UI thread to be freed there, so the audio thread never allocates or frees memory.

use std::f32::consts::{FRAC_PI_4, SQRT_2, TAU};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, mpsc};

/// Vinyl hiss and crackle fade in on play and out on stop over this many seconds,
/// so a stopped engine is silent.
const VINYL_FADE: f32 = 0.25;

// ---------------------------------------------------------------------------------------------
// Time and the snapshot the engine plays.

/// Master insert: the lo-fi chain, plus master volume.
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct Fx {
    pub tone: f32,
    pub crush: f32,
    pub wobble: f32,
    pub vinyl: f32,
    pub reverb: f32,
    pub volume: f32,
}

/// Master insert that leaves the sound (nearly) untouched.
pub const FX_NEUTRAL: Fx = Fx { tone: 1.0, crush: 0.0, wobble: 0.0, vinyl: 0.0, reverb: 0.0, volume: 0.8 };

pub type Tick = u32;
/// Ticks per quarter note. Divides evenly into 16ths (24) and 8th-note triplets (32).
pub const PPQ: Tick = 96;
/// One 16th note: the step sequencer's grid.
pub const STEP: Tick = PPQ / 4;

/// Most channels and mixer tracks the engine plays; their state is allocated up front.
pub const MAX_CHANNELS: usize = 64;
pub const MAX_MIXER: usize = 16;
/// Meter index of the master; meters `0..MAX_MIXER` are the mixer tracks.
pub const MASTER: usize = MAX_MIXER;
pub const METERS: usize = MAX_MIXER + 1;
const VOICES: usize = 32;
const CHOP_VOICES: usize = 32;
/// Chop pads: pad `k` (from 0) plays at pitch `CHOP_BASE + k`, so pad 1 is C3 (`Z` on the
/// typing keyboard).
pub const CHOP_BASE: u8 = 48;
/// Most slices (pads) one chop channel has.
pub const MAX_SLICES: usize = 16;

/// Built-in instruments.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Instrument {
    Kick,
    Snare,
    Hat,
    /// FM electric piano.
    Rhodes,
    /// Sine bass (one note at a time).
    Bass,
    /// Chop sampler: plays slices of an audio file, one per pad.
    Chop,
}

impl Instrument {
    pub fn is_drum(self) -> bool {
        matches!(self, Instrument::Kick | Instrument::Snare | Instrument::Hat)
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct ChannelMix {
    pub instrument: Instrument,
    /// Volume and pan as left/right gains.
    pub gain: [f32; 2],
    pub mute: bool,
    /// Index into `Snapshot::mixer`.
    pub mixer: usize,
    /// Index into `Snapshot::chops` for `Instrument::Chop` channels.
    pub chop: Option<usize>,
}

/// Decoded audio, stereo, at the file's own sample rate.
#[derive(Clone, PartialEq, Default)]
pub struct SampleData {
    pub sr: u32,
    pub frames: Vec<[f32; 2]>,
}

impl std::fmt::Debug for SampleData {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "SampleData({} frames at {} Hz)", self.frames.len(), self.sr)
    }
}

/// One chop: a slice of the sample played by one pad.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct SliceMix {
    /// Frames of the sample, `start..end`.
    pub start: u32,
    pub end: u32,
    /// Playback speed from the slice's pitch (2.0 = an octave up).
    pub speed: f32,
    pub reverse: bool,
    pub gain: f32,
}

/// A chop channel's sample and slices. The sample is shared with the UI; the engine never drops
/// the last reference because it leaves with the snapshot through the trash queue.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct ChopMix {
    pub sample: Option<Arc<SampleData>>,
    pub slices: Vec<SliceMix>,
    /// Play the whole slice whatever the note length (otherwise the note's end fades it out).
    pub one_shot: bool,
    /// A new pad hit fades out the channel's sounding chops.
    pub cut_itself: bool,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct MixerMix {
    pub gain: [f32; 2],
    pub mute: bool,
}

/// A note placed in song time.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SeqNote {
    pub start: Tick,
    pub end: Tick,
    /// Index into `Snapshot::channels`.
    pub channel: u16,
    pub pitch: u8,
    /// 1..=127; 100 is the instrument's normal level.
    pub vel: u8,
}

/// Everything the engine plays, with indices instead of IDs and every clip's notes placed in
/// song time. Built on the UI thread by `Project::compile`.
#[derive(Clone, PartialEq, Debug)]
pub struct Snapshot {
    pub bpm: f32,
    pub swing: f32,
    pub fx: Fx,
    pub channels: Vec<ChannelMix>,
    pub mixer: Vec<MixerMix>,
    /// Sorted by start.
    pub notes: Vec<SeqNote>,
    pub chops: Vec<ChopMix>,
    /// Playback runs `loop_start..loop_end`, then wraps.
    pub loop_start: Tick,
    pub loop_end: Tick,
    /// Length of a bar; the metronome accents its first beat.
    pub bar: Tick,
}

impl Default for Snapshot {
    fn default() -> Self {
        Snapshot { bpm: 120.0, swing: 0.5, fx: FX_NEUTRAL, channels: Vec::new(), mixer: Vec::new(), notes: Vec::new(), chops: Vec::new(), loop_start: 0, loop_end: 4 * PPQ, bar: 4 * PPQ }
    }
}

pub enum Cmd {
    Snapshot(Box<Snapshot>),
    Play(bool),
    /// Live notes (typing keyboard, piano-roll keys) on a channel index.
    NoteOn { channel: u16, pitch: u8 },
    NoteOff { channel: u16, pitch: u8 },
    /// A click on every beat while playing, louder and higher on the first beat of a bar.
    Metronome(bool),
}

/// Most commands the UI can queue before the audio thread drains them.
pub const CMD_QUEUE: usize = 256;

/// UI → audio command queue. It is bounded, so its buffer is allocated once here and
/// receiving never allocates or frees (an unbounded `mpsc::channel` frees its blocks on the
/// receiving side). Send with `try_send` so the UI never blocks.
pub fn command_channel() -> (mpsc::SyncSender<Cmd>, mpsc::Receiver<Cmd>) {
    mpsc::sync_channel(CMD_QUEUE)
}

/// Audio → UI queue of replaced snapshots, which the UI drops. Every snapshot arrives through the
/// command queue, so this one has room for all of them.
pub fn trash_channel() -> (mpsc::SyncSender<Box<Snapshot>>, mpsc::Receiver<Box<Snapshot>>) {
    mpsc::sync_channel(CMD_QUEUE + 1)
}

pub fn midi_hz(n: u8) -> f32 {
    440.0 * 2f32.powf((n as f32 - 69.0) / 12.0)
}

/// Length in samples of one tick. Swing lengthens the first 16th of each 8th-note pair and
/// shortens the second, keeping the pair's length.
pub fn tick_len(tick: Tick, bpm: f32, swing: f32, sr: f32) -> f64 {
    let pair = 2.0 * sr as f64 * 60.0 / bpm as f64 / 4.0;
    let share = f64::from(if tick % (2 * STEP) < STEP { swing } else { 1.0 - swing });
    pair * share / STEP as f64
}

/// Constant-power pan, unity gain per side at center.
pub fn pan_gains(pan: f32) -> (f32, f32) {
    let a = (pan.clamp(-1.0, 1.0) + 1.0) * FRAC_PI_4;
    (a.cos() * SQRT_2, a.sin() * SQRT_2)
}

// ---------------------------------------------------------------------------------------------
// Sound generators.

struct Rng(u32);
impl Rng {
    /// Uniform in [-1, 1).
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        (self.0 as f32 / u32::MAX as f32) * 2.0 - 1.0
    }
}

#[derive(Clone, Copy, Default)]
struct Voice {
    active: bool,
    gate: bool,
    /// Played from the keyboard; released by NoteOff, not by the sequencer.
    live: bool,
    channel: u16,
    note: u8,
    /// Tick the sequencer releases it at.
    end: Tick,
    freq: f32,
    vel: f32,
    t: f32,
    rel: f32,
    cph: f32,
    mph: f32,
    tph: f32,
}

impl Voice {
    /// 2-operator FM electric piano with a short 14:1 "tine" partial on the attack.
    fn tick(&mut self, dt: f32, rel_coef: f32) -> f32 {
        let env = (-self.t * 1.3).exp() * self.rel * (self.t * 400.0).min(1.0);
        if !self.gate {
            self.rel *= rel_coef;
        }
        if env < 1e-4 && self.t > 0.01 {
            self.active = false;
            return 0.0;
        }
        let index = 1.6 * (-self.t * 3.0).exp() + 0.35;
        let tine = (TAU * self.tph).sin() * 1.5 * (-self.t * 30.0).exp();
        let m = (TAU * self.mph).sin() * index + tine;
        let out = (TAU * self.cph + m).sin() * env * self.vel;
        self.cph = (self.cph + self.freq * dt).fract();
        self.mph = (self.mph + self.freq * dt).fract();
        self.tph = (self.tph + self.freq * 14.0 * dt).fract();
        self.t += dt;
        out
    }
}

/// One sounding chop.
#[derive(Clone, Copy, Default)]
struct ChopVoice {
    active: bool,
    gate: bool,
    live: bool,
    channel: u16,
    /// Index into `Snapshot::chops`.
    chop: u16,
    note: u8,
    end: Tick,
    /// Read position in sample frames; moves by `step` per output frame (negative in reverse).
    pos: f64,
    step: f64,
    /// Where the slice ends (the lower bound when reversed).
    stop: f64,
    gain: f32,
    /// Fade-in and release envelope, 0..1.
    env: f32,
    /// Seconds since the start, for the declick fade-in.
    t: f32,
    /// Fading out after note-off or a cut, per output frame.
    release: f32,
}

impl ChopVoice {
    /// Stereo output for one frame, reading `data` with linear interpolation.
    fn tick(&mut self, data: &SampleData, dt: f32) -> [f32; 2] {
        let len = data.frames.len();
        let remaining = if self.step >= 0.0 { self.stop - self.pos } else { self.pos - self.stop };
        if remaining <= 0.0 || len < 2 || self.env < 1e-4 {
            self.active = false;
            return [0.0; 2];
        }
        // 2 ms fade in, and fade out over the last 3 ms of the slice, so chops never click.
        let edge = (remaining / self.step.abs() * dt as f64 / 0.003).min(1.0) as f32;
        let fade_in = (self.t / 0.002).min(1.0);
        if self.release > 0.0 {
            self.env *= self.release;
        }
        let i = (self.pos.max(0.0) as usize).min(len - 2);
        let frac = (self.pos - i as f64).clamp(0.0, 1.0) as f32;
        let (a, b) = (data.frames[i], data.frames[i + 1]);
        let g = self.gain * self.env * edge * fade_in;
        self.pos += self.step;
        self.t += dt;
        [(a[0] + (b[0] - a[0]) * frac) * g, (a[1] + (b[1] - a[1]) * frac) * g]
    }
}

/// Per-channel state for drums and the mono bass. Rhodes channels use the shared voice pool.
#[derive(Clone, Copy)]
struct ChanState {
    /// Seconds since the last drum hit; 10 or more means silent.
    hit_t: f32,
    hit_vel: f32,
    /// Oscillator phase (kick, bass).
    ph: f32,
    /// Hat noise low-pass state.
    lp: f32,
    // Mono bass.
    note: u8,
    freq: f32,
    t: f32,
    gate: bool,
    live: bool,
    amp: f32,
    vel: f32,
    end: Tick,
}

impl Default for ChanState {
    fn default() -> Self {
        ChanState { hit_t: 10.0, hit_vel: 1.0, ph: 0.0, lp: 0.0, note: 0, freq: 0.0, t: 0.0, gate: false, live: false, amp: 0.0, vel: 1.0, end: 0 }
    }
}

impl ChanState {
    fn render(&mut self, inst: Instrument, dt: f32, rel_coef: f32, rng: &mut Rng) -> f32 {
        let t = self.hit_t;
        let out = match inst {
            Instrument::Kick if t < 0.6 => {
                let f = 45.0 + 110.0 * (-t * 35.0).exp();
                self.ph = (self.ph + f * dt).fract();
                (TAU * self.ph).sin() * (-t * 7.0).exp() * 0.72 * self.hit_vel
            }
            Instrument::Snare if t < 0.4 => (rng.next() * 0.4 * (-t * 22.0).exp() + (TAU * 185.0 * t).sin() * 0.35 * (-t * 35.0).exp()) * 0.8 * self.hit_vel,
            Instrument::Hat => {
                let n = rng.next();
                self.lp += 0.35 * (n - self.lp);
                if t < 0.15 { (n - self.lp) * 0.18 * (-t * 70.0).exp() * self.hit_vel } else { 0.0 }
            }
            Instrument::Bass => {
                if !self.gate {
                    self.amp *= rel_coef;
                }
                if self.amp < 1e-4 {
                    return 0.0;
                }
                let env = (self.t * 200.0).min(1.0) * (0.55 + 0.45 * (-self.t * 2.0).exp()) * self.amp;
                self.ph = (self.ph + self.freq * dt).fract();
                self.t += dt;
                return ((TAU * self.ph).sin() * 1.6).tanh() * 0.35 * env * self.vel;
            }
            _ => 0.0,
        };
        if self.hit_t < 10.0 {
            self.hit_t += dt;
        }
        out
    }
}

struct Comb {
    buf: Vec<f32>,
    i: usize,
    lp: f32,
}

struct Allpass {
    buf: Vec<f32>,
    i: usize,
}

/// Lo-fi master chain for one stereo side.
struct FxChain {
    wow: Vec<f32>,
    wow_i: usize,
    lfo: f32,
    flutter: f32,
    hold: f32,
    hold_n: usize,
    lp1: f32,
    lp2: f32,
    combs: [Comb; 4],
    aps: [Allpass; 2],
    crackle: f32,
    hiss_lp: f32,
    rng: Rng,
}

impl FxChain {
    /// `spread` offsets the reverb delay lengths so left and right decorrelate.
    fn new(sr: f32, spread: usize, seed: u32) -> Self {
        let s = |n: usize| (((n + spread) as f32 * sr / 44100.0) as usize).max(1);
        let comb = |n| Comb { buf: vec![0.0; s(n)], i: 0, lp: 0.0 };
        let ap = |n| Allpass { buf: vec![0.0; s(n)], i: 0 };
        FxChain {
            wow: vec![0.0; (sr * 0.02) as usize],
            wow_i: 0,
            lfo: 0.0,
            flutter: 0.0,
            hold: 0.0,
            hold_n: 0,
            lp1: 0.0,
            lp2: 0.0,
            combs: [comb(1116), comb(1188), comb(1277), comb(1356)],
            aps: [ap(556), ap(441)],
            crackle: 0.0,
            hiss_lp: 0.0,
            rng: Rng(seed),
        }
    }

    fn process(&mut self, x: f32, fx: &Fx, sr: f32) -> f32 {
        let dt = 1.0 / sr;
        // Wow and flutter: modulated delay line, read with linear interpolation.
        let len = self.wow.len();
        self.wow[self.wow_i] = x;
        self.lfo = (self.lfo + 0.6 * dt).fract();
        self.flutter = (self.flutter + 7.0 * dt).fract();
        let d = (0.004 + fx.wobble * (0.0025 * (TAU * self.lfo).sin() + 0.0003 * (TAU * self.flutter).sin())) * sr;
        let pos = self.wow_i as f32 - d + len as f32;
        let (i0, frac) = (pos.floor() as usize % len, pos.fract());
        let mut y = self.wow[i0] * (1.0 - frac) + self.wow[(i0 + 1) % len] * frac;
        self.wow_i = (self.wow_i + 1) % len;

        // Bitcrush: sample-and-hold plus fewer bits.
        let hold = 1 + (fx.crush * 7.0) as usize;
        if self.hold_n.is_multiple_of(hold) {
            let levels = 2f32.powf(16.0 - fx.crush * 10.0);
            self.hold = (y * levels).round() / levels;
        }
        self.hold_n = self.hold_n.wrapping_add(1);
        y = self.hold;

        // Tone: two cascaded one-pole low-passes, 600 Hz .. 18 kHz.
        let fc = 600.0 * 30f32.powf(fx.tone);
        let a = 1.0 - (-TAU * fc / sr).exp();
        self.lp1 += a * (y - self.lp1);
        self.lp2 += a * (self.lp1 - self.lp2);
        y = self.lp2;

        // Schroeder reverb.
        let input = y * 0.15;
        let mut wet = 0.0;
        for c in &mut self.combs {
            let out = c.buf[c.i];
            c.lp = out * 0.8 + c.lp * 0.2;
            c.buf[c.i] = input + c.lp * 0.84;
            c.i = (c.i + 1) % c.buf.len();
            wet += out;
        }
        for ap in &mut self.aps {
            let b = ap.buf[ap.i];
            ap.buf[ap.i] = wet + b * 0.5;
            wet = b - wet;
            ap.i = (ap.i + 1) % ap.buf.len();
        }
        y += wet * fx.reverb * 1.5;

        // Vinyl: low hiss plus random crackle pops.
        let n = self.rng.next();
        self.hiss_lp += 0.05 * (n - self.hiss_lp);
        if self.rng.next() * 0.5 + 0.5 < fx.vinyl * 0.0004 {
            self.crackle = self.rng.next() * 0.5 * fx.vinyl;
        }
        y += self.hiss_lp * 0.03 * fx.vinyl + self.crackle;
        self.crackle *= 0.6;

        (y * fx.volume * 1.4).tanh()
    }
}

// ---------------------------------------------------------------------------------------------
// The engine.

pub struct Engine {
    sr: f32,
    dt: f32,
    snap: Box<Snapshot>,
    /// Where replaced snapshots go to be freed; `None` drops them in place (offline rendering).
    trash: Option<mpsc::SyncSender<Box<Snapshot>>>,
    /// Snapshots the trash queue had no room for. They are leaked, not freed on this thread.
    pub leaked: usize,
    pub playing: bool,
    /// Tick most recently reached while playing.
    pub tick: Tick,
    next_tick: Tick,
    to_next: f64,
    /// Index of the first note not yet started.
    cursor: usize,
    /// Playback wrapped to the loop start; held notes are released before the next tick.
    wrapped: bool,
    rng: Rng,
    chans: [ChanState; MAX_CHANNELS],
    voices: [Voice; VOICES],
    chop_voices: [ChopVoice; CHOP_VOICES],
    rel_coef: f32,
    /// Per-frame release multipliers for chops: after note-off (10 ms) and when cut (5 ms).
    chop_release: f32,
    chop_cut: f32,
    fx: [FxChain; 2],
    /// 0..1 gain on the vinyl noise: ramps to 1 while playing, to 0 when stopped.
    vinyl_amt: f32,
    peaks: [f32; METERS],
    metronome: bool,
    click: Click,
}

/// The metronome's click: a short decaying sine, added after the master chain.
#[derive(Clone, Copy, Default)]
struct Click {
    t: f32,
    freq: f32,
    amp: f32,
}

/// How fast a click dies away, in seconds (time constant).
const CLICK_DECAY: f32 = 0.012;

impl Engine {
    pub fn new(sr: f32) -> Self {
        Engine {
            sr,
            dt: 1.0 / sr,
            snap: Box::new(Snapshot::default()),
            trash: None,
            leaked: 0,
            playing: false,
            tick: 0,
            next_tick: 0,
            to_next: 0.0,
            cursor: 0,
            wrapped: false,
            rng: Rng(0x1234_5678),
            chans: [ChanState::default(); MAX_CHANNELS],
            voices: [Voice::default(); VOICES],
            chop_voices: [ChopVoice::default(); CHOP_VOICES],
            rel_coef: (-1.0 / (0.25 * sr)).exp(),
            chop_release: (-1.0 / (0.01 * sr)).exp(),
            chop_cut: (-1.0 / (0.005 * sr)).exp(),
            fx: [FxChain::new(sr, 0, 0x9e37_79b9), FxChain::new(sr, 23, 0x7f4a_7c15)],
            vinyl_amt: 0.0,
            peaks: [0.0; METERS],
            metronome: false,
            click: Click::default(),
        }
    }

    /// Live playback: replaced snapshots are sent to `trash` for the UI thread to free.
    pub fn with_trash(mut self, trash: mpsc::SyncSender<Box<Snapshot>>) -> Self {
        self.trash = Some(trash);
        self
    }

    pub fn handle(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Snapshot(s) => {
                let old = std::mem::replace(&mut self.snap, s);
                // Chops whose sample was replaced or removed stop at once.
                for v in self.chop_voices.iter_mut().filter(|v| v.active) {
                    let sample = |s: &Snapshot| s.chops.get(v.chop as usize).and_then(|c| c.sample.as_ref()).map(Arc::as_ptr);
                    if sample(&old).is_none() || sample(&old) != sample(&self.snap) {
                        v.active = false;
                    }
                }
                self.retire(old);
                let n = self.snap.channels.len();
                for v in self.voices.iter_mut().filter(|v| v.channel as usize >= n) {
                    v.gate = false;
                }
                if self.playing {
                    if !(self.snap.loop_start..self.snap.loop_end).contains(&self.next_tick) {
                        self.next_tick = self.snap.loop_start;
                        self.wrapped = true;
                    }
                    self.seek(self.next_tick);
                }
            }
            Cmd::Play(on) => {
                self.playing = on;
                self.next_tick = self.snap.loop_start;
                self.to_next = 0.0;
                self.wrapped = false;
                self.seek(self.next_tick);
                self.release_seq();
            }
            Cmd::NoteOn { channel, pitch } => self.note_on(channel, pitch, 100, Tick::MAX, true),
            Cmd::Metronome(on) => self.metronome = on,
            Cmd::NoteOff { channel, pitch } => {
                for v in self.voices.iter_mut().filter(|v| v.live && v.channel == channel && v.note == pitch) {
                    v.gate = false;
                }
                if let Some(c) = self.chans.get_mut(channel as usize).filter(|c| c.live && c.note == pitch) {
                    c.gate = false;
                }
                for v in self.chop_voices.iter_mut().filter(|v| v.live && v.channel == channel && v.note == pitch) {
                    v.gate = false;
                }
            }
        }
    }

    /// Peak level per meter since the last call, then reset. Index `MASTER` is the master.
    pub fn take_peaks(&mut self) -> [f32; METERS] {
        std::mem::take(&mut self.peaks)
    }

    fn retire(&mut self, old: Box<Snapshot>) {
        let Some(tx) = &self.trash else { return drop(old) };
        if let Err(mpsc::TrySendError::Full(old) | mpsc::TrySendError::Disconnected(old)) = tx.try_send(old) {
            std::mem::forget(old);
            self.leaked += 1;
        }
    }

    fn seek(&mut self, t: Tick) {
        self.cursor = self.snap.notes.partition_point(|n| n.start < t);
    }

    /// Releases every note the sequencer started.
    fn release_seq(&mut self) {
        for v in self.voices.iter_mut().filter(|v| !v.live) {
            v.gate = false;
        }
        for c in self.chans.iter_mut().filter(|c| !c.live) {
            c.gate = false;
        }
        for v in self.chop_voices.iter_mut().filter(|v| !v.live) {
            v.gate = false;
        }
    }

    /// Starts slice `pitch - CHOP_BASE` of the channel's chop.
    fn chop_on(&mut self, channel: u16, chop: usize, pitch: u8, g: f32, end: Tick, live: bool) {
        let Some(c) = self.snap.chops.get(chop) else { return };
        let (Some(data), Some(slice)) = (&c.sample, pitch.checked_sub(CHOP_BASE).and_then(|k| c.slices.get(k as usize))) else { return };
        let (start, stop) = (slice.start.min(slice.end) as f64, (slice.end as usize).min(data.frames.len()) as f64);
        if stop <= start {
            return;
        }
        if c.cut_itself {
            for v in self.chop_voices.iter_mut().filter(|v| v.active && v.channel == channel) {
                v.release = self.chop_cut;
            }
        }
        let step = data.sr as f64 / self.sr as f64 * slice.speed as f64;
        let (pos, step, stop) = if slice.reverse { (stop - 1.0, -step, start) } else { (start, step, stop) };
        // A free voice, otherwise the one closest to silence.
        let i = self.chop_voices.iter().position(|v| !v.active).unwrap_or_else(|| {
            let mut best = 0;
            for (i, v) in self.chop_voices.iter().enumerate() {
                if v.env < self.chop_voices[best].env {
                    best = i;
                }
            }
            best
        });
        self.chop_voices[i] = ChopVoice { active: true, gate: true, live, channel, chop: chop as u16, note: pitch, end, pos, step, stop, gain: slice.gain * g, env: 1.0, t: 0.0, release: 0.0 };
    }

    fn note_on(&mut self, channel: u16, pitch: u8, vel: u8, end: Tick, live: bool) {
        let ch = channel as usize;
        let (Some(mix), Some(st)) = (self.snap.channels.get(ch), self.chans.get_mut(ch)) else { return };
        let g = vel as f32 / 100.0;
        match mix.instrument {
            Instrument::Kick | Instrument::Snare | Instrument::Hat => {
                st.hit_t = 0.0;
                st.hit_vel = g;
            }
            Instrument::Bass => *st = ChanState { note: pitch, freq: midi_hz(pitch), t: 0.0, gate: true, live, amp: 1.0, vel: g, end, ..*st },
            Instrument::Chop => {
                if let Some(chop) = mix.chop {
                    self.chop_on(channel, chop, pitch, g, end, live);
                }
            }
            Instrument::Rhodes => {
                // Free voice first, otherwise steal the oldest.
                let i = self.voices.iter().position(|v| !v.active).unwrap_or_else(|| {
                    let mut best = 0;
                    for (i, v) in self.voices.iter().enumerate() {
                        if v.t > self.voices[best].t {
                            best = i;
                        }
                    }
                    best
                });
                self.voices[i] = Voice { active: true, gate: true, live, channel, note: pitch, end, freq: midi_hz(pitch), vel: 0.3 * g, rel: 1.0, ..Voice::default() };
            }
        }
    }

    /// Ends notes due at tick `t`, then starts the notes that begin there.
    fn fire_tick(&mut self, t: Tick) {
        if self.wrapped {
            self.wrapped = false;
            self.release_seq();
            self.seek(t);
        }
        for v in self.voices.iter_mut().filter(|v| v.gate && !v.live && v.end <= t) {
            v.gate = false;
        }
        for c in self.chans.iter_mut().filter(|c| c.gate && !c.live && c.end <= t) {
            c.gate = false;
        }
        for v in self.chop_voices.iter_mut().filter(|v| v.gate && !v.live && v.end <= t) {
            v.gate = false;
        }
        if self.metronome && t.is_multiple_of(PPQ) {
            let accent = self.snap.bar > 0 && t.is_multiple_of(self.snap.bar);
            self.click = if accent { Click { t: 0.0, freq: 1760.0, amp: 0.5 } } else { Click { t: 0.0, freq: 1320.0, amp: 0.3 } };
        }
        while let Some(&n) = self.snap.notes.get(self.cursor) {
            if n.start > t {
                break;
            }
            self.cursor += 1;
            if n.start == t {
                self.note_on(n.channel, n.pitch, n.vel, n.end, false);
            }
        }
    }

    /// One stereo frame.
    pub fn next_frame(&mut self) -> [f32; 2] {
        if self.playing {
            while self.to_next <= 0.0 {
                let t = self.next_tick;
                self.tick = t;
                self.fire_tick(t);
                self.to_next += tick_len(t, self.snap.bpm, self.snap.swing, self.sr);
                self.next_tick = t + 1;
                if self.next_tick >= self.snap.loop_end {
                    self.next_tick = self.snap.loop_start;
                    self.wrapped = true;
                }
            }
            self.to_next -= 1.0;
        }

        let (dt, rc) = (self.dt, self.rel_coef);
        let n = self.snap.channels.len().min(MAX_CHANNELS);
        let mut ch = [[0.0f32; 2]; MAX_CHANNELS];
        for (i, out) in ch.iter_mut().enumerate().take(n) {
            let x = self.chans[i].render(self.snap.channels[i].instrument, dt, rc, &mut self.rng);
            *out = [x, x];
        }
        for v in self.voices.iter_mut().filter(|v| v.active) {
            let out = v.tick(dt, rc);
            if let Some(c) = ch.get_mut(v.channel as usize) {
                c[0] += out;
                c[1] += out;
            }
        }
        for v in self.chop_voices.iter_mut().filter(|v| v.active) {
            let Some(data) = self.snap.chops.get(v.chop as usize).and_then(|c| c.sample.as_deref()) else {
                v.active = false;
                continue;
            };
            let one_shot = self.snap.chops[v.chop as usize].one_shot;
            if !v.gate && !one_shot && v.release == 0.0 {
                v.release = self.chop_release;
            }
            let [l, r] = v.tick(data, dt);
            if let Some(c) = ch.get_mut(v.channel as usize) {
                c[0] += l;
                c[1] += r;
            }
        }

        let mut bus = [[0.0f32; 2]; MAX_MIXER];
        for (c, &[l, r]) in self.snap.channels.iter().zip(&ch).filter(|(c, _)| !c.mute) {
            let b = &mut bus[c.mixer.min(MAX_MIXER - 1)];
            b[0] += l * c.gain[0];
            b[1] += r * c.gain[1];
        }
        let mut mix = [0.0; 2];
        for (m, (mx, [l, r])) in self.snap.mixer.iter().zip(bus).enumerate().filter(|(_, (mx, _))| !mx.mute) {
            let (l, r) = (l * mx.gain[0], r * mx.gain[1]);
            self.peaks[m] = self.peaks[m].max(l.abs()).max(r.abs());
            mix[0] += l;
            mix[1] += r;
        }

        let ramp = self.dt / VINYL_FADE;
        self.vinyl_amt = if self.playing { (self.vinyl_amt + ramp).min(1.0) } else { (self.vinyl_amt - ramp).max(0.0) };
        let fx = Fx { vinyl: self.snap.fx.vinyl * self.vinyl_amt, ..self.snap.fx };
        let out = [self.fx[0].process(mix[0], &fx, self.sr), self.fx[1].process(mix[1], &fx, self.sr)];
        self.peaks[MASTER] = self.peaks[MASTER].max(out[0].abs()).max(out[1].abs());
        let c = &mut self.click;
        if c.amp > 0.0 {
            let x = c.amp * (-c.t / CLICK_DECAY).exp() * (TAU * c.freq * c.t).sin();
            c.t += self.dt;
            if c.t > 10.0 * CLICK_DECAY {
                c.amp = 0.0;
            }
            return [out[0] + x, out[1] + x];
        }
        out
    }
}

/// Render `loops` passes of the loop range plus a 2 s tail, stereo.
pub fn render_offline(snap: &Snapshot, sr: u32, loops: usize) -> Vec<[f32; 2]> {
    let mut e = Engine::new(sr as f32);
    e.handle(Cmd::Snapshot(Box::new(snap.clone())));
    e.handle(Cmd::Play(true));
    let beats = (snap.loop_end - snap.loop_start) as f64 / PPQ as f64;
    let body = loops as f64 * beats * 60.0 / snap.bpm as f64 * sr as f64;
    let mut out: Vec<[f32; 2]> = (0..body as usize).map(|_| e.next_frame()).collect();
    e.handle(Cmd::Play(false));
    out.extend((0..sr as usize * 2).map(|_| e.next_frame()));
    out
}

pub fn export_wav(path: &std::path::Path, snap: &Snapshot) -> Result<(), hound::Error> {
    let sr = 44100;
    let spec = hound::WavSpec { channels: 2, sample_rate: sr, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
    let mut w = hound::WavWriter::create(path, spec)?;
    for frame in render_offline(snap, sr, 1) {
        for s in frame {
            w.write_sample((s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)?;
        }
    }
    w.finalize()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::Project;
    use crate::rt;

    const UNITY: [f32; 2] = [1.0, 1.0];

    /// The demo project as the engine plays it.
    fn demo() -> Snapshot {
        Project::demo().compile()
    }

    /// One mixer track, the given channels, these notes, a 1-bar loop.
    fn snap(instruments: &[Instrument], notes: &[SeqNote]) -> Snapshot {
        Snapshot {
            channels: instruments.iter().map(|&instrument| ChannelMix { instrument, gain: UNITY, mute: false, mixer: 0, chop: None }).collect(),
            mixer: vec![MixerMix { gain: UNITY, mute: false }],
            notes: notes.to_vec(),
            ..Snapshot::default()
        }
    }

    fn note(channel: u16, pitch: u8, start: Tick, len: Tick) -> SeqNote {
        SeqNote { start, end: start + len, channel, pitch, vel: 100 }
    }

    /// One chop channel playing `data` cut into `slices` (start, end) at normal speed.
    fn chop_snap(data: SampleData, slices: &[(u32, u32)], one_shot: bool, cut_itself: bool) -> Snapshot {
        let mut s = snap(&[Instrument::Chop], &[]);
        s.channels[0].chop = Some(0);
        let slices = slices.iter().map(|&(start, end)| SliceMix { start, end, speed: 1.0, reverse: false, gain: 1.0 }).collect();
        s.chops.push(ChopMix { sample: Some(Arc::new(data)), slices, one_shot, cut_itself });
        s
    }

    fn sine(sr: u32, n: usize, hz: f32) -> Vec<[f32; 2]> {
        (0..n).map(|i| [(std::f32::consts::TAU * hz * i as f32 / sr as f32).sin() * 0.5; 2]).collect()
    }

    /// First and last frame (from now) with sound, within `max` frames.
    fn span(e: &mut Engine, max: usize) -> Option<(usize, usize)> {
        let loud: Vec<usize> = (0..max).filter(|_| e.next_frame()[0].abs() > 1e-3).collect();
        Some((*loud.first()?, *loud.last()?))
    }

    fn engine(s: &Snapshot, sr: f32) -> Engine {
        let mut e = Engine::new(sr);
        e.handle(Cmd::Snapshot(Box::new(s.clone())));
        e.handle(Cmd::Play(true));
        e
    }

    fn peak(e: &mut Engine, n: usize) -> [f32; 2] {
        let mut p = [0f32; 2];
        for _ in 0..n {
            let f = e.next_frame();
            p = [p[0].max(f[0].abs()), p[1].max(f[1].abs())];
        }
        p
    }

    /// Frames until the output first exceeds `level`.
    fn onset(e: &mut Engine, level: f32, max: usize) -> Option<usize> {
        (0..max).find(|_| e.next_frame()[0].abs() > level)
    }

    #[test]
    fn swing_keeps_pair_length() {
        let len = |range: std::ops::Range<Tick>, swing| range.map(|t| tick_len(t, 90.0, swing, 48000.0)).sum::<f64>();
        let pair = 2.0 * 48000.0 * 60.0 / 90.0 / 4.0;
        assert!((len(0..2 * STEP, 0.66) - pair).abs() < 1e-6);
        assert!((len(0..STEP, 0.66) - pair * f64::from(0.66f32)).abs() < 1e-6);
        assert!((len(0..STEP, 0.5) - len(STEP..2 * STEP, 0.5)).abs() < 1e-9);
    }

    #[test]
    fn midi_pitch() {
        assert!((midi_hz(69) - 440.0).abs() < 1e-3);
        assert!((midi_hz(57) - 220.0).abs() < 1e-3);
    }

    #[test]
    fn pan_law() {
        let (l, r) = pan_gains(0.0);
        assert!((l - 1.0).abs() < 1e-6 && (r - 1.0).abs() < 1e-6);
        let (l, r) = pan_gains(-1.0);
        assert!(r.abs() < 1e-6 && l > 1.0);
    }

    #[test]
    fn demo_renders_audible_and_bounded() {
        let out = render_offline(&demo(), 22050, 1);
        let expected = (4.0 * 4.0 * 60.0 / 80.0 * 22050.0) as usize + 44100;
        assert_eq!(out.len(), expected);
        let peak = out.iter().flatten().fold(0f32, |m, s| m.max(s.abs()));
        assert!(out.iter().flatten().all(|s| s.is_finite()));
        assert!(peak > 0.1 && peak <= 1.0, "peak {peak}");
    }

    #[test]
    fn empty_snapshot_is_silent() {
        let out = render_offline(&Snapshot::default(), 22050, 1);
        assert!(out.iter().flatten().all(|s| s.abs() < 1e-3));
        let out = render_offline(&snap(&[Instrument::Kick, Instrument::Rhodes, Instrument::Bass], &[]), 22050, 1);
        assert!(out.iter().flatten().all(|s| s.abs() < 1e-3));
    }

    #[test]
    fn kick_on_tick_zero_sounds_immediately() {
        let s = snap(&[Instrument::Kick], &[note(0, 60, 0, STEP)]);
        assert!(peak(&mut engine(&s, 22050.0), 2205)[0] > 0.1);
    }

    /// Notes start on time: a hit on beat 2 starts one beat after a hit on beat 1; with swing,
    /// the second 16th starts `swing` of the way through the 8th-note pair. (Onsets are compared
    /// with a hit on tick 0 because the master chain's wow delay shifts everything a little.)
    #[test]
    fn notes_start_on_time_with_swing() {
        let sr = 22050.0;
        let at = |s: &Snapshot, start: Tick| {
            let mut s = s.clone();
            s.notes = vec![note(0, 60, start, STEP)];
            onset(&mut engine(&s, sr), 1e-3, 30000).unwrap() as f64
        };
        let mut s = snap(&[Instrument::Kick], &[]);
        let beat = 60.0 / s.bpm as f64 * sr as f64;
        let got = at(&s, PPQ) - at(&s, 0);
        assert!((got - beat).abs() <= 1.0, "beat 2 at {got}, want {beat}");

        s.swing = 0.66;
        let want = 2.0 * sr as f64 * 60.0 / s.bpm as f64 / 4.0 * 0.66;
        let got = at(&s, STEP) - at(&s, 0);
        assert!((got - want).abs() <= 1.0, "swung 16th at {got}, want {want}");
    }

    #[test]
    fn note_end_releases_voice() {
        let s = snap(&[Instrument::Rhodes, Instrument::Bass], &[note(0, 60, 0, STEP), note(1, 36, 0, STEP)]);
        let mut e = engine(&s, 22050.0);
        peak(&mut e, 100);
        assert!(e.voices.iter().any(|v| v.active && v.gate) && e.chans[1].gate);
        // A 16th at 120 BPM is 0.125 s; well after it the gates are closed.
        peak(&mut e, 22050 / 4);
        assert!(e.voices.iter().all(|v| !v.gate) && !e.chans[1].gate);
    }

    #[test]
    fn mute_pan_and_mixer() {
        let mut s = snap(&[Instrument::Kick], &[note(0, 60, 0, STEP)]);
        let (l, r) = pan_gains(-1.0);
        s.channels[0].gain = [l, r];
        let p = peak(&mut engine(&s, 22050.0), 2205);
        assert!(p[0] > 0.1 && p[1] < 1e-3, "{p:?}");
        s.channels[0].mute = true;
        assert!(peak(&mut engine(&s, 22050.0), 2205)[0] < 1e-3);
        s.channels[0].mute = false;
        s.mixer[0].mute = true;
        assert!(peak(&mut engine(&s, 22050.0), 2205)[0] < 1e-3);
        s.mixer[0] = MixerMix { gain: [0.0, 0.0], mute: false };
        assert!(peak(&mut engine(&s, 22050.0), 2205)[0] < 1e-3);
    }

    #[test]
    fn meters_follow_mixer_routing() {
        let mut s = snap(&[Instrument::Kick, Instrument::Rhodes], &[note(1, 72, 0, 2 * STEP)]);
        s.mixer.push(MixerMix { gain: UNITY, mute: false });
        s.channels[1].mixer = 1;
        let mut e = engine(&s, 22050.0);
        assert!(peak(&mut e, 2205)[0] > 0.05);
        let m = e.take_peaks();
        assert!(m[1] > 0.05 && m[0] == 0.0 && m[MASTER] > 0.05, "{m:?}");
        assert_eq!(e.take_peaks(), [0.0; METERS]);
    }

    #[test]
    fn live_notes_play_and_release() {
        let s = snap(&[Instrument::Rhodes, Instrument::Kick, Instrument::Bass], &[]);
        let mut e = Engine::new(22050.0);
        e.handle(Cmd::Snapshot(Box::new(s)));
        e.handle(Cmd::NoteOn { channel: 0, pitch: 60 });
        assert!(peak(&mut e, 2205)[0] > 0.05);
        e.handle(Cmd::NoteOff { channel: 0, pitch: 60 });
        peak(&mut e, 22050 * 3);
        assert!(e.voices.iter().all(|v| !v.active));
        e.handle(Cmd::NoteOn { channel: 1, pitch: 60 });
        assert!(peak(&mut e, 2205)[0] > 0.1);
        e.handle(Cmd::NoteOn { channel: 2, pitch: 36 });
        assert!(peak(&mut e, 2205)[0] > 0.05);
        e.handle(Cmd::NoteOff { channel: 2, pitch: 36 });
        peak(&mut e, 22050 * 3);
        assert!(peak(&mut e, 100)[0] < 1e-3);
        // Out-of-range channels are ignored.
        e.handle(Cmd::NoteOn { channel: 999, pitch: 60 });
        assert!(peak(&mut e, 100)[0] < 1e-3);
    }

    #[test]
    fn export_writes_readable_stereo_wav() {
        let path = std::env::temp_dir().join(format!("ritmo-test-{}.wav", std::process::id()));
        export_wav(&path, &demo()).unwrap();
        let r = hound::WavReader::open(&path).unwrap();
        let spec = r.spec();
        let frames = r.len() / 2;
        std::fs::remove_file(&path).unwrap();
        assert_eq!((spec.channels, spec.sample_rate), (2, 44100));
        // The song once: 4 bars at 80 BPM = 12 s, plus 2 s tail.
        assert_eq!(frames as usize, (12.0 * 44100.0) as usize + 88200);
    }

    #[test]
    fn metronome_clicks_on_every_beat() {
        // 120 BPM at 8 kHz: a beat every 4000 frames; two bars of four beats.
        let sr = 8000;
        let mut s = snap(&[], &[]);
        s.loop_end = 2 * s.bar;
        let run = |on: bool| {
            let mut e = Engine::new(sr as f32);
            e.handle(Cmd::Snapshot(Box::new(s.clone())));
            e.handle(Cmd::Metronome(on));
            e.handle(Cmd::Play(true));
            (0..8 * 4000).map(|_| e.next_frame()[0]).collect::<Vec<f32>>()
        };
        let out = run(true);
        let peak = |a: usize| out[a..a + 200].iter().fold(0f32, |m, x| m.max(x.abs()));
        let beats: Vec<f32> = (0..8).map(|b| peak(b * 4000)).collect();
        // Each beat clicks; the first of each bar is louder; it has died away before the next.
        assert!(beats.iter().all(|&p| p > 0.1), "{beats:?}");
        assert!(beats[0] > beats[1] * 1.3 && beats[4] > beats[5] * 1.3, "{beats:?}");
        assert!((0..8).all(|b| peak(b * 4000 + 1500) < 1e-3));
        assert!(run(false).iter().all(|x| x.abs() < 1e-6));
    }

    #[test]
    fn sequencer_wraps_at_loop_end() {
        let mut s = snap(&[], &[]);
        s.bpm = 160.0;
        s.loop_end = 4 * 4 * PPQ;
        let mut e = engine(&s, 8000.0);
        let total = (4.0 * 4.0 * 60.0 / 160.0 * 8000.0) as usize;
        let mut max = 0;
        for _ in 0..total + 10 {
            e.next_frame();
            max = max.max(e.tick);
        }
        assert_eq!(max, s.loop_end - 1);
        assert_eq!(e.tick, 0);
    }

    #[test]
    fn loop_range_and_held_notes_at_wrap() {
        let mut s = snap(&[Instrument::Rhodes], &[note(0, 60, 0, 8 * PPQ)]);
        (s.loop_start, s.loop_end) = (4 * PPQ, 8 * PPQ);
        s.bpm = 240.0;
        let mut e = engine(&s, 8000.0);
        let mut ticks = Vec::new();
        for _ in 0..8000 * 3 {
            e.next_frame();
            ticks.push(e.tick);
        }
        assert!(ticks.iter().all(|t| (4 * PPQ..8 * PPQ).contains(t)));
        assert!(ticks.windows(2).any(|w| w[1] < w[0]), "never wrapped");
        // The note starts before the loop, so it never plays.
        assert!(e.voices.iter().all(|v| !v.active));
    }

    #[test]
    fn new_snapshot_keeps_position_and_frees_on_ui_thread() {
        let (tx, rx) = trash_channel();
        let s = demo();
        let mut e = Engine::new(22050.0).with_trash(tx);
        e.handle(Cmd::Snapshot(Box::new(s.clone())));
        e.handle(Cmd::Play(true));
        peak(&mut e, 22050);
        let at = e.tick;
        assert!(at > 0);
        let mut s2 = s.clone();
        s2.notes.clear();
        e.handle(Cmd::Snapshot(Box::new(s2)));
        e.next_frame();
        assert!(e.tick >= at && e.tick <= at + 1, "{} vs {at}", e.tick);
        // The initial empty snapshot and the demo came back to be freed here.
        assert_eq!(rx.try_iter().count(), 2);
        assert_eq!(e.leaked, 0);
        // With no notes left, the voices ring out and the song goes quiet (vinyl aside).
        let mut s3 = s.clone();
        s3.notes.clear();
        s3.fx.vinyl = 0.0;
        e.handle(Cmd::Snapshot(Box::new(s3)));
        peak(&mut e, 22050 * 5);
        assert!(peak(&mut e, 22050)[0] < 1e-3);
    }

    #[test]
    fn full_trash_leaks_instead_of_freeing() {
        let (tx, _rx) = mpsc::sync_channel(0);
        let mut e = Engine::new(8000.0).with_trash(tx);
        e.handle(Cmd::Snapshot(Box::default()));
        assert_eq!(e.leaked, 1);
    }

    /// The audio callback's work (drain commands, swap snapshots, render) must not touch the heap.
    #[test]
    fn audio_path_never_allocates_or_frees() {
        const SNAPS: usize = 200;
        let (tx, rx) = command_channel();
        let (trash_tx, trash_rx) = trash_channel();
        let mut base = demo();
        // Plus a chop channel (index 6) with a sample.
        let chop = chop_snap(SampleData { sr: 44100, frames: sine(44100, 44100, 220.0) }, &[(0, 20000), (20000, 44100)], true, true);
        base.channels.push(ChannelMix { chop: Some(0), ..chop.channels[0] });
        base.chops = chop.chops;
        let mut e = Engine::new(22050.0).with_trash(trash_tx);
        // Like the app's UI thread: build snapshots, try_send them, free what comes back.
        let ui = std::thread::spawn(move || {
            let send = |mut cmd: Cmd| loop {
                while trash_rx.try_recv().is_ok() {}
                match tx.try_send(cmd) {
                    Ok(()) => break,
                    Err(mpsc::TrySendError::Full(c)) => {
                        cmd = c;
                        std::thread::yield_now();
                    }
                    Err(e) => panic!("{e}"),
                }
            };
            send(Cmd::Play(true));
            for i in 0..SNAPS {
                let mut s = base.clone();
                s.bpm = 80.0 + i as f32 * 0.1;
                if i % 2 == 0 {
                    s.notes.truncate(i % s.notes.len());
                }
                send(Cmd::Snapshot(Box::new(s)));
                send(Cmd::NoteOn { channel: 5, pitch: 60 + (i % 12) as u8 });
                send(Cmd::NoteOff { channel: 5, pitch: 60 + (i % 12) as u8 });
                send(Cmd::NoteOn { channel: 6, pitch: CHOP_BASE + (i % 2) as u8 });
            }
            trash_rx
        });
        let total = 1 + SNAPS * 4;
        let mut got = 0;
        let before = rt::violations();
        rt::enter();
        while got < total {
            while let Ok(cmd) = rx.try_recv() {
                e.handle(cmd);
                got += 1;
            }
            for _ in 0..64 {
                e.next_frame();
            }
            e.take_peaks();
        }
        rt::exit();
        drop(ui.join().unwrap());
        assert_eq!(rt::violations() - before, 0, "audio path allocated or freed memory");
        assert_eq!(e.leaked, 0);
        assert!(e.playing && (e.snap.bpm - (80.0 + (SNAPS - 1) as f32 * 0.1)).abs() < 1e-4);
    }

    #[test]
    fn chop_pads_play_their_slices() {
        let mut frames = sine(8000, 2000, 400.0);
        frames.extend(vec![[0.0; 2]; 2000]);
        let s = chop_snap(SampleData { sr: 8000, frames }, &[(0, 2000), (2000, 4000)], true, true);
        let hit = |pitch| {
            let mut e = Engine::new(8000.0);
            e.handle(Cmd::Snapshot(Box::new(s.clone())));
            e.handle(Cmd::NoteOn { channel: 0, pitch });
            peak(&mut e, 1500)[0]
        };
        assert!(hit(CHOP_BASE) > 0.1);
        assert!(hit(CHOP_BASE + 1) < 1e-3, "pad 2 is the silent slice");
        assert!(hit(CHOP_BASE + 5) < 1e-3, "no pad 6");
        assert!(hit(CHOP_BASE - 1) < 1e-3);
    }

    #[test]
    fn chop_length_follows_pitch_and_sample_rate() {
        let len = |sample_sr: u32, speed: f32| {
            let mut s = chop_snap(SampleData { sr: sample_sr, frames: sine(sample_sr, 1000, 300.0) }, &[(0, 1000)], true, true);
            s.chops[0].slices[0].speed = speed;
            let mut e = Engine::new(8000.0);
            e.handle(Cmd::Snapshot(Box::new(s)));
            e.handle(Cmd::NoteOn { channel: 0, pitch: CHOP_BASE });
            let (a, b) = span(&mut e, 4000).unwrap();
            (b - a) as f32
        };
        assert!((len(8000, 1.0) - 1000.0).abs() < 30.0, "{}", len(8000, 1.0));
        assert!((len(8000, 2.0) - 500.0).abs() < 30.0, "an octave up plays twice as fast");
        assert!((len(16000, 1.0) - 500.0).abs() < 30.0, "16 kHz audio at 8 kHz");
    }

    #[test]
    fn reversed_chops_start_from_the_end() {
        // Silence, then a burst at the end of the slice.
        let mut frames = vec![[0.0; 2]; 900];
        frames.extend(sine(8000, 100, 400.0));
        let onset_of = |reverse| {
            let mut s = chop_snap(SampleData { sr: 8000, frames: frames.clone() }, &[(0, 1000)], true, true);
            s.chops[0].slices[0].reverse = reverse;
            let mut e = Engine::new(8000.0);
            e.handle(Cmd::Snapshot(Box::new(s)));
            e.handle(Cmd::NoteOn { channel: 0, pitch: CHOP_BASE });
            span(&mut e, 3000).unwrap().0
        };
        let (fwd, rev) = (onset_of(false), onset_of(true));
        assert!(rev + 500 < fwd, "forward {fwd}, reverse {rev}");
    }

    #[test]
    fn gated_chops_stop_with_the_note_and_one_shots_do_not() {
        let tail = |one_shot| {
            let mut s = chop_snap(SampleData { sr: 8000, frames: sine(8000, 8000, 300.0) }, &[(0, 8000)], one_shot, true);
            s.notes = vec![note(0, CHOP_BASE, 0, STEP)]; // a 16th: 1000 frames at 120 BPM
            s.loop_end = 16 * PPQ;
            let mut e = engine(&s, 8000.0);
            peak(&mut e, 2000);
            peak(&mut e, 500)[0]
        };
        assert!(tail(false) < 1e-3);
        assert!(tail(true) > 0.1);
    }

    #[test]
    fn chops_cut_themselves() {
        let voices = |cut| {
            let s = chop_snap(SampleData { sr: 8000, frames: sine(8000, 8000, 300.0) }, &[(0, 4000), (4000, 8000)], true, cut);
            let mut e = Engine::new(8000.0);
            e.handle(Cmd::Snapshot(Box::new(s)));
            e.handle(Cmd::NoteOn { channel: 0, pitch: CHOP_BASE });
            peak(&mut e, 100);
            e.handle(Cmd::NoteOn { channel: 0, pitch: CHOP_BASE + 1 });
            // The cut chop fades to silence in about 50 ms.
            peak(&mut e, 600);
            e.chop_voices.iter().filter(|v| v.active).count()
        };
        assert_eq!(voices(true), 1);
        assert_eq!(voices(false), 2);
    }

    #[test]
    fn chops_keep_stereo() {
        let frames: Vec<[f32; 2]> = sine(8000, 2000, 300.0).into_iter().map(|[l, _]| [l, 0.0]).collect();
        let s = chop_snap(SampleData { sr: 8000, frames }, &[(0, 2000)], true, true);
        let mut e = Engine::new(8000.0);
        e.handle(Cmd::Snapshot(Box::new(s)));
        e.handle(Cmd::NoteOn { channel: 0, pitch: CHOP_BASE });
        let p = peak(&mut e, 1500);
        assert!(p[0] > 0.1 && p[1] < 1e-3, "{p:?}");
    }

    #[test]
    fn replacing_the_sample_stops_its_chops() {
        let s = chop_snap(SampleData { sr: 8000, frames: sine(8000, 8000, 300.0) }, &[(0, 8000)], true, true);
        let sounding = |next: Snapshot| {
            let mut e = Engine::new(8000.0);
            e.handle(Cmd::Snapshot(Box::new(s.clone())));
            e.handle(Cmd::NoteOn { channel: 0, pitch: CHOP_BASE });
            peak(&mut e, 100);
            e.handle(Cmd::Snapshot(Box::new(next)));
            e.chop_voices.iter().any(|v| v.active)
        };
        // Same sample (only the level changed): keeps playing.
        let mut same = s.clone();
        same.channels[0].gain = [0.5, 0.5];
        assert!(sounding(same));
        // A different sample, or none: stops.
        let mut other = s.clone();
        other.chops[0].sample = Some(Arc::new((*s.chops[0].sample.clone().unwrap()).clone()));
        assert!(!sounding(other));
        let mut gone = s.clone();
        gone.chops.clear();
        assert!(!sounding(gone));
    }

    #[test]
    fn vinyl_crackles_while_playing() {
        let mut s = snap(&[], &[]);
        s.fx.vinyl = 1.0;
        assert!(peak(&mut engine(&s, 22050.0), 22050 * 2)[0] > 0.01);
    }

    #[test]
    fn stopped_engine_goes_silent() {
        // Demo song: vinyl on, chords, reverb. Play a while, stop, let tails ring out.
        let mut e = engine(&demo(), 22050.0);
        peak(&mut e, 22050 * 3);
        e.handle(Cmd::Play(false));
        peak(&mut e, 22050 * 3);
        e.take_peaks();
        let p = peak(&mut e, 22050 * 2);
        assert!(p[0] < 1e-3 && p[1] < 1e-3, "{p:?}");
        // The UI keeps redrawing while any meter is above 1e-3.
        assert!(e.take_peaks().iter().all(|&m| m < 1e-3));
        // A fresh engine that never played is silent too.
        assert!(peak(&mut Engine::new(22050.0), 22050)[0] < 1e-3);
    }
}
