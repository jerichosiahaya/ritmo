//! Audio engine: synthesized drums, FM Rhodes, sine bass, melody sequencer, mixer and lo-fi master chain.
//! Pure Rust and no I/O, so the same engine plays live and renders WAV exports.

use std::f32::consts::{FRAC_PI_4, SQRT_2, TAU};
use std::sync::mpsc;

/// Vinyl hiss and crackle fade in on play and out on stop over this many seconds,
/// so a stopped engine is silent.
const VINYL_FADE: f32 = 0.25;

pub const STEPS: usize = 16;
pub const BARS: usize = 4;
pub const TOTAL_STEPS: usize = STEPS * BARS;
pub const TRACKS: usize = 3;

pub const CHANNELS: usize = 6;
pub const CHANNEL_NAMES: [&str; CHANNELS] = ["Kick", "Snare", "Hat", "Keys", "Bass", "Lead"];
pub const BUSES: usize = 4;
pub const BUS_NAMES: [&str; BUSES] = ["Drums", "Keys", "Bass", "Lead"];
/// Mixer bus each channel feeds.
pub const CH_BUS: [usize; CHANNELS] = [0, 0, 0, 1, 2, 3];
/// Meters: one per bus, then master.
pub const METERS: usize = BUSES + 1;

/// Piano roll range: LEAD_LOW .. LEAD_LOW + LEAD_KEYS (C4..C6).
pub const LEAD_LOW: u8 = 60;
pub const LEAD_KEYS: usize = 25;

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

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Fx {
    pub tone: f32,
    pub crush: f32,
    pub wobble: f32,
    pub vinyl: f32,
    pub reverb: f32,
    pub volume: f32,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Channel {
    pub vol: f32,
    /// -1 = left, 1 = right.
    pub pan: f32,
    pub mute: bool,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Song {
    pub drums: [[bool; STEPS]; TRACKS],
    pub chords: [usize; BARS],
    pub bpm: f32,
    /// Share of each 8th-note pair given to the first 16th: 0.5 = straight, 0.75 = hard swing.
    pub swing: f32,
    pub fx: Fx,
    pub channels: [Channel; CHANNELS],
    /// Mixer fader per bus.
    pub bus: [f32; BUSES],
    /// Piano roll: bit `s` of row `k` = pitch LEAD_LOW + k is held during global step `s`.
    /// Adjacent set bits make one longer note.
    pub melody: [u64; LEAD_KEYS],
    /// Playlist: which bars play the drum pattern / the melody.
    pub drum_bars: [bool; BARS],
    pub lead_bars: [bool; BARS],
}

impl Song {
    pub fn note(&self, pitch: u8, step: usize) -> bool {
        pitch >= LEAD_LOW && ((pitch - LEAD_LOW) as usize) < LEAD_KEYS && self.melody[(pitch - LEAD_LOW) as usize] >> step & 1 == 1
    }

    pub fn set_note(&mut self, pitch: u8, step: usize, on: bool) {
        let row = &mut self.melody[(pitch - LEAD_LOW) as usize];
        if on { *row |= 1 << step } else { *row &= !(1 << step) }
    }
}

impl Default for Song {
    fn default() -> Self {
        let p = |s: &str| {
            let mut row = [false; STEPS];
            for (i, c) in s.chars().enumerate() {
                row[i] = c == 'x';
            }
            row
        };
        let mut song = Song {
            drums: [p("x......x..x....."), p("....x.......x..."), p("x.x.x.x.x.x.x.x.")],
            chords: [1, 2, 3, 4],
            bpm: 80.0,
            swing: 0.6,
            fx: Fx { tone: 0.55, crush: 0.2, wobble: 0.35, vinyl: 0.4, reverb: 0.3, volume: 0.8 },
            channels: [Channel { vol: 0.8, pan: 0.0, mute: false }; CHANNELS],
            bus: [0.8; BUSES],
            melody: [0; LEAD_KEYS],
            drum_bars: [true; BARS],
            lead_bars: [true; BARS],
        };
        // A short motif in bars 2 and 4: (pitch, start step, length).
        for (pitch, start, len) in [(72, 20, 2), (69, 24, 2), (67, 28, 4), (72, 52, 2), (74, 56, 2), (76, 60, 4)] {
            for s in start..start + len {
                song.set_note(pitch, s, true);
            }
        }
        song
    }
}

// Song goes by value (about 400 bytes) so the audio thread never frees heap memory.
#[allow(clippy::large_enum_variant)]
pub enum Cmd {
    Song(Song),
    Play(bool),
    NoteOn(u8),
    NoteOff(u8),
}

/// Most commands the UI can queue before the audio thread drains them.
pub const CMD_QUEUE: usize = 256;

/// UI → audio command queue. It is bounded, so its buffer is allocated once here and
/// receiving never allocates or frees (an unbounded `mpsc::channel` frees its blocks on the
/// receiving side). Send with `try_send` so the UI never blocks.
pub fn command_channel() -> (mpsc::SyncSender<Cmd>, mpsc::Receiver<Cmd>) {
    mpsc::sync_channel(CMD_QUEUE)
}

pub fn midi_hz(n: u8) -> f32 {
    440.0 * 2f32.powf((n as f32 - 69.0) / 12.0)
}

/// Length in samples of a 16th step; odd steps are shortened by swing so each pair keeps its length.
pub fn step_len(step: usize, bpm: f32, swing: f32, sr: f32) -> f64 {
    let pair = 2.0 * sr as f64 * 60.0 / bpm as f64 / 4.0;
    if step.is_multiple_of(2) { pair * swing as f64 } else { pair * (1.0 - swing as f64) }
}

/// Constant-power pan, unity gain per side at center.
pub fn pan_gains(pan: f32) -> (f32, f32) {
    let a = (pan.clamp(-1.0, 1.0) + 1.0) * FRAC_PI_4;
    (a.cos() * SQRT_2, a.sin() * SQRT_2)
}

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

#[derive(Clone, Copy, PartialEq, Default)]
enum Kind {
    #[default]
    Chord,
    Live,
    Seq,
}

#[derive(Clone, Copy, Default)]
struct Voice {
    active: bool,
    gate: bool,
    kind: Kind,
    note: u8,
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

pub struct Engine {
    sr: f32,
    dt: f32,
    song: Song,
    pub playing: bool,
    /// Global step (0..TOTAL_STEPS) most recently triggered.
    pub step: usize,
    next_step: usize,
    to_next: f64,
    rng: Rng,
    drum_t: [f32; TRACKS],
    kick_ph: f32,
    hat_lp: f32,
    voices: [Voice; 32],
    lead_held: [bool; LEAD_KEYS],
    rel_coef: f32,
    bass_freq: f32,
    bass_t: f32,
    bass_gate: bool,
    bass_amp: f32,
    bass_ph: f32,
    fx: [FxChain; 2],
    /// 0..1 gain on the vinyl noise: ramps to 1 while playing, to 0 when stopped.
    vinyl_amt: f32,
    peaks: [f32; METERS],
}

impl Engine {
    pub fn new(sr: f32) -> Self {
        Engine {
            sr,
            dt: 1.0 / sr,
            song: Song::default(),
            playing: false,
            step: 0,
            next_step: 0,
            to_next: 0.0,
            rng: Rng(0x1234_5678),
            drum_t: [f32::MAX; TRACKS],
            kick_ph: 0.0,
            hat_lp: 0.0,
            voices: [Voice::default(); 32],
            lead_held: [false; LEAD_KEYS],
            rel_coef: (-1.0 / (0.25 * sr)).exp(),
            bass_freq: 0.0,
            bass_t: 0.0,
            bass_gate: false,
            bass_amp: 0.0,
            bass_ph: 0.0,
            fx: [FxChain::new(sr, 0, 0x9e37_79b9), FxChain::new(sr, 23, 0x7f4a_7c15)],
            vinyl_amt: 0.0,
            peaks: [0.0; METERS],
        }
    }

    pub fn handle(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Song(s) => self.song = s,
            Cmd::Play(on) => {
                self.playing = on;
                self.next_step = 0;
                self.to_next = 0.0;
                if !on {
                    self.release(Kind::Chord);
                    self.release(Kind::Seq);
                    self.bass_gate = false;
                    self.lead_held = [false; LEAD_KEYS];
                }
            }
            Cmd::NoteOn(n) => self.note_on(n, 0.3, Kind::Live),
            Cmd::NoteOff(n) => self.note_off(n, Kind::Live),
        }
    }

    /// Peak level per meter since the last call, then reset.
    pub fn take_peaks(&mut self) -> [f32; METERS] {
        std::mem::take(&mut self.peaks)
    }

    fn note_on(&mut self, note: u8, vel: f32, kind: Kind) {
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
        self.voices[i] = Voice { active: true, gate: true, kind, note, freq: midi_hz(note), vel, rel: 1.0, ..Voice::default() };
    }

    fn note_off(&mut self, note: u8, kind: Kind) {
        for v in self.voices.iter_mut().filter(|v| v.kind == kind && v.note == note) {
            v.gate = false;
        }
    }

    fn release(&mut self, kind: Kind) {
        for v in self.voices.iter_mut().filter(|v| v.kind == kind) {
            v.gate = false;
        }
    }

    fn trigger(&mut self, step: usize) {
        let (s, bar) = (step % STEPS, step / STEPS);
        if self.song.drum_bars[bar] {
            for tr in 0..TRACKS {
                if self.song.drums[tr][s] {
                    self.drum_t[tr] = 0.0;
                }
            }
        }
        if s == 0 {
            self.release(Kind::Chord);
            self.bass_gate = false;
            let c = &CHORDS[self.song.chords[bar].min(CHORDS.len() - 1)];
            for &n in c.notes {
                self.note_on(n, 0.16, Kind::Chord);
            }
            if c.bass > 0 {
                self.bass_freq = midi_hz(c.bass);
                self.bass_t = 0.0;
                self.bass_gate = true;
                self.bass_amp = 1.0;
            }
        }
        for k in 0..LEAD_KEYS {
            let pitch = LEAD_LOW + k as u8;
            let now = self.song.lead_bars[bar] && self.song.note(pitch, step);
            if now && !self.lead_held[k] {
                self.note_on(pitch, 0.3, Kind::Seq);
            } else if !now && self.lead_held[k] {
                self.note_off(pitch, Kind::Seq);
            }
            self.lead_held[k] = now;
        }
    }

    fn drums(&mut self) -> [f32; TRACKS] {
        let dt = self.dt;
        let mut out = [0.0; TRACKS];
        let t = self.drum_t[0];
        if t < 0.6 {
            let f = 45.0 + 110.0 * (-t * 35.0).exp();
            self.kick_ph = (self.kick_ph + f * dt).fract();
            out[0] = (TAU * self.kick_ph).sin() * (-t * 7.0).exp() * 0.72;
        }
        let t = self.drum_t[1];
        if t < 0.4 {
            out[1] = (self.rng.next() * 0.4 * (-t * 22.0).exp() + (TAU * 185.0 * t).sin() * 0.35 * (-t * 35.0).exp()) * 0.8;
        }
        let t = self.drum_t[2];
        let n = self.rng.next();
        self.hat_lp += 0.35 * (n - self.hat_lp);
        if t < 0.15 {
            out[2] = (n - self.hat_lp) * 0.18 * (-t * 70.0).exp();
        }
        for t in &mut self.drum_t {
            if *t < 10.0 {
                *t += dt;
            }
        }
        out
    }

    fn bass(&mut self) -> f32 {
        if !self.bass_gate {
            self.bass_amp *= self.rel_coef;
        }
        if self.bass_amp < 1e-4 {
            return 0.0;
        }
        let env = (self.bass_t * 200.0).min(1.0) * (0.55 + 0.45 * (-self.bass_t * 2.0).exp()) * self.bass_amp;
        self.bass_ph = (self.bass_ph + self.bass_freq * self.dt).fract();
        self.bass_t += self.dt;
        ((TAU * self.bass_ph).sin() * 1.6).tanh() * 0.35 * env
    }

    /// One stereo frame.
    pub fn next_frame(&mut self) -> [f32; 2] {
        if self.playing {
            if self.to_next <= 0.0 {
                self.step = self.next_step;
                self.trigger(self.step);
                self.to_next += step_len(self.step, self.song.bpm, self.song.swing, self.sr);
                self.next_step = (self.step + 1) % TOTAL_STEPS;
            }
            self.to_next -= 1.0;
        }
        let mut ch = [0.0; CHANNELS];
        ch[..TRACKS].copy_from_slice(&self.drums());
        let (dt, rc) = (self.dt, self.rel_coef);
        for v in self.voices.iter_mut().filter(|v| v.active) {
            let out = v.tick(dt, rc);
            ch[if v.kind == Kind::Chord { 3 } else { 5 }] += out;
        }
        ch[4] = self.bass();

        let mut bus = [[0.0f32; 2]; BUSES];
        for (i, &x) in ch.iter().enumerate() {
            let c = self.song.channels[i];
            if !c.mute {
                let (l, r) = pan_gains(c.pan);
                bus[CH_BUS[i]][0] += x * c.vol * l;
                bus[CH_BUS[i]][1] += x * c.vol * r;
            }
        }
        let mut mix = [0.0; 2];
        for (b, [l, r]) in bus.into_iter().enumerate() {
            let (l, r) = (l * self.song.bus[b], r * self.song.bus[b]);
            self.peaks[b] = self.peaks[b].max(l.abs()).max(r.abs());
            mix[0] += l;
            mix[1] += r;
        }
        let ramp = self.dt / VINYL_FADE;
        self.vinyl_amt = if self.playing { (self.vinyl_amt + ramp).min(1.0) } else { (self.vinyl_amt - ramp).max(0.0) };
        let fx = Fx { vinyl: self.song.fx.vinyl * self.vinyl_amt, ..self.song.fx };
        let out = [self.fx[0].process(mix[0], &fx, self.sr), self.fx[1].process(mix[1], &fx, self.sr)];
        self.peaks[BUSES] = self.peaks[BUSES].max(out[0].abs()).max(out[1].abs());
        out
    }
}

/// Render `loops` passes of the 4-bar song plus a 2 s tail, stereo.
pub fn render_offline(song: Song, sr: u32, loops: usize) -> Vec<[f32; 2]> {
    let mut e = Engine::new(sr as f32);
    e.handle(Cmd::Song(song));
    e.handle(Cmd::Play(true));
    let body = (loops * BARS) as f64 * 4.0 * 60.0 / song.bpm as f64 * sr as f64;
    let mut out: Vec<[f32; 2]> = (0..body as usize).map(|_| e.next_frame()).collect();
    e.handle(Cmd::Play(false));
    out.extend((0..sr as usize * 2).map(|_| e.next_frame()));
    out
}

pub fn export_wav(path: &std::path::Path, song: Song) -> Result<(), hound::Error> {
    let sr = 44100;
    let spec = hound::WavSpec { channels: 2, sample_rate: sr, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
    let mut w = hound::WavWriter::create(path, spec)?;
    for frame in render_offline(song, sr, 2) {
        for s in frame {
            w.write_sample((s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)?;
        }
    }
    w.finalize()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Counts allocations and frees made by threads that set `WATCH` (other tests are unaffected).
    struct CountingAlloc;
    static HEAP_OPS: AtomicUsize = AtomicUsize::new(0);
    thread_local! { static WATCH: Cell<bool> = const { Cell::new(false) }; }

    unsafe impl GlobalAlloc for CountingAlloc {
        unsafe fn alloc(&self, l: Layout) -> *mut u8 {
            if WATCH.with(Cell::get) {
                HEAP_OPS.fetch_add(1, Ordering::Relaxed);
            }
            unsafe { System.alloc(l) }
        }
        unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
            if WATCH.with(Cell::get) {
                HEAP_OPS.fetch_add(1, Ordering::Relaxed);
            }
            unsafe { System.dealloc(p, l) }
        }
    }

    #[global_allocator]
    static ALLOC: CountingAlloc = CountingAlloc;

    fn silent() -> Song {
        let d = Song::default();
        Song { drums: [[false; STEPS]; TRACKS], chords: [0; BARS], melody: [0; LEAD_KEYS], fx: Fx { vinyl: 0.0, ..d.fx }, ..d }
    }

    fn engine(song: Song, sr: f32) -> Engine {
        let mut e = Engine::new(sr);
        e.handle(Cmd::Song(song));
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

    #[test]
    fn swing_keeps_pair_length() {
        let (a, b) = (step_len(0, 90.0, 0.66, 48000.0), step_len(1, 90.0, 0.66, 48000.0));
        let pair = 2.0 * 48000.0 * 60.0 / 90.0 / 4.0;
        assert!((a + b - pair).abs() < 1e-6);
        assert!(a > b);
        assert_eq!(step_len(0, 90.0, 0.5, 48000.0), step_len(1, 90.0, 0.5, 48000.0));
    }

    #[test]
    fn chord_table_is_sane() {
        for c in &CHORDS[1..] {
            assert!(c.notes.iter().all(|&n| (36..=96).contains(&n)), "{}", c.name);
            assert!((24..60).contains(&c.bass), "{}", c.name);
        }
        assert!((midi_hz(69) - 440.0).abs() < 1e-3);
    }

    #[test]
    fn pan_law() {
        let (l, r) = pan_gains(0.0);
        assert!((l - 1.0).abs() < 1e-6 && (r - 1.0).abs() < 1e-6);
        let (l, r) = pan_gains(-1.0);
        assert!(r.abs() < 1e-6 && l > 1.0);
    }

    #[test]
    fn default_song_renders_audible_and_bounded() {
        let out = render_offline(Song::default(), 22050, 1);
        let expected = (BARS as f64 * 4.0 * 60.0 / 80.0 * 22050.0) as usize + 44100;
        assert_eq!(out.len(), expected);
        let peak = out.iter().flatten().fold(0f32, |m, s| m.max(s.abs()));
        assert!(out.iter().flatten().all(|s| s.is_finite()));
        assert!(peak > 0.1 && peak <= 1.0, "peak {peak}");
    }

    #[test]
    fn empty_song_is_silent() {
        let out = render_offline(silent(), 22050, 1);
        assert!(out.iter().flatten().all(|s| s.abs() < 1e-3));
    }

    #[test]
    fn kick_on_step_zero_sounds_immediately() {
        let mut s = silent();
        s.drums[0][0] = true;
        assert!(peak(&mut engine(s, 22050.0), 2205)[0] > 0.1);
    }

    #[test]
    fn mute_pan_and_drum_bars() {
        let mut s = silent();
        s.drums[0][0] = true;
        s.channels[0].pan = -1.0;
        let p = peak(&mut engine(s, 22050.0), 2205);
        assert!(p[0] > 0.1 && p[1] < 1e-3, "{p:?}");
        s.channels[0].mute = true;
        assert!(peak(&mut engine(s, 22050.0), 2205)[0] < 1e-3);
        s.channels[0].mute = false;
        s.drum_bars[0] = false;
        assert!(peak(&mut engine(s, 22050.0), 2205)[0] < 1e-3);
    }

    #[test]
    fn melody_plays_on_lead_bus_and_respects_lead_bars() {
        let mut s = silent();
        s.set_note(72, 0, true);
        s.set_note(72, 1, true);
        assert!(s.note(72, 1) && !s.note(72, 2) && !s.note(40, 0));
        let mut e = engine(s, 22050.0);
        assert!(peak(&mut e, 2205)[0] > 0.05);
        let meters = e.take_peaks();
        assert!(meters[3] > 0.05 && meters[0] == 0.0 && meters[BUSES] > 0.05, "{meters:?}");
        assert_eq!(e.take_peaks(), [0.0; METERS]);
        s.lead_bars[0] = false;
        assert!(peak(&mut engine(s, 22050.0), 2205)[0] < 1e-3);
    }

    #[test]
    fn live_note_on_off_releases() {
        let mut e = Engine::new(22050.0);
        e.handle(Cmd::Song(silent()));
        e.handle(Cmd::NoteOn(60));
        let on = peak(&mut e, 2205)[0];
        e.handle(Cmd::NoteOff(60));
        peak(&mut e, 22050 * 3);
        assert!(on > 0.05);
        assert!(e.voices.iter().all(|v| !v.active));
    }

    #[test]
    fn export_writes_readable_stereo_wav() {
        let path = std::env::temp_dir().join(format!("ritmo-test-{}.wav", std::process::id()));
        export_wav(&path, Song::default()).unwrap();
        let r = hound::WavReader::open(&path).unwrap();
        let spec = r.spec();
        let frames = r.len() / 2;
        std::fs::remove_file(&path).unwrap();
        assert_eq!((spec.channels, spec.sample_rate), (2, 44100));
        // 2 loops x 4 bars at 80 BPM = 24 s, plus 2 s tail.
        assert_eq!(frames as usize, (24.0 * 44100.0) as usize + 88200);
    }

    /// The audio callback's work (drain commands, render frames) must not touch the heap.
    #[test]
    fn audio_path_never_allocates_or_frees() {
        const SONGS: usize = 300;
        let (tx, rx) = command_channel();
        let mut e = Engine::new(22050.0);
        // Like the app's UI thread: try_send, never a blocking send (a blocked sender
        // registers a wakeup that the receiver may then touch).
        let ui = std::thread::spawn(move || {
            let send = |mut cmd: Cmd| loop {
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
            for i in 0..SONGS {
                send(Cmd::Song(Song { bpm: 80.0 + i as f32 * 0.1, ..Song::default() }));
                send(Cmd::NoteOn(60 + (i % 12) as u8));
                send(Cmd::NoteOff(60 + (i % 12) as u8));
            }
        });
        let total = 1 + SONGS * 3;
        let mut got = 0;
        WATCH.with(|w| w.set(true));
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
        WATCH.with(|w| w.set(false));
        ui.join().unwrap();
        assert_eq!(HEAP_OPS.load(Ordering::Relaxed), 0, "audio path allocated or freed memory (got {got} cmds)");
        assert!(e.playing && (e.song.bpm - (80.0 + (SONGS - 1) as f32 * 0.1)).abs() < 1e-4);
    }

    #[test]
    fn vinyl_crackles_while_playing() {
        let mut s = silent();
        s.fx.vinyl = 1.0;
        assert!(peak(&mut engine(s, 22050.0), 22050 * 2)[0] > 0.01);
    }

    #[test]
    fn stopped_engine_goes_silent() {
        // Default song: vinyl on, chords, reverb. Play one bar, stop, let tails ring out.
        let mut e = engine(Song::default(), 22050.0);
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

    #[test]
    fn sequencer_wraps_after_four_bars() {
        let mut s = silent();
        s.bpm = 160.0;
        let mut e = engine(s, 8000.0);
        let total = (BARS as f64 * 4.0 * 60.0 / 160.0 * 8000.0) as usize;
        let mut max = 0;
        for _ in 0..total + 10 {
            e.next_frame();
            max = max.max(e.step);
        }
        assert_eq!(max, TOTAL_STEPS - 1);
        assert_eq!(e.step, 0);
    }
}
