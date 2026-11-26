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
const SYNTH_VOICES: usize = 32;
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
    /// Subtractive synth: two oscillators, a resonant low-pass filter and an envelope.
    Synth,
}

impl Instrument {
    pub fn is_drum(self) -> bool {
        matches!(self, Instrument::Kick | Instrument::Snare | Instrument::Hat)
    }
}

/// How a channel sounds. Pitch, tone, attack and release work on every instrument; the rest
/// shape the synth. The defaults leave every other instrument as it was.
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Sound {
    /// Semitones, -24..24. Drums are tuned by it; a chop channel moves all its pads.
    pub pitch: f32,
    /// Low-pass on the channel, 0..1: 1 is open, 0 is 150 Hz.
    pub tone: f32,
    /// Seconds to fade in, 0..2. 0 keeps the instrument's own quick, click-free start.
    pub attack: f32,
    /// How long notes ring after they end (drums: how long hits ring), times the instrument's
    /// own, 0.1..4.
    pub release: f32,
    /// Synth: oscillator shape, 0 saw .. 1 square.
    pub wave: f32,
    /// Synth: the second oscillator's detune in cents, 0..50.
    pub detune: f32,
    /// Synth: filter cutoff (0..1: 40 Hz .. 20 kHz), resonance (0..1) and how far the
    /// envelope opens the filter (0..1).
    pub cutoff: f32,
    pub resonance: f32,
    pub envelope: f32,
    /// Synth: seconds to decay to the sustain level (0.01..4), and that level (0..1).
    pub decay: f32,
    pub sustain: f32,
}

impl Default for Sound {
    fn default() -> Self {
        Sound { pitch: 0.0, tone: 1.0, attack: 0.0, release: 1.0, wave: 0.0, detune: 12.0, cutoff: 0.55, resonance: 0.25, envelope: 0.45, decay: 0.4, sustain: 0.6 }
    }
}

impl Sound {
    /// Whether every value is in its range (for checking loaded projects).
    pub fn is_valid(&self) -> bool {
        let r = |v: f32, lo: f32, hi: f32| v.is_finite() && (lo..=hi).contains(&v);
        r(self.pitch, -24.0, 24.0)
            && r(self.tone, 0.0, 1.0)
            && r(self.attack, 0.0, 2.0)
            && r(self.release, 0.1, 4.0)
            && r(self.wave, 0.0, 1.0)
            && r(self.detune, 0.0, 50.0)
            && r(self.cutoff, 0.0, 1.0)
            && r(self.resonance, 0.0, 1.0)
            && r(self.envelope, 0.0, 1.0)
            && r(self.decay, 0.01, 4.0)
            && r(self.sustain, 0.0, 1.0)
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct ChannelMix {
    pub instrument: Instrument,
    pub sound: Sound,
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
    pub fx: TrackFx,
}

/// A mixer track's effects. The defaults leave the track untouched.
#[derive(Clone, Copy, PartialEq, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TrackFx {
    /// EQ in dB, -12..12: a low shelf at 200 Hz, a bell at 1 kHz, a high shelf at 5 kHz.
    pub low: f32,
    pub mid: f32,
    pub high: f32,
    /// One-knob filter, -1..1: 0 is off; left cuts the highs (down to 150 Hz), right cuts the
    /// lows (up to 5 kHz).
    pub filter: f32,
    /// How much goes to the shared delay and reverb, 0..1.
    pub delay: f32,
    pub reverb: f32,
}

impl TrackFx {
    pub fn is_valid(&self) -> bool {
        let r = |v: f32, lo: f32, hi: f32| v.is_finite() && (lo..=hi).contains(&v);
        r(self.low, -12.0, 12.0) && r(self.mid, -12.0, 12.0) && r(self.high, -12.0, 12.0) && r(self.filter, -1.0, 1.0) && r(self.delay, 0.0, 1.0) && r(self.reverb, 0.0, 1.0)
    }
}

/// The delay and reverb every mixer track can send to.
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Sends {
    /// Delay time in 16ths, 1..16 (3 is a dotted 8th); echoes bounce left and right.
    pub delay_steps: f32,
    /// How much of each echo comes back, 0..0.9.
    pub feedback: f32,
    /// Reverb room size and how dark its tail is, 0..1.
    pub size: f32,
    pub damping: f32,
}

impl Default for Sends {
    fn default() -> Self {
        Sends { delay_steps: 3.0, feedback: 0.4, size: 0.6, damping: 0.4 }
    }
}

impl Sends {
    pub fn is_valid(&self) -> bool {
        let r = |v: f32, lo: f32, hi: f32| v.is_finite() && (lo..=hi).contains(&v);
        r(self.delay_steps, 1.0, 16.0) && r(self.feedback, 0.0, 0.9) && r(self.size, 0.0, 1.0) && r(self.damping, 0.0, 1.0)
    }
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
    pub sends: Sends,
}

impl Default for Snapshot {
    fn default() -> Self {
        Snapshot { bpm: 120.0, swing: 0.5, fx: FX_NEUTRAL, channels: Vec::new(), mixer: Vec::new(), notes: Vec::new(), chops: Vec::new(), loop_start: 0, loop_end: 4 * PPQ, bar: 4 * PPQ, sends: Sends::default() }
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
    /// Where playback starts (and Stop returns to); None: the loop start. While playing, jumps
    /// there at once.
    Seek(Option<Tick>),
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
    fn tick(&mut self, dt: f32, k: &ChanCoefs) -> f32 {
        let (rel_coef, attack) = (k.release, k.attack.max(1.0 / 400.0));
        let env = (-self.t * 1.3).exp() * self.rel * (self.t / attack).min(1.0);
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
    /// Fade-in, seconds.
    attack: f32,
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
        let fade_in = (self.t / self.attack.max(0.002)).min(1.0);
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

/// A channel's `Sound` as per-sample numbers, worked out when a snapshot arrives so the audio
/// path only multiplies.
#[derive(Clone, Copy)]
struct ChanCoefs {
    /// Frequency ratio from the pitch setting.
    ratio: f32,
    /// One-pole coefficient of the tone low-pass (1 passes everything).
    tone: f32,
    /// Fade-in, seconds.
    attack: f32,
    /// Per-sample multiplier of a released note's level (for the channel's instrument).
    release: f32,
    /// Drums: how much longer hits ring.
    ring: f32,
    /// Synth: per-sample multipliers toward the sustain level and of the filter envelope; the
    /// filter's base cutoff (Hz), how many octaves the envelope opens it, its damping, the second
    /// oscillator's frequency ratio.
    decay: f32,
    cutoff: f32,
    env_octaves: f32,
    damping: f32,
    detune: f32,
}

impl ChanCoefs {
    fn new(s: &Sound, inst: Instrument, sr: f32) -> Self {
        let per_sample = |secs: f32| (-1.0 / (secs.max(1e-4) * sr)).exp();
        // How long each instrument's notes ring after they end, before the release setting.
        let base = match inst {
            Instrument::Chop => 0.01,
            Instrument::Synth => 0.3,
            _ => 0.25,
        };
        let fc = 150.0 * (20000.0f32 / 150.0).powf(s.tone);
        ChanCoefs {
            ratio: 2f32.powf(s.pitch / 12.0),
            tone: if s.tone >= 0.999 { 1.0 } else { 1.0 - (-TAU * fc / sr).exp() },
            attack: s.attack,
            release: per_sample(base * s.release),
            ring: s.release,
            decay: per_sample(s.decay / 4.0),
            cutoff: 40.0 * 500f32.powf(s.cutoff),
            env_octaves: s.envelope * 7.0,
            damping: 2.0 - 1.9 * s.resonance,
            detune: 2f32.powf(s.detune / 1200.0),
        }
    }
}

/// A smoothed step for a band-limited oscillator edge at phase 0 (PolyBLEP).
fn blep(ph: f32, inc: f32) -> f32 {
    if ph < inc {
        let x = ph / inc;
        2.0 * x - x * x - 1.0
    } else if ph > 1.0 - inc {
        let x = (ph - 1.0) / inc;
        x * x + 2.0 * x + 1.0
    } else {
        0.0
    }
}

/// One synth note: two oscillators (saw to square), a resonant low-pass whose envelope opens it
/// at the start of the note, and an attack-decay-sustain-release level.
#[derive(Clone, Copy, Default)]
struct SynthVoice {
    active: bool,
    gate: bool,
    live: bool,
    channel: u16,
    note: u8,
    end: Tick,
    freq: f32,
    vel: f32,
    t: f32,
    level: f32,
    /// The filter envelope, from 1 down to 0.
    fenv: f32,
    ph: [f32; 2],
    /// Filter state (two integrators).
    ic: [f32; 2],
}

impl SynthVoice {
    fn tick(&mut self, s: &Sound, k: &ChanCoefs, dt: f32, sr: f32) -> f32 {
        let attack = k.attack.max(0.002);
        if !self.gate {
            self.level *= k.release;
            if self.level < 1e-4 {
                self.active = false;
                return 0.0;
            }
        } else if self.t < attack {
            self.level = self.level.max(self.t / attack);
        } else {
            self.level = s.sustain + (self.level - s.sustain) * k.decay;
        }
        let osc = |ph: f32, inc: f32| {
            let saw = 2.0 * ph - 1.0 - blep(ph, inc);
            let square = (if ph < 0.5 { 1.0 } else { -1.0 }) + blep(ph, inc) - blep((ph + 0.5).fract(), inc);
            saw + (square - saw) * s.wave
        };
        let inc = [self.freq * dt, self.freq * k.detune * dt];
        let x = 0.5 * (osc(self.ph[0], inc[0]) + osc(self.ph[1], inc[1]));
        self.ph = [(self.ph[0] + inc[0]).fract(), (self.ph[1] + inc[1]).fract()];
        // State-variable low-pass (topology-preserving), cutoff swept by the envelope.
        let fc = (k.cutoff * 2f32.powf(k.env_octaves * self.fenv)).min(0.45 * sr);
        let g = (std::f32::consts::PI * fc * dt).tan();
        let v1 = (self.ic[0] + g * (x - self.ic[1])) / (1.0 + g * (g + k.damping));
        let v2 = self.ic[1] + g * v1;
        self.ic = [2.0 * v1 - self.ic[0], 2.0 * v2 - self.ic[1]];
        self.fenv *= k.decay;
        self.t += dt;
        v2 * self.level * self.vel * 0.3
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
    fn render(&mut self, inst: Instrument, dt: f32, k: &ChanCoefs, rng: &mut Rng) -> f32 {
        // Drums: `release` stretches how long a hit rings, `ratio` tunes it.
        let (t, ring, ratio) = (self.hit_t, k.ring, k.ratio);
        let out = match inst {
            Instrument::Kick if t < 0.6 * ring => {
                let f = (45.0 + 110.0 * (-t * 35.0).exp()) * ratio;
                self.ph = (self.ph + f * dt).fract();
                (TAU * self.ph).sin() * (-t * 7.0 / ring).exp() * 0.72 * self.hit_vel
            }
            Instrument::Snare if t < 0.4 * ring => (rng.next() * 0.4 * (-t * 22.0 / ring).exp() + (TAU * 185.0 * ratio * t).sin() * 0.35 * (-t * 35.0 / ring).exp()) * 0.8 * self.hit_vel,
            Instrument::Hat => {
                let n = rng.next();
                self.lp += (0.35 / ratio).min(0.95) * (n - self.lp);
                if t < 0.15 * ring { (n - self.lp) * 0.18 * (-t * 70.0 / ring).exp() * self.hit_vel } else { 0.0 }
            }
            Instrument::Bass => {
                if !self.gate {
                    self.amp *= k.release;
                }
                if self.amp < 1e-4 {
                    return 0.0;
                }
                let env = (self.t / k.attack.max(0.005)).min(1.0) * (0.55 + 0.45 * (-self.t * 2.0).exp()) * self.amp;
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

/// A second-order filter (RBJ cookbook), run in transposed direct form II.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Biquad {
    b: [f32; 3],
    a: [f32; 2],
}

impl Biquad {
    fn from(b: [f32; 3], a: [f32; 3]) -> Self {
        Biquad { b: [b[0] / a[0], b[1] / a[0], b[2] / a[0]], a: [a[1] / a[0], a[2] / a[0]] }
    }

    /// `kind`: 0 low shelf, 1 bell (Q 0.9), 2 high shelf; `db` of boost or cut at `f` Hz.
    fn eq(kind: u8, f: f32, db: f32, sr: f32) -> Self {
        // Kept below the Nyquist frequency, so low sample rates stay stable.
        let w = TAU * f.min(0.45 * sr) / sr;
        let (cw, sw) = (w.cos(), w.sin());
        let a = 10f32.powf(db / 40.0);
        if kind == 1 {
            let al = sw / (2.0 * 0.9);
            return Biquad::from([1.0 + al * a, -2.0 * cw, 1.0 - al * a], [1.0 + al / a, -2.0 * cw, 1.0 - al / a]);
        }
        // Shelves with slope 1.
        let al = sw / 2.0 * SQRT_2;
        let s = 2.0 * a.sqrt() * al;
        if kind == 0 {
            Biquad::from([a * ((a + 1.0) - (a - 1.0) * cw + s), 2.0 * a * ((a - 1.0) - (a + 1.0) * cw), a * ((a + 1.0) - (a - 1.0) * cw - s)], [(a + 1.0) + (a - 1.0) * cw + s, -2.0 * ((a - 1.0) + (a + 1.0) * cw), (a + 1.0) + (a - 1.0) * cw - s])
        } else {
            Biquad::from([a * ((a + 1.0) + (a - 1.0) * cw + s), -2.0 * a * ((a - 1.0) + (a + 1.0) * cw), a * ((a + 1.0) + (a - 1.0) * cw - s)], [(a + 1.0) - (a - 1.0) * cw + s, 2.0 * ((a - 1.0) - (a + 1.0) * cw), (a + 1.0) - (a - 1.0) * cw - s])
        }
    }

    /// Low-pass (`high_pass` false) or high-pass at `f` Hz, Q 0.707.
    fn pass(high_pass: bool, f: f32, sr: f32) -> Self {
        let w = TAU * f.min(0.45 * sr) / sr;
        let (cw, al) = (w.cos(), w.sin() / SQRT_2);
        let a = [1.0 + al, -2.0 * cw, 1.0 - al];
        if high_pass { Biquad::from([(1.0 + cw) / 2.0, -(1.0 + cw), (1.0 + cw) / 2.0], a) } else { Biquad::from([(1.0 - cw) / 2.0, 1.0 - cw, (1.0 - cw) / 2.0], a) }
    }

    fn run(&self, z: &mut [f32; 2], x: f32) -> f32 {
        let y = self.b[0] * x + z[0];
        z[0] = self.b[1] * x - self.a[0] * y + z[1];
        z[1] = self.b[2] * x - self.a[1] * y;
        y
    }
}

/// A mixer track's EQ bands and filter, worked out when a snapshot arrives; `None` is a stage
/// left out (flat), so untouched tracks cost nothing and sound exactly as before.
#[derive(Clone, Copy)]
struct TrackCoefs {
    stages: [Option<Biquad>; 4],
}

impl TrackCoefs {
    const FLAT: TrackCoefs = TrackCoefs { stages: [None; 4] };

    fn new(fx: &TrackFx, sr: f32) -> Self {
        let band = |kind: u8, f: f32, db: f32| (db.abs() > 0.05).then(|| Biquad::eq(kind, f, db, sr));
        let filter = match fx.filter {
            x if x < -0.01 => Some(Biquad::pass(false, 20000.0 * (150.0f32 / 20000.0).powf(-x), sr)),
            x if x > 0.01 => Some(Biquad::pass(true, 20.0 * 250f32.powf(x), sr)),
            _ => None,
        };
        TrackCoefs { stages: [band(0, 200.0, fx.low), band(1, 1000.0, fx.mid), band(2, 5000.0, fx.high), filter] }
    }
}

/// The shared delay: echoes that bounce between left and right, a little darker each time.
struct Delay {
    buf: [Vec<f32>; 2],
    i: usize,
    /// Low-pass state of the feedback, per side.
    lp: [f32; 2],
}

impl Delay {
    /// Longest delay, in seconds.
    const MAX: f32 = 4.0;

    fn new(sr: f32) -> Self {
        let n = (Delay::MAX * sr) as usize + 1;
        Delay { buf: [vec![0.0; n], vec![0.0; n]], i: 0, lp: [0.0; 2] }
    }

    fn process(&mut self, x: [f32; 2], samples: usize, feedback: f32) -> [f32; 2] {
        let n = self.buf[0].len();
        let r = (self.i + n - samples.clamp(1, n - 1)) % n;
        let out = [self.buf[0][r], self.buf[1][r]];
        for side in 0..2 {
            // Each side feeds the other: ping-pong.
            self.lp[side] += 0.5 * (out[1 - side] - self.lp[side]);
            self.buf[side][self.i] = x[side] + self.lp[side] * feedback;
        }
        self.i = (self.i + 1) % n;
        out
    }
}

/// The shared reverb: eight damped combs into four allpasses per side (Freeverb's layout).
struct Reverb {
    combs: [[Comb; 8]; 2],
    aps: [[Allpass; 4]; 2],
}

impl Reverb {
    fn new(sr: f32) -> Self {
        let s = |n: usize, spread: usize| (((n + spread) as f32 * sr / 44100.0) as usize).max(1);
        let comb = |n, sp| Comb { buf: vec![0.0; s(n, sp)], i: 0, lp: 0.0 };
        let ap = |n, sp| Allpass { buf: vec![0.0; s(n, sp)], i: 0 };
        let side = |sp| [1116, 1188, 1277, 1356, 1422, 1491, 1557, 1617].map(|n| comb(n, sp));
        let aps = |sp| [556, 441, 341, 225].map(|n| ap(n, sp));
        Reverb { combs: [side(0), side(23)], aps: [aps(0), aps(23)] }
    }

    fn process(&mut self, x: [f32; 2], size: f32, damping: f32) -> [f32; 2] {
        let (fb, damp) = (0.7 + 0.25 * size, 0.05 + 0.6 * damping);
        let input = (x[0] + x[1]) * 0.015;
        let mut out = [0.0; 2];
        for ((o, combs), aps) in out.iter_mut().zip(&mut self.combs).zip(&mut self.aps) {
            let mut wet = 0.0;
            for c in combs.iter_mut() {
                let y = c.buf[c.i];
                c.lp = y * (1.0 - damp) + c.lp * damp;
                c.buf[c.i] = input + c.lp * fb;
                c.i = (c.i + 1) % c.buf.len();
                wet += y;
            }
            for ap in aps.iter_mut() {
                let b = ap.buf[ap.i];
                ap.buf[ap.i] = wet + b * 0.5;
                wet = b - wet;
                ap.i = (ap.i + 1) % ap.buf.len();
            }
            *o = wet * 1.5;
        }
        out
    }
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
    synths: [SynthVoice; SYNTH_VOICES],
    /// Each channel's sound settings as per-sample numbers, and its tone filter (two stages,
    /// left and right).
    coefs: [ChanCoefs; MAX_CHANNELS],
    tone_lp: [[f32; 4]; MAX_CHANNELS],
    /// Per-frame release multiplier for a chop that is cut by the next one (5 ms).
    chop_cut: f32,
    fx: [FxChain; 2],
    /// Mixer tracks' EQ and filter, and their state (four stages, left and right).
    track_coefs: [TrackCoefs; MAX_MIXER],
    track_z: [[[[f32; 2]; 4]; 2]; MAX_MIXER],
    delay: Delay,
    reverb: Reverb,
    /// Seconds since anything was sent to the delay and the reverb: they stop working once
    /// their tails have died away.
    delay_idle: f32,
    reverb_idle: f32,
    /// 0..1 gain on the vinyl noise: ramps to 1 while playing, to 0 when stopped.
    vinyl_amt: f32,
    peaks: [f32; METERS],
    metronome: bool,
    click: Click,
    /// Where Play starts: see `start_tick`.
    cue: Option<Tick>,
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
            synths: [SynthVoice::default(); SYNTH_VOICES],
            coefs: [ChanCoefs::new(&Sound::default(), Instrument::Rhodes, sr); MAX_CHANNELS],
            tone_lp: [[0.0; 4]; MAX_CHANNELS],
            chop_cut: (-1.0 / (0.005 * sr)).exp(),
            fx: [FxChain::new(sr, 0, 0x9e37_79b9), FxChain::new(sr, 23, 0x7f4a_7c15)],
            track_coefs: [TrackCoefs::FLAT; MAX_MIXER],
            track_z: [[[[0.0; 2]; 4]; 2]; MAX_MIXER],
            delay: Delay::new(sr),
            reverb: Reverb::new(sr),
            delay_idle: f32::MAX,
            reverb_idle: f32::MAX,
            vinyl_amt: 0.0,
            peaks: [0.0; METERS],
            metronome: false,
            click: Click::default(),
            cue: None,
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
                for v in self.synths.iter_mut().filter(|v| v.channel as usize >= n) {
                    v.gate = false;
                }
                for (k, c) in self.coefs.iter_mut().zip(&self.snap.channels) {
                    *k = ChanCoefs::new(&c.sound, c.instrument, self.sr);
                }
                for (k, m) in self.track_coefs.iter_mut().zip(&self.snap.mixer) {
                    *k = TrackCoefs::new(&m.fx, self.sr);
                }
                if self.playing {
                    // Before the loop is fine (playing in from the cue); past its end wraps.
                    if self.next_tick >= self.snap.loop_end {
                        self.next_tick = self.snap.loop_start;
                        self.wrapped = true;
                    }
                    self.seek(self.next_tick);
                }
            }
            Cmd::Play(on) => {
                self.playing = on;
                self.next_tick = self.start_tick();
                self.to_next = 0.0;
                self.wrapped = false;
                self.seek(self.next_tick);
                self.release_seq();
            }
            Cmd::NoteOn { channel, pitch } => self.note_on(channel, pitch, 100, Tick::MAX, true),
            Cmd::Metronome(on) => self.metronome = on,
            Cmd::Seek(t) => {
                self.cue = t;
                if self.playing {
                    self.next_tick = self.start_tick();
                    self.to_next = 0.0;
                    // Release what was playing before the jump, then go on from there.
                    self.wrapped = true;
                    self.seek(self.next_tick);
                }
            }
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
                for v in self.synths.iter_mut().filter(|v| v.live && v.channel == channel && v.note == pitch) {
                    v.gate = false;
                }
            }
        }
    }

    /// Where playback starts: the cue, unless there is none or it is past the loop's end (then
    /// the loop start). A cue before the loop plays into it.
    fn start_tick(&self) -> Tick {
        self.cue.filter(|&t| t < self.snap.loop_end).unwrap_or(self.snap.loop_start)
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
        for v in self.synths.iter_mut().filter(|v| !v.live) {
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
        let k = self.coefs.get(channel as usize).copied().unwrap_or(self.coefs[0]);
        let step = data.sr as f64 / self.sr as f64 * slice.speed as f64 * k.ratio as f64;
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
        self.chop_voices[i] = ChopVoice { active: true, gate: true, live, channel, chop: chop as u16, note: pitch, end, pos, step, stop, gain: slice.gain * g, env: 1.0, t: 0.0, release: 0.0, attack: k.attack };
    }

    fn note_on(&mut self, channel: u16, pitch: u8, vel: u8, end: Tick, live: bool) {
        let ch = channel as usize;
        let (Some(mix), Some(st)) = (self.snap.channels.get(ch), self.chans.get_mut(ch)) else { return };
        let g = vel as f32 / 100.0;
        let freq = midi_hz(pitch) * self.coefs[ch].ratio;
        match mix.instrument {
            Instrument::Kick | Instrument::Snare | Instrument::Hat => {
                st.hit_t = 0.0;
                st.hit_vel = g;
            }
            Instrument::Bass => *st = ChanState { note: pitch, freq, t: 0.0, gate: true, live, amp: 1.0, vel: g, end, ..*st },
            Instrument::Synth => {
                // Free voice first, otherwise steal the quietest.
                let i = self.synths.iter().position(|v| !v.active).unwrap_or_else(|| {
                    let mut best = 0;
                    for (i, v) in self.synths.iter().enumerate() {
                        if v.level < self.synths[best].level {
                            best = i;
                        }
                    }
                    best
                });
                self.synths[i] = SynthVoice { active: true, gate: true, live, channel, note: pitch, end, freq, vel: g, fenv: 1.0, ..SynthVoice::default() };
            }
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
                self.voices[i] = Voice { active: true, gate: true, live, channel, note: pitch, end, freq, vel: 0.3 * g, rel: 1.0, ..Voice::default() };
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
        for v in self.synths.iter_mut().filter(|v| v.gate && !v.live && v.end <= t) {
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

        let dt = self.dt;
        let n = self.snap.channels.len().min(MAX_CHANNELS);
        let mut ch = [[0.0f32; 2]; MAX_CHANNELS];
        for (i, out) in ch.iter_mut().enumerate().take(n) {
            let x = self.chans[i].render(self.snap.channels[i].instrument, dt, &self.coefs[i], &mut self.rng);
            *out = [x, x];
        }
        for v in self.voices.iter_mut().filter(|v| v.active) {
            let Some(k) = self.coefs.get(v.channel as usize) else { continue };
            let out = v.tick(dt, k);
            if let Some(c) = ch.get_mut(v.channel as usize) {
                c[0] += out;
                c[1] += out;
            }
        }
        for v in self.synths.iter_mut().filter(|v| v.active) {
            let (Some(k), Some(mix)) = (self.coefs.get(v.channel as usize), self.snap.channels.get(v.channel as usize)) else {
                v.active = false;
                continue;
            };
            let out = v.tick(&mix.sound, k, dt, self.sr);
            ch[v.channel as usize][0] += out;
            ch[v.channel as usize][1] += out;
        }
        for v in self.chop_voices.iter_mut().filter(|v| v.active) {
            let Some(data) = self.snap.chops.get(v.chop as usize).and_then(|c| c.sample.as_deref()) else {
                v.active = false;
                continue;
            };
            let one_shot = self.snap.chops[v.chop as usize].one_shot;
            if !v.gate && !one_shot && v.release == 0.0 {
                v.release = self.coefs.get(v.channel as usize).map_or(0.99, |k| k.release);
            }
            let [l, r] = v.tick(data, dt);
            if let Some(c) = ch.get_mut(v.channel as usize) {
                c[0] += l;
                c[1] += r;
            }
        }
        // Each channel's tone: two one-pole low-passes, skipped when open.
        for ((out, k), lp) in ch.iter_mut().zip(&self.coefs).zip(self.tone_lp.iter_mut()).take(n) {
            if k.tone < 1.0 {
                for (side, x) in out.iter_mut().enumerate() {
                    lp[side] += k.tone * (*x - lp[side]);
                    lp[side + 2] += k.tone * (lp[side] - lp[side + 2]);
                    *x = lp[side + 2];
                }
            }
        }

        let mut bus = [[0.0f32; 2]; MAX_MIXER];
        for (c, &[l, r]) in self.snap.channels.iter().zip(&ch).filter(|(c, _)| !c.mute) {
            let b = &mut bus[c.mixer.min(MAX_MIXER - 1)];
            b[0] += l * c.gain[0];
            b[1] += r * c.gain[1];
        }
        let mut mix = [0.0; 2];
        let (mut to_delay, mut to_reverb) = ([0.0f32; 2], [0.0f32; 2]);
        for (m, (mx, [l, r])) in self.snap.mixer.iter().zip(bus).enumerate().filter(|(_, (mx, _))| !mx.mute) {
            let mut out = [l * mx.gain[0], r * mx.gain[1]];
            // EQ and filter; stages left flat are skipped.
            for (stage, bq) in self.track_coefs[m].stages.iter().enumerate() {
                if let Some(bq) = bq {
                    for (side, x) in out.iter_mut().enumerate() {
                        *x = bq.run(&mut self.track_z[m][side][stage], *x);
                    }
                }
            }
            self.peaks[m] = self.peaks[m].max(out[0].abs()).max(out[1].abs());
            for side in 0..2 {
                mix[side] += out[side];
                to_delay[side] += out[side] * mx.fx.delay;
                to_reverb[side] += out[side] * mx.fx.reverb;
            }
        }
        // The shared delay and reverb, while anything goes in or their tails still ring.
        let sends = self.snap.sends;
        self.delay_idle = if to_delay != [0.0; 2] { 0.0 } else { self.delay_idle + self.dt };
        if self.delay_idle < Delay::MAX * 12.0 {
            let samples = (sends.delay_steps.round() * 15.0 / self.snap.bpm * self.sr) as usize;
            let echo = self.delay.process(to_delay, samples, sends.feedback);
            mix = [mix[0] + echo[0], mix[1] + echo[1]];
        }
        self.reverb_idle = if to_reverb != [0.0; 2] { 0.0 } else { self.reverb_idle + self.dt };
        if self.reverb_idle < 10.0 {
            let wet = self.reverb.process(to_reverb, sends.size, sends.damping);
            mix = [mix[0] + wet[0], mix[1] + wet[1]];
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
            channels: instruments.iter().map(|&instrument| ChannelMix { instrument, sound: Sound::default(), gain: UNITY, mute: false, mixer: 0, chop: None }).collect(),
            mixer: vec![MixerMix { gain: UNITY, mute: false, fx: TrackFx::default() }],
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
        s.mixer[0] = MixerMix { gain: [0.0, 0.0], mute: false, fx: TrackFx::default() };
        assert!(peak(&mut engine(&s, 22050.0), 2205)[0] < 1e-3);
    }

    #[test]
    fn meters_follow_mixer_routing() {
        let mut s = snap(&[Instrument::Kick, Instrument::Rhodes], &[note(1, 72, 0, 2 * STEP)]);
        s.mixer.push(MixerMix { gain: UNITY, mute: false, fx: TrackFx::default() });
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

    /// Renders `frames` of one channel playing `pitch` from tick 0 for a beat, with `sound`.
    fn one_note(inst: Instrument, sound: Sound, pitch: u8, frames: usize) -> Vec<f32> {
        let mut s = snap(&[inst], &[note(0, pitch, 0, PPQ)]);
        s.channels[0].sound = sound;
        s.loop_end = 64 * PPQ;
        let mut e = engine(&s, 16000.0);
        (0..frames).map(|_| e.next_frame()[0]).collect()
    }

    /// Upward zero crossings per second: the pitch of a simple tone.
    fn crossings(x: &[f32], sr: f32) -> f32 {
        x.windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count() as f32 / (x.len() as f32 / sr)
    }

    /// How much of the signal is high frequencies: energy of the sample-to-sample difference
    /// against the energy of the signal.
    fn brightness(x: &[f32]) -> f32 {
        let e: f32 = x.iter().map(|v| v * v).sum();
        x.windows(2).map(|w| (w[1] - w[0]).powi(2)).sum::<f32>() / e.max(1e-12)
    }

    #[test]
    fn synth_plays_in_tune_and_its_filter_and_envelope_work() {
        let sr = 16000.0;
        let open = Sound { cutoff: 1.0, envelope: 0.0, resonance: 0.0, ..Sound::default() };
        // A4 = 440 Hz: a saw crosses zero upwards once a cycle.
        let a4 = one_note(Instrument::Synth, Sound { detune: 0.0, ..open }, 69, 8000);
        assert!((crossings(&a4[800..6000], sr) - 440.0).abs() < 8.0, "{}", crossings(&a4[800..6000], sr));
        // A closed filter is darker; the square is louder in its upper partials than nothing.
        let dark = one_note(Instrument::Synth, Sound { cutoff: 0.2, ..open }, 57, 6000);
        let bright = one_note(Instrument::Synth, open, 57, 6000);
        assert!(brightness(&dark[1000..5000]) < 0.3 * brightness(&bright[1000..5000]));
        // The envelope opens the filter at the start of the note, then it closes again.
        let swept = one_note(Instrument::Synth, Sound { cutoff: 0.2, envelope: 1.0, decay: 0.1, ..open }, 57, 8000);
        assert!(brightness(&swept[100..600]) > 2.0 * brightness(&swept[5000..7000]));
        // Sustain holds the level while the note lasts (a beat at 120 BPM = 0.5 s), then it
        // releases: a short release dies away much faster than a long one.
        let level = |x: &[f32]| x.iter().fold(0f32, |m, v| m.max(v.abs()));
        let held = one_note(Instrument::Synth, Sound { sustain: 0.5, decay: 0.05, release: 0.2, ..open }, 57, 16000);
        let ringing = one_note(Instrument::Synth, Sound { sustain: 0.5, decay: 0.05, release: 4.0, ..open }, 57, 16000);
        assert!(level(&held[6000..7000]) > 0.05 && level(&held[6000..7000]) > 0.8 * level(&held[4000..5000]));
        assert!(level(&held[14000..15000]) < 1e-3 && level(&ringing[14000..15000]) > 0.5 * level(&ringing[6000..7000]));
        // Slow attack: quiet at first.
        let slow = one_note(Instrument::Synth, Sound { attack: 0.2, ..open }, 57, 6000);
        assert!(level(&slow[0..320]) < 0.3 * level(&slow[3200..4000]));
    }

    #[test]
    fn sound_settings_change_every_instrument() {
        let sr = 16000.0;
        let level = |x: &[f32]| x.iter().fold(0f32, |m, v| m.max(v.abs()));
        // Pitch: an octave up doubles the frequency (the bass is a near-sine).
        let bass = one_note(Instrument::Bass, Sound::default(), 45, 6000);
        let up = one_note(Instrument::Bass, Sound { pitch: 12.0, ..Sound::default() }, 45, 6000);
        let ratio = crossings(&up[500..5500], sr) / crossings(&bass[500..5500], sr);
        assert!((ratio - 2.0).abs() < 0.05, "{ratio}");
        // Tone: the hat through a closed tone filter is much darker.
        let hat = one_note(Instrument::Hat, Sound::default(), 60, 1600);
        let muffled = one_note(Instrument::Hat, Sound { tone: 0.2, ..Sound::default() }, 60, 1600);
        assert!(brightness(&muffled) < 0.2 * brightness(&hat));
        // Release: a kick set to ring longer is still sounding when the normal one is gone.
        let kick = one_note(Instrument::Kick, Sound::default(), 60, 16000);
        let long = one_note(Instrument::Kick, Sound { release: 4.0, ..Sound::default() }, 60, 16000);
        // (The master chain delays everything by a few milliseconds.)
        assert!(level(&kick[9800..11200]) < 1e-3 && level(&long[9800..11200]) > 0.01, "{} {}", level(&kick[9800..11200]), level(&long[9800..11200]));
        // Attack: the electric piano fades in.
        let keys = one_note(Instrument::Rhodes, Sound::default(), 60, 4000);
        let soft = one_note(Instrument::Rhodes, Sound { attack: 0.1, ..Sound::default() }, 60, 4000);
        assert!(level(&soft[0..160]) < 0.3 * level(&keys[0..160]));
        // Defaults are in range; out-of-range settings are caught.
        assert!(Sound::default().is_valid() && !Sound { pitch: 30.0, ..Sound::default() }.is_valid() && !Sound { tone: f32::NAN, ..Sound::default() }.is_valid());
    }

    /// Gain in dB of `bq` for a sine at `f` Hz.
    fn gain_db(bq: &Biquad, f: f32, sr: f32) -> f32 {
        let mut z = [0.0; 2];
        let n = sr as usize;
        let (mut e_in, mut e_out) = (0.0, 0.0);
        for i in 0..n {
            let x = (TAU * f * i as f32 / sr).sin();
            let y = bq.run(&mut z, x);
            if i > n / 2 {
                e_in += x * x;
                e_out += y * y;
            }
        }
        10.0 * (e_out / e_in).log10()
    }

    #[test]
    fn eq_and_filter_shapes() {
        let sr = 44100.0;
        let near = |got: f32, want: f32| (got - want).abs() < 1.0;
        let low = Biquad::eq(0, 200.0, 12.0, sr);
        assert!(near(gain_db(&low, 40.0, sr), 12.0) && near(gain_db(&low, 5000.0, sr), 0.0));
        let bell = Biquad::eq(1, 1000.0, -6.0, sr);
        assert!(near(gain_db(&bell, 1000.0, sr), -6.0) && near(gain_db(&bell, 60.0, sr), 0.0) && near(gain_db(&bell, 15000.0, sr), 0.0));
        let high = Biquad::eq(2, 5000.0, -12.0, sr);
        assert!(near(gain_db(&high, 16000.0, sr), -12.0) && near(gain_db(&high, 100.0, sr), 0.0));
        let lp = Biquad::pass(false, 1000.0, sr);
        assert!(near(gain_db(&lp, 100.0, sr), 0.0) && near(gain_db(&lp, 1000.0, sr), -3.0) && gain_db(&lp, 8000.0, sr) < -30.0);
        let hp = Biquad::pass(true, 1000.0, sr);
        assert!(near(gain_db(&hp, 10000.0, sr), 0.0) && gain_db(&hp, 125.0, sr) < -30.0);
        // At a low sample rate, bands above half of it stay stable.
        let z = &mut [0.0; 2];
        let shelf = Biquad::eq(2, 5000.0, 12.0, 8000.0);
        assert!((0..8000).map(|i| shelf.run(z, (i as f32 * 0.7).sin())).all(|y| y.is_finite() && y.abs() < 10.0));
        // Flat settings add no stage at all; the filter knob sweeps its cutoff.
        assert!(TrackCoefs::new(&TrackFx::default(), sr).stages.iter().all(Option::is_none));
        let dark = TrackCoefs::new(&TrackFx { filter: -0.5, ..TrackFx::default() }, sr).stages[3].unwrap();
        assert!(gain_db(&dark, 6000.0, sr) < -6.0 && near(gain_db(&dark, 200.0, sr), 0.0));
        assert!(TrackFx::default().is_valid() && !TrackFx { low: 20.0, ..TrackFx::default() }.is_valid());
        assert!(Sends::default().is_valid() && !Sends { feedback: 1.0, ..Sends::default() }.is_valid());
    }

    #[test]
    fn mixer_filter_delay_and_reverb() {
        let sr = 16000.0;
        // A hat and a kick at the start, panned hard left, through mixer track 0.
        let run = |fx: TrackFx, sends: Sends, frames: usize| {
            let mut s = snap(&[Instrument::Hat, Instrument::Kick], &[note(0, 60, 0, STEP), note(1, 60, 0, STEP)]);
            s.loop_end = 64 * PPQ;
            s.channels[0].gain = [1.0, 0.0];
            s.channels[1].gain = [1.0, 0.0];
            (s.mixer[0].fx, s.sends) = (fx, sends);
            let mut e = engine(&s, sr);
            (0..frames).map(|_| e.next_frame()).collect::<Vec<[f32; 2]>>()
        };
        let left = |x: &[[f32; 2]]| x.iter().map(|f| f[0]).collect::<Vec<f32>>();
        let level = |x: &[[f32; 2]], side: usize| x.iter().fold(0f32, |m, f| m.max(f[side].abs()));
        // The filter knob turned left darkens the track.
        let plain = run(TrackFx::default(), Sends::default(), 2400);
        let filtered = run(TrackFx { filter: -0.7, ..TrackFx::default() }, Sends::default(), 2400);
        assert!(brightness(&left(&filtered)) < 0.3 * brightness(&left(&plain)));
        // Delay of four 16ths at 120 BPM: 0.5 s. With no send there is nothing after the hit;
        // with it, the first echo comes back on the left and the next one bounces to the right.
        let dry = run(TrackFx::default(), Sends::default(), 20000);
        let echo = run(TrackFx { delay: 1.0, ..TrackFx::default() }, Sends { delay_steps: 4.0, feedback: 0.5, ..Sends::default() }, 20000);
        let window = |x: &[[f32; 2]], t: f32| x[(t * sr) as usize..((t + 0.1) * sr) as usize].to_vec();
        assert!(level(&window(&dry, 0.62), 0) < 1e-3);
        assert!(level(&window(&echo, 0.5), 0) > 0.2 && level(&window(&echo, 0.5), 1) < 0.02);
        assert!(level(&window(&echo, 1.0), 1) > 0.05);
        // Reverb: a tail after the hit has ended, longer for a bigger room.
        let room = |size: f32| run(TrackFx { reverb: 1.0, ..TrackFx::default() }, Sends { size, ..Sends::default() }, 32000);
        let (small, big) = (room(0.1), room(1.0));
        assert!(level(&window(&small, 0.7), 0) > 1e-3);
        assert!(level(&window(&big, 1.0), 0) > 10.0 * level(&window(&small, 1.0), 0));
    }

    #[test]
    fn play_starts_from_the_cue_and_seek_jumps() {
        // One note per beat, pitch 60 + beat, over two bars; beats last 0.125 s at 480 BPM.
        let notes: Vec<SeqNote> = (0..8).map(|b| note(0, 60 + b as u8, b * PPQ, PPQ / 2)).collect();
        let mut s = snap(&[Instrument::Rhodes], &notes);
        (s.loop_end, s.bpm) = (8 * PPQ, 480.0);
        let sr = 8000.0;
        // The notes the engine has started, in order.
        let heard = |e: &mut Engine, frames: usize| {
            let mut got = Vec::new();
            for _ in 0..frames {
                e.next_frame();
                for v in e.voices.iter().filter(|v| v.active && v.gate) {
                    if got.last() != Some(&v.note) {
                        got.push(v.note);
                    }
                }
            }
            got
        };
        let mut e = Engine::new(sr);
        e.handle(Cmd::Snapshot(Box::new(s.clone())));
        // Stopped: Play starts at the cue (beat 6), plays to the end, wraps to the loop start.
        e.handle(Cmd::Seek(Some(6 * PPQ)));
        e.handle(Cmd::Play(true));
        assert_eq!(heard(&mut e, 3 * 1000), [66, 67, 60]);
        // Stop and play again: back at the cue.
        e.handle(Cmd::Play(false));
        e.handle(Cmd::Play(true));
        assert_eq!(heard(&mut e, 1000), [66]);
        // Playing: a seek jumps at once and releases what was held.
        e.handle(Cmd::Seek(Some(2 * PPQ)));
        assert!(e.voices.iter().all(|v| !v.gate || v.note == 66));
        assert_eq!(heard(&mut e, 2 * 1000), [62, 63]);
        assert!(e.voices.iter().filter(|v| v.gate).all(|v| v.note == 63));
        // A cue past the end, or none, starts at the loop start; one before the loop plays in.
        e.handle(Cmd::Seek(Some(99 * PPQ)));
        assert_eq!(heard(&mut e, 1000), [60]);
        let mut looped = s.clone();
        looped.loop_start = 4 * PPQ;
        e.handle(Cmd::Snapshot(Box::new(looped)));
        e.handle(Cmd::Seek(None));
        assert_eq!(heard(&mut e, 1000), [64]);
        e.handle(Cmd::Seek(Some(2 * PPQ)));
        assert_eq!(heard(&mut e, 7 * 1000), [62, 63, 64, 65, 66, 67, 64]);
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
        // And a synth (index 7) with a darker tone, playing chords every beat.
        base.channels.push(ChannelMix { instrument: Instrument::Synth, sound: Sound { tone: 0.5, ..Sound::default() }, ..base.channels[0] });
        for beat in 0..16 {
            for p in [57, 60, 64] {
                base.notes.push(note(7, p, beat * PPQ, PPQ / 2));
            }
        }
        base.notes.sort_by_key(|n| n.start);
        // Every mixer effect on, so their paths run too.
        for m in &mut base.mixer {
            m.fx = TrackFx { low: 3.0, mid: -2.0, high: 4.0, filter: -0.3, delay: 0.4, reverb: 0.5 };
        }
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
