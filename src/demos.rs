//! The demo projects in `demos/` and the sample they chop, built here so they can be remade with
//! `cargo test -- --ignored write_demos`. `demos_open` checks that the shipped files still open,
//! find their samples and play. Everything in them, the sample included, is made by Ritmo's own
//! instruments.

use crate::engine::{self, Fx, STEP, SampleData, Tick};
use std::sync::Arc;
use crate::file;
use crate::engine::Instrument;
use crate::project::{CHOP_BASE, CHORDS, ChannelId, DEFAULT_VEL, MAX_SLICES, PatternId, Project};
use crate::sample;
use std::fs;
use std::path::{Path, PathBuf};

fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("demos")
}

/// Where the boom-bap demo's sample lives, relative to the demo.
const BREAK: &str = "samples/dusty-break.wav";

/// Puts a hit on every `x` of `row` (16ths) for `channel`, from tick `at`.
fn steps(p: &mut Project, pattern: PatternId, channel: ChannelId, at: Tick, row: &str) {
    for (i, _) in row.char_indices().filter(|&(_, c)| c == 'x') {
        p.add_note(pattern, channel, 60, at + i as Tick * STEP, STEP, DEFAULT_VEL);
    }
}

/// Channels of `Project::blank()`: kick, snare, hat, keys, bass, lead.
fn kit(p: &Project) -> [ChannelId; 6] {
    std::array::from_fn(|i| p.channels[i].id)
}

/// A two-bar drum break under electric-piano chords, through a dusty master chain, rendered to
/// mono: the "record" the boom-bap demo chops.
fn dusty_break() -> SampleData {
    let mut p = Project::blank();
    (p.bpm, p.swing) = (90.0, 0.58);
    p.master = Fx { tone: 0.6, crush: 0.15, wobble: 0.3, vinyl: 0.5, reverb: 0.25, volume: 0.8 };
    let [kick, snare, hat, ..] = kit(&p);
    let bar = p.bar_len();
    let beat = p.patterns[0].id;
    p.patterns[0].len = 2 * bar;
    steps(&mut p, beat, kick, 0, "x......x..x.....x.x.......x..x..");
    steps(&mut p, beat, snare, 0, "....x.......x.......x.......x..x");
    steps(&mut p, beat, hat, 0, "x.x.x.x.x.x.x.x.x.x.x.x.x.x.x.x.");
    p.stamp_chord(beat, 0, bar, 4); // Am9
    p.stamp_chord(beat, bar, bar, 1); // Dm9
    let track = p.tracks[0].id;
    p.add_clip(track, beat, 0, 2 * bar);
    let snap = p.compile();
    let sr = 44100;
    let frames = (8.0 * 60.0 / p.bpm as f64 * sr as f64) as usize;
    let out = engine::render_offline(&snap, sr, 1);
    SampleData { sr, frames: out[..frames].iter().map(|&[l, r]| [(l + r) / 2.0; 2]).collect() }
}

/// 90 BPM: the break chopped into 16 pads and flipped into a new order, with drums and bass.
fn boom_bap(data: &SampleData) -> Project {
    let mut p = Project::blank();
    (p.bpm, p.swing) = (90.0, 0.6);
    p.master = Fx { tone: 0.75, crush: 0.05, wobble: 0.15, vinyl: 0.25, reverb: 0.2, volume: 0.8 };
    let [kick, snare, _, _, bass, _] = kit(&p);
    let bar = p.bar_len();
    let chops = p.add_chop_channel("Dusty Break", PathBuf::from(BREAK), data.frames.len() as u32).expect("room for a channel");
    // One pad per 8th note of the break.
    p.set_chop_points(chops, &sample::even_points(data.frames.len() as u32, MAX_SLICES));
    let pads = p.chop(chops).map_or(1, |c| c.slices.len()) as u8;
    if let Some(c) = p.channels.iter_mut().find_map(|c| c.chop.as_mut()) {
        (c.one_shot, c.cut_itself) = (true, true);
    }
    if let Some(s) = p.slice_mut(chops, 2) {
        s.pitch = -2.0;
    }
    if let Some(s) = p.slice_mut(chops, 5) {
        s.reverse = true;
    }

    let flip = p.patterns[0].id;
    p.patterns[0].name = "Flip".into();
    p.patterns[0].len = 2 * bar;
    for (step, pad) in [(0, 0), (2, 1), (4, 6), (6, 3), (8, 8), (10, 5), (12, 2), (14, 7), (16, 0), (18, 1), (20, 6), (22, 3), (24, 12), (26, 13), (28, 10), (30, 15)] {
        p.add_note(flip, chops, CHOP_BASE + pad % pads, step * STEP, 2 * STEP, DEFAULT_VEL);
    }
    let drums = p.add_pattern("Drums", bar);
    steps(&mut p, drums, kick, 0, "x.........x.....");
    steps(&mut p, drums, snare, 0, "....x.......x...");
    let line = p.add_pattern("Bass", 2 * bar);
    for (pitch, step, len) in [(33, 0, 6), (33, 10, 2), (40, 12, 4), (38, 16, 6), (38, 26, 2), (36, 28, 4)] {
        p.add_note(line, bass, pitch, step * STEP, len * STEP, DEFAULT_VEL);
    }

    let names = ["Chops", "Drums", "Bass"];
    for (t, name) in p.tracks.iter_mut().zip(names) {
        t.name = name.into();
    }
    let [t_chops, t_drums, t_bass] = std::array::from_fn(|i| p.tracks[i].id);
    // Chops alone for two bars, then everything, then the chops again to end.
    p.add_clip(t_chops, flip, 0, 10 * bar);
    p.add_clip(t_drums, drums, 2 * bar, 6 * bar);
    p.add_clip(t_bass, line, 2 * bar, 6 * bar);
    p
}

/// 122 BPM: four-on-the-floor, offbeat chord stabs and bass, a breakdown and a lead.
fn house() -> Project {
    let mut p = Project::blank();
    (p.bpm, p.swing) = (122.0, 0.5);
    p.master = Fx { tone: 0.95, crush: 0.0, wobble: 0.05, vinyl: 0.0, reverb: 0.25, volume: 0.8 };
    let [kick, snare, hat, keys, bass, lead] = kit(&p);
    let bar = p.bar_len();

    let kick_only = p.patterns[0].id;
    p.patterns[0].name = "Kick".into();
    steps(&mut p, kick_only, kick, 0, "x...x...x...x...");
    let beat = p.add_pattern("Beat", bar);
    steps(&mut p, beat, kick, 0, "x...x...x...x...");
    steps(&mut p, beat, snare, 0, "....x.......x...");
    steps(&mut p, beat, hat, 0, "..x...x...x...x.");

    // Am9, Fmaj7, Cmaj9, G13: two short stabs a bar, and the root on every offbeat.
    let progression = [4, 5, 3, 2];
    let stabs = p.add_pattern("Stabs", 4 * bar);
    let line = p.add_pattern("Bass", 4 * bar);
    for (b, &c) in progression.iter().enumerate() {
        let at = b as Tick * bar;
        for step in [3, 10] {
            for &n in CHORDS[c].notes {
                p.add_note(stabs, keys, n, at + step * STEP, 2 * STEP, 90);
            }
        }
        for step in [2, 6, 10, 14] {
            p.add_note(line, bass, CHORDS[c].bass, at + step * STEP, 2 * STEP, DEFAULT_VEL);
        }
    }
    let melody = p.add_pattern("Lead", 4 * bar);
    for (pitch, step, len) in [(76, 0, 4), (74, 6, 2), (72, 8, 6), (72, 16, 4), (69, 22, 2), (72, 24, 8), (76, 32, 4), (79, 38, 2), (76, 40, 6), (74, 48, 6), (71, 56, 8)] {
        p.add_note(melody, lead, pitch, step * STEP, len * STEP, DEFAULT_VEL);
    }

    let names = ["Drums", "Keys", "Bass"];
    for (t, name) in p.tracks.iter_mut().zip(names) {
        t.name = name.into();
    }
    let [t_drums, t_keys, t_bass] = std::array::from_fn(|i| p.tracks[i].id);
    let t_lead = p.add_track("Lead");
    // Intro, groove, breakdown (bars 9-12), drop with the lead, outro.
    p.add_clip(t_drums, kick_only, 0, 4 * bar);
    p.add_clip(t_drums, beat, 4 * bar, 4 * bar);
    p.add_clip(t_drums, beat, 12 * bar, 12 * bar);
    p.add_clip(t_keys, stabs, 4 * bar, 20 * bar);
    p.add_clip(t_bass, line, 4 * bar, 4 * bar);
    p.add_clip(t_bass, line, 12 * bar, 12 * bar);
    p.add_clip(t_lead, melody, 12 * bar, 8 * bar);
    p
}

fn write_wav(path: &Path, data: &SampleData) {
    let spec = hound::WavSpec { channels: 1, sample_rate: data.sr, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
    let mut w = hound::WavWriter::create(path, spec).unwrap();
    for f in &data.frames {
        w.write_sample((f[0].clamp(-1.0, 1.0) * i16::MAX as f32) as i16).unwrap();
    }
    w.finalize().unwrap();
}

#[test]
#[ignore = "writes demos/"]
fn write_demos() {
    let dir = dir();
    fs::create_dir_all(dir.join("samples")).unwrap();
    write_wav(&dir.join(BREAK), &dusty_break());
    // Chop the file as written (16-bit), as the app would.
    let data = sample::decode(&dir.join(BREAK)).unwrap();
    for (name, p) in [("lofi-loop", Project::demo()), ("boom-bap-chops", boom_bap(&data)), ("house-sketch", house())] {
        file::save(&dir.join(format!("{name}.{}", file::EXTENSION)), &p).unwrap();
    }
}

#[test]
fn demos_open() {
    let mut seen = 0;
    for entry in fs::read_dir(dir()).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != file::EXTENSION) {
            continue;
        }
        let p = file::load(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let mut samples = Vec::new();
        for chop in p.channels.iter().filter_map(|c| c.chop.as_ref()) {
            let data = sample::decode(&chop.path).unwrap_or_else(|e| panic!("{}: {e}", chop.path.display()));
            assert_eq!(data.frames.len() as u32, chop.len);
            samples.push((chop.path.clone(), Arc::new(data)));
        }
        // It plays: loud enough, and not clipping.
        let snap = p.compile_with(&|path| samples.iter().find(|(p, _)| p == path).map(|(_, d)| d.clone()));
        assert!(snap.notes.len() > 20, "{}", path.display());
        let peak = engine::render_offline(&snap, 8000, 1).iter().fold(0f32, |m, f| m.max(f[0].abs()).max(f[1].abs()));
        assert!((0.2..1.0).contains(&peak), "{}: peak {peak}", path.display());
        seen += 1;
    }
    assert_eq!(seen, 3);
}

/// How much faster than real time each demo renders at 44.1 kHz: the headroom the audio thread
/// has. `cargo test --release -- --ignored render_speed --nocapture`
#[test]
#[ignore = "benchmark; run in release"]
fn render_speed() {
    let mut paths: Vec<PathBuf> = fs::read_dir(dir()).unwrap().map(|e| e.unwrap().path()).filter(|p| p.extension().is_some_and(|e| e == file::EXTENSION)).collect();
    paths.sort();
    for path in paths {
        let p = file::load(&path).unwrap();
        let samples: Vec<(PathBuf, Arc<SampleData>)> = p.channels.iter().filter_map(|c| c.chop.as_ref()).map(|c| (c.path.clone(), Arc::new(sample::decode(&c.path).unwrap()))).collect();
        let snap = p.compile_with(&|path| samples.iter().find(|(p, _)| p == path).map(|(_, d)| d.clone()));
        let start = std::time::Instant::now();
        let frames = engine::render_offline(&snap, 44100, 1).len();
        let took = start.elapsed().as_secs_f64();
        println!("{}: {:.1} s of audio in {:.2} s, {:.0}x real time", path.file_name().unwrap().display(), frames as f64 / 44100.0, took, frames as f64 / 44100.0 / took);
    }
    // The worst case: every channel the engine allows, each playing on every 16th, the pianos
    // in four-note chords so every voice is busy and stealing.
    let mut p = Project::blank();
    let mixer = p.mixer[0].id;
    let kinds = [Instrument::Kick, Instrument::Snare, Instrument::Hat, Instrument::Rhodes, Instrument::Bass];
    for i in p.channels.len()..engine::MAX_CHANNELS {
        p.add_channel(&format!("C{i}"), kinds[i % kinds.len()], mixer);
    }
    let pat = p.patterns[0].id;
    let chans: Vec<ChannelId> = p.channels.iter().map(|c| c.id).collect();
    for (i, &ch) in chans.iter().enumerate() {
        for step in 0..16 {
            for k in 0..4 {
                p.add_note(pat, ch, 48 + (i % 12) as u8 + 4 * k, step * STEP, STEP, DEFAULT_VEL);
            }
        }
    }
    let track = p.tracks[0].id;
    p.add_clip(track, pat, 0, 8 * p.bar_len());
    let snap = p.compile();
    let start = std::time::Instant::now();
    let frames = engine::render_offline(&snap, 44100, 1).len();
    let took = start.elapsed().as_secs_f64();
    println!("worst case ({} channels, {} notes): {:.0}x real time", snap.channels.len(), snap.notes.len(), frames as f64 / 44100.0 / took);
}
