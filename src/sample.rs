//! Audio files for the chop sampler: decoding (WAV, FLAC, MP3, OGG Vorbis, AIFF), finding the hits
//! to chop at, and waveform overviews for drawing. This runs on the UI side or a loader thread,
//! never on the audio thread.

use crate::engine::{MAX_SLICES, SampleData};
use std::fs::File;
use std::path::Path;
use symphonia::core::errors::Error;
use symphonia::core::formats::TrackType;
use symphonia::core::formats::probe::Hint;
use symphonia::core::io::MediaSourceStream;

/// Longest audio the sampler loads, in seconds (it costs about 8 bytes per frame in memory).
pub const MAX_SECONDS: usize = 600;

/// Decodes an audio file to stereo frames at its own sample rate. Mono files play on both sides;
/// only the first two channels of a surround file are kept.
pub fn decode(path: &Path) -> Result<SampleData, String> {
    let file = File::open(path).map_err(|e| e.to_string())?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let unsupported = |e: Error| format!("not an audio file Ritmo can read ({e})");
    let mut format = symphonia::default::get_probe().probe(&hint, mss, Default::default(), Default::default()).map_err(unsupported)?;
    let track = format.default_track(TrackType::Audio).ok_or("the file has no audio")?;
    let id = track.id;
    let params = track.codec_params.as_ref().and_then(|p| p.audio()).ok_or("the file has no audio")?;
    let mut decoder = symphonia::default::get_codecs().make_audio_decoder(params, &Default::default()).map_err(unsupported)?;

    let (mut sr, mut frames, mut buf) = (0u32, Vec::new(), Vec::<f32>::new());
    loop {
        let packet = match format.next_packet() {
            Ok(Some(p)) => p,
            Ok(None) => break,
            // A truncated file: keep what decoded.
            Err(_) if !frames.is_empty() => break,
            Err(e) => return Err(unsupported(e)),
        };
        if packet.track_id != id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(audio) => {
                sr = audio.spec().rate();
                let channels = audio.spec().channels().count().max(1);
                buf.resize(audio.samples_interleaved(), 0.0);
                audio.copy_to_slice_interleaved(&mut buf);
                frames.extend(buf.chunks_exact(channels).map(|f| if channels == 1 { [f[0], f[0]] } else { [f[0], f[1]] }));
                if frames.len() > MAX_SECONDS * sr as usize {
                    return Err(format!("longer than {} minutes", MAX_SECONDS / 60));
                }
            }
            // A damaged packet: skip it.
            Err(Error::DecodeError(_)) => continue,
            Err(e) => return Err(unsupported(e)),
        }
    }
    if frames.is_empty() || sr == 0 {
        return Err("the file has no audio".into());
    }
    Ok(SampleData { sr, frames })
}

/// `n` equal slices of `len` frames: where each starts.
pub fn even_points(len: u32, n: usize) -> Vec<u32> {
    let n = n.clamp(1, MAX_SLICES) as u64;
    (0..n).map(|k| (k * len as u64 / n) as u32).collect()
}

/// Where the hits are: frames where the level jumps. `sensitivity` 0..1 finds only big jumps
/// at 0 and small ones at 1. Returns at most `max` points, in order, at least 50 ms apart, and
/// always at least one point (the start of the sound).
pub fn detect_hits(data: &SampleData, sensitivity: f32, max: usize) -> Vec<u32> {
    let hop = (data.sr as usize / 200).max(1); // 5 ms
    let env: Vec<f32> = data
        .frames
        .chunks(hop)
        .map(|c| (c.iter().map(|f| ((f[0] + f[1]) * 0.5).powi(2)).sum::<f32>() / c.len() as f32).sqrt())
        .collect();
    let loudest = env.iter().copied().fold(0.0, f32::max);
    if loudest <= 0.0 {
        return vec![0];
    }
    // Ignore anything 40 dB below the loudest part.
    let floor = loudest * 0.01;
    let db = |x: f32| 20.0 * (x + 1e-6).log10();
    // How far the level over the next 10 ms rises above the loudest 5 ms of the 30 ms before.
    // (Two blocks, so a hit starting mid-block still counts in full.)
    let rise: Vec<f32> = (0..env.len())
        .map(|i| {
            let before = env[i.saturating_sub(6)..i].iter().copied().fold(0.0, f32::max);
            let now = env[i].max(env.get(i + 1).copied().unwrap_or(0.0));
            if now < floor { 0.0 } else { (db(now) - db(before)).max(0.0) }
        })
        .collect();
    let threshold = 2.0 + (1.0 - sensitivity.clamp(0.0, 1.0)) * 12.0;
    let local_peak = |i: usize| rise[i.saturating_sub(2)..(i + 3).min(rise.len())].iter().all(|&r| r <= rise[i]);
    let mut found: Vec<usize> = (0..rise.len()).filter(|&i| rise[i] >= threshold && local_peak(i)).collect();
    // Strongest first, dropping any within 50 ms of a stronger one.
    found.sort_by(|&a, &b| rise[b].total_cmp(&rise[a]));
    let mut kept: Vec<usize> = Vec::new();
    for i in found {
        if kept.iter().all(|&k| k.abs_diff(i) >= 10) {
            kept.push(i);
        }
    }
    kept.truncate(max.clamp(1, MAX_SLICES));
    let mut points: Vec<u32> = kept.iter().map(|&i| (i * hop) as u32).collect();
    if points.is_empty() {
        let first = env.iter().position(|&e| e >= floor).unwrap_or(0);
        points.push((first * hop) as u32);
    }
    points.sort_unstable();
    points
}

/// Waveform overview for drawing: the lowest and highest mono value in each of `buckets` equal
/// parts of `start..end`.
pub fn overview(data: &SampleData, start: usize, end: usize, buckets: usize) -> Vec<[f32; 2]> {
    let end = end.min(data.frames.len());
    if buckets == 0 || start >= end {
        return Vec::new();
    }
    (0..buckets)
        .map(|b| {
            let (a, z) = (start + (end - start) * b / buckets, start + (end - start) * (b + 1) / buckets);
            data.frames[a..z.max(a + 1).min(end)].iter().fold([f32::MAX, f32::MIN], |[lo, hi], f| {
                let m = (f[0] + f[1]) * 0.5;
                [lo.min(m), hi.max(m)]
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::TAU;

    /// Decaying 200 Hz hits at `at` seconds over quiet noise, plus a steady tone at `bed` level.
    fn hits_over(sr: u32, seconds: f32, at: &[f32], bed: f32) -> SampleData {
        let n = (sr as f32 * seconds) as usize;
        let mut seed = 0x1234_5678u32;
        let mut frames: Vec<[f32; 2]> = (0..n)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                let noise = (seed as f32 / u32::MAX as f32 - 0.5) * 0.002; // about -60 dB
                [noise, noise]
            })
            .collect();
        for (i, f) in frames.iter_mut().enumerate() {
            let x = (TAU * 330.0 * i as f32 / sr as f32).sin() * bed;
            f[0] += x;
            f[1] += x;
        }
        for &t in at {
            let s = (t * sr as f32) as usize;
            for (i, f) in frames[s..].iter_mut().enumerate().take(sr as usize / 4) {
                let x = (TAU * 200.0 * i as f32 / sr as f32).sin() * 0.8 * (-(i as f32) / sr as f32 * 12.0).exp();
                f[0] += x;
                f[1] += x;
            }
        }
        SampleData { sr, frames }
    }

    fn hits(sr: u32, seconds: f32, at: &[f32]) -> SampleData {
        hits_over(sr, seconds, at, 0.0)
    }

    #[test]
    fn finds_hits_over_a_held_note() {
        // Hits between the 5 ms blocks, over music 15 dB below them. The music starting at 0 s
        // counts as the first hit.
        let at = [0.1027, 0.6013, 1.1041, 1.6033];
        let found = detect_hits(&hits_over(44100, 2.0, &at, 0.14), 0.5, 16);
        let truth = [0.0, at[0], at[1], at[2], at[3]];
        assert_eq!(found.len(), truth.len(), "{found:?}");
        for (f, t) in found.iter().zip(truth) {
            assert!((*f as f32 / 44100.0 - t).abs() < 0.01, "found {f} for {t} s");
        }
    }

    #[test]
    fn finds_the_hits() {
        let truth = [0.25, 0.75, 1.3, 1.62];
        let data = hits(8000, 2.0, &truth);
        let found = detect_hits(&data, 0.5, 16);
        assert_eq!(found.len(), truth.len(), "{found:?}");
        for (f, t) in found.iter().zip(truth) {
            let err = (*f as f32 / 8000.0 - t).abs();
            assert!(err < 0.01, "found {f} for {t} s");
        }
        // Asking for fewer keeps the strongest, still in order.
        let two = detect_hits(&data, 0.5, 2);
        assert_eq!(two.len(), 2);
        assert!(two[0] < two[1]);
    }

    #[test]
    fn silence_and_steady_sound_still_give_one_point() {
        let silent = SampleData { sr: 8000, frames: vec![[0.0; 2]; 8000] };
        assert_eq!(detect_hits(&silent, 1.0, 16), [0]);
        let steady: Vec<[f32; 2]> = (0..8000).map(|i| [(i as f32 * 0.1).sin() * 0.5; 2]).collect();
        let found = detect_hits(&SampleData { sr: 8000, frames: steady }, 0.5, 16);
        assert_eq!(found, [0]);
    }

    #[test]
    fn even_slices() {
        assert_eq!(even_points(1600, 4), [0, 400, 800, 1200]);
        assert_eq!(even_points(10, 1), [0]);
        assert_eq!(even_points(1000, 99).len(), MAX_SLICES);
    }

    #[test]
    fn waveform_overview() {
        let data = hits(8000, 1.0, &[0.5]);
        let o = overview(&data, 0, data.frames.len(), 100);
        assert_eq!(o.len(), 100);
        assert!(o[..49].iter().all(|[lo, hi]| hi - lo < 0.01));
        assert!(o[50][1] > 0.3 && o[50][0] < -0.3);
        assert!(overview(&data, 10, 5, 10).is_empty());
    }

    #[test]
    fn decodes_wav_files() {
        let dir = std::env::temp_dir().join(format!("ritmo-sample-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // Stereo 16-bit: left a ramp, right silent.
        let path = dir.join("stereo.wav");
        let spec = hound::WavSpec { channels: 2, sample_rate: 22050, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
        let mut w = hound::WavWriter::create(&path, spec).unwrap();
        for i in 0..1000 {
            w.write_sample((i * 30) as i16).unwrap();
            w.write_sample(0i16).unwrap();
        }
        w.finalize().unwrap();
        let d = decode(&path).unwrap();
        assert_eq!((d.sr, d.frames.len()), (22050, 1000));
        assert!((d.frames[500][0] - 500.0 * 30.0 / 32768.0).abs() < 1e-3 && d.frames[500][1] == 0.0);

        // Mono float: both sides.
        let path = dir.join("mono.wav");
        let spec = hound::WavSpec { channels: 1, sample_rate: 8000, bits_per_sample: 32, sample_format: hound::SampleFormat::Float };
        let mut w = hound::WavWriter::create(&path, spec).unwrap();
        for i in 0..400 {
            w.write_sample(i as f32 / 400.0).unwrap();
        }
        w.finalize().unwrap();
        let d = decode(&path).unwrap();
        assert_eq!((d.sr, d.frames.len()), (8000, 400));
        assert_eq!(d.frames[200][0], d.frames[200][1]);

        let junk = dir.join("junk.mp3");
        std::fs::write(&junk, b"this is not audio at all").unwrap();
        assert!(decode(&junk).is_err());
        assert!(decode(&dir.join("missing.wav")).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
