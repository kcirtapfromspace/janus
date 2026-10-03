//! How someone sounds, measured locally from their audio track: pitch (YIN), how animated their
//! pitch is, loudness, and pace. Used by the temperature timeline (temperature.rs) for each
//! interviewer turn and each of your answers. Nothing here leaves the Mac.
//!
//! Input is always 16 kHz mono f32 (`audio::SR`, from `audio::load`). Frames are 40 ms (640
//! samples) every 10 ms (160 samples).
//!
//! Pitch is YIN (de Cheveigné & Kawahara 2002): cumulative-mean-normalised difference over lags
//! 40–267 samples (60–400 Hz), absolute threshold 0.15, parabolic interpolation of the chosen lag.
//! A frame is voiced when its aperiodicity (the normalised difference at the chosen lag) is below
//! 0.25 and its RMS is more than 10 dB above the track's noise floor. Octave errors are dropped per
//! span: voiced frames more than 7 semitones from the span's median pitch are ignored.
//!
//! Features are compared with the same speaker's own interview, never across people: see
//! `robust_z`.

use serde::{Deserialize, Serialize};

use crate::audio::SR;

pub const FRAME: usize = 640;
pub const HOP: usize = 160;
pub const MIN_LAG: usize = 40;
pub const MAX_LAG: usize = 267;
pub const YIN_THRESHOLD: f64 = 0.15;
pub const MAX_APERIODICITY: f64 = 0.25;
pub const VOICED_ABOVE_FLOOR_DB: f64 = 10.0;
pub const OCTAVE_ERROR_ST: f64 = 7.0;
/// Spans with less voiced speech than this get no features (z = 0 downstream).
pub const MIN_VOICED_S: f64 = 1.5;

/// RMS of digital silence, in dBFS.
const SILENCE_DB: f64 = -120.0;
/// YIN's integration window: the difference at every lag compares the same 373 samples, so all
/// lags sum the same number of terms and the frame never reads past its 640 samples.
const WINDOW: usize = FRAME - MAX_LAG;
/// Voiced runs closer together than this are one burst.
const BURST_GAP_S: f64 = 0.2;
const HOP_S: f64 = HOP as f64 / SR as f64;

/// One analysis frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Frame {
    /// Centre of the frame, in seconds from the start of the track.
    pub t: f64,
    pub voiced: bool,
    /// Pitch, when voiced.
    pub f0_hz: Option<f64>,
    pub rms_db: f64,
}

/// How a span (a turn or an answer) sounds.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct VoiceFeatures {
    pub voiced_s: f64,
    pub median_f0_hz: f64,
    /// Within-span standard deviation of pitch, in semitones: how animated it sounds.
    pub pitch_var_st: f64,
    /// Mean RMS over voiced frames, in dBFS.
    pub energy_db: f64,
    /// Words per voiced second.
    pub words_per_s: f64,
}

/// The track's noise floor: a low percentile (10th) of frame RMS in dB.
pub fn noise_floor_db(samples: &[f32]) -> f64 {
    let mut db = frame_rms_db(samples);
    if db.is_empty() {
        return SILENCE_DB;
    }
    let k = (0.1 * (db.len() - 1) as f64).round() as usize;
    *db.select_nth_unstable_by(k, f64::total_cmp).1
}

/// Frame-by-frame pitch and loudness for a whole track.
///
/// Frames too quiet to count as voiced skip the pitch search, so silence costs almost nothing.
pub fn frames(samples: &[f32], noise_floor_db: f64) -> Vec<Frame> {
    let gate = noise_floor_db + VOICED_ABOVE_FLOOR_DB;
    let mut yin = Yin::default();
    frame_rms_db(samples)
        .into_iter()
        .enumerate()
        .map(|(i, rms_db)| {
            let at = i * HOP;
            let f0_hz = (rms_db > gate)
                .then(|| yin.pitch(&samples[at..at + FRAME]))
                .filter(|&(_, aperiodicity)| aperiodicity < MAX_APERIODICITY)
                .map(|(f0, _)| f0);
            Frame { t: (at + FRAME / 2) as f64 / SR as f64, voiced: f0_hz.is_some(), f0_hz, rms_db }
        })
        .collect()
}

/// Features for the frames inside [start, end), or None with under `MIN_VOICED_S` voiced.
/// `words` is the span's word count (from the transcript) for the pace.
///
/// `median_f0_hz` is taken over all voiced frames; `voiced_s`, the pace and `pitch_var_st` count
/// only the frames left after dropping octave errors.
pub fn span_features(frames: &[Frame], start: f64, end: f64, words: usize) -> Option<VoiceFeatures> {
    let voiced: Vec<&Frame> = in_span(frames, start, end).iter().filter(|f| f.voiced && f.f0_hz.is_some()).collect();
    let mut f0s: Vec<f64> = voiced.iter().filter_map(|f| f.f0_hz).collect();
    let median_f0_hz = median(&mut f0s)?;
    let offsets: Vec<f64> = f0s
        .iter()
        .map(|&f| 12.0 * (f / median_f0_hz).log2())
        .filter(|st| st.abs() <= OCTAVE_ERROR_ST)
        .collect();
    let voiced_s = offsets.len() as f64 * HOP_S;
    if voiced_s < MIN_VOICED_S {
        return None;
    }
    let n = offsets.len() as f64;
    let mean_st = offsets.iter().sum::<f64>() / n;
    let pitch_var_st = (offsets.iter().map(|st| (st - mean_st).powi(2)).sum::<f64>() / n).sqrt();
    let energy_db = voiced.iter().map(|f| f.rms_db).sum::<f64>() / voiced.len() as f64;
    Some(VoiceFeatures { voiced_s, median_f0_hz, pitch_var_st, energy_db, words_per_s: words as f64 / voiced_s })
}

/// Short voiced bursts (min_s..=max_s long, separated by unvoiced gaps of at least 0.2 s) inside
/// [start, end): an interviewer's "mm-hmm"s while you answer, found acoustically because the
/// transcript drops many of them. Returns (start, end) pairs.
///
/// A burst runs from its first voiced frame's centre to one hop past its last.
pub fn voiced_bursts(frames: &[Frame], start: f64, end: f64, min_s: f64, max_s: f64) -> Vec<(f64, f64)> {
    let mut runs: Vec<(f64, f64)> = Vec::new();
    for f in in_span(frames, start, end).iter().filter(|f| f.voiced) {
        match runs.last_mut() {
            Some(run) if f.t - run.1 - HOP_S < BURST_GAP_S - 1e-6 => run.1 = f.t,
            _ => runs.push((f.t, f.t)),
        }
    }
    runs.into_iter()
        .map(|(first, last)| (first, last + HOP_S))
        .filter(|(s, e)| (min_s..=max_s).contains(&(e - s)))
        .collect()
}

/// Robust z-scores: (x − median) / (1.4826 · MAD). With fewer than 8 values, or MAD of 0, all 0.
///
/// Non-finite values (NaN, ±inf) are left out of the median and MAD, don't count towards the 8,
/// and score 0.
pub fn robust_z(values: &[f64]) -> Vec<f64> {
    let mut finite: Vec<f64> = values.iter().copied().filter(|v| v.is_finite()).collect();
    let zeros = || vec![0.0; values.len()];
    if finite.len() < 8 {
        return zeros();
    }
    let Some(med) = median(&mut finite) else { return zeros() };
    let mut deviations: Vec<f64> = finite.iter().map(|v| (v - med).abs()).collect();
    let mad = median(&mut deviations).unwrap_or(0.0);
    if mad == 0.0 {
        return zeros();
    }
    let scale = 1.4826 * mad;
    values.iter().map(|&v| if v.is_finite() { (v - med) / scale } else { 0.0 }).collect()
}

/// Per-frame RMS in dBFS (floored at -120), from per-hop energies so each sample is squared once.
fn frame_rms_db(samples: &[f32]) -> Vec<f64> {
    if samples.len() < FRAME {
        return Vec::new();
    }
    let hops: Vec<f64> =
        samples.as_chunks::<HOP>().0.iter().map(|h| h.iter().map(|&x| f64::from(x) * f64::from(x)).sum()).collect();
    let n = (samples.len() - FRAME) / HOP + 1;
    hops.windows(FRAME / HOP)
        .take(n)
        .map(|w| {
            let mean_sq = w.iter().sum::<f64>() / FRAME as f64;
            if mean_sq > 0.0 { (10.0 * mean_sq.log10()).max(SILENCE_DB) } else { SILENCE_DB }
        })
        .collect()
}

/// Buffers for YIN, reused across frames.
struct Yin {
    diff: [f64; MAX_LAG + 1],
    norm: [f64; MAX_LAG + 1],
}

impl Default for Yin {
    fn default() -> Self {
        Self { diff: [0.0; MAX_LAG + 1], norm: [1.0; MAX_LAG + 1] }
    }
}

impl Yin {
    /// (f0 in Hz, aperiodicity) of one `FRAME`-long frame.
    fn pitch(&mut self, x: &[f32]) -> (f64, f64) {
        let head = &x[..WINDOW];
        let mut running = 0.0;
        for (tau, (d, n)) in self.diff.iter_mut().zip(self.norm.iter_mut()).enumerate().skip(1) {
            *d = f64::from(sq_diff(head, &x[tau..tau + WINDOW]));
            running += *d;
            *n = if running > 0.0 { *d * tau as f64 / running } else { 1.0 };
        }
        let norm = &self.norm;
        let tau = match (MIN_LAG..=MAX_LAG).find(|&t| norm[t] < YIN_THRESHOLD) {
            Some(mut t) => {
                while t < MAX_LAG && norm[t + 1] < norm[t] {
                    t += 1;
                }
                t
            }
            None => (MIN_LAG..=MAX_LAG).min_by(|&a, &b| norm[a].total_cmp(&norm[b])).unwrap_or(MIN_LAG),
        };
        let shift = if tau < MAX_LAG {
            let (a, b, c) = (self.diff[tau - 1], self.diff[tau], self.diff[tau + 1]);
            let curvature = a - 2.0 * b + c;
            if curvature > 0.0 { (0.5 * (a - c) / curvature).clamp(-0.5, 0.5) } else { 0.0 }
        } else {
            0.0
        };
        (f64::from(SR) / (tau as f64 + shift), norm[tau])
    }
}

/// Σ (a − b)² over equal-length slices, in 16 independent lanes so it vectorises.
fn sq_diff(a: &[f32], b: &[f32]) -> f32 {
    let (a_chunks, a_rest) = a.as_chunks::<16>();
    let (b_chunks, b_rest) = b.as_chunks::<16>();
    let mut lanes = [0f32; 16];
    for (x, y) in a_chunks.iter().zip(b_chunks) {
        for ((acc, &x), &y) in lanes.iter_mut().zip(x).zip(y) {
            let d = x - y;
            *acc += d * d;
        }
    }
    let rest: f32 = a_rest.iter().zip(b_rest).map(|(&x, &y)| (x - y) * (x - y)).sum();
    lanes.iter().sum::<f32>() + rest
}

/// The frames with start <= t < end (frames are in time order).
fn in_span(frames: &[Frame], start: f64, end: f64) -> &[Frame] {
    let lo = frames.partition_point(|f| f.t < start);
    let hi = frames.partition_point(|f| f.t < end).max(lo);
    &frames[lo..hi]
}

/// Median of finite values (sorts in place); None when empty.
fn median(v: &mut [f64]) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_unstable_by(f64::total_cmp);
    let mid = v.len() / 2;
    Some(if v.len() % 2 == 1 { v[mid] } else { (v[mid - 1] + v[mid]) / 2.0 })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::TAU;
    use std::path::{Path, PathBuf};
    use std::time::Instant;

    fn n(secs: f64) -> usize {
        (secs * SR as f64).round() as usize
    }

    /// Deterministic uniform noise with the given RMS in dBFS.
    fn noise(len: usize, rms_db: f64, seed: u64) -> Vec<f32> {
        let amp = 10f64.powf(rms_db / 20.0) * 3f64.sqrt();
        let mut x = seed;
        (0..len)
            .map(|_| {
                x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                ((((x >> 11) as f64 / (1u64 << 53) as f64) * 2.0 - 1.0) * amp) as f32
            })
            .collect()
    }

    fn add_noise(x: &mut [f32], rms_db: f64, seed: u64) {
        let added = noise(x.len(), rms_db, seed);
        for (s, e) in x.iter_mut().zip(added) {
            *s += e;
        }
    }

    /// A tone following `pitch(t)`, made of `harmonics` (number, relative amplitude), peaking near `amp`.
    fn tone(secs: f64, pitch: impl Fn(f64) -> f64, harmonics: &[(usize, f64)], amp: f64) -> Vec<f32> {
        let total: f64 = harmonics.iter().map(|h| h.1).sum();
        let mut phase = 0.0;
        (0..n(secs))
            .map(|i| {
                phase += TAU * pitch(i as f64 / SR as f64) / SR as f64;
                let s: f64 = harmonics.iter().map(|&(k, a)| a * (k as f64 * phase).sin()).sum();
                (amp * s / total) as f32
            })
            .collect()
    }

    fn sine(secs: f64, hz: f64) -> Vec<f32> {
        tone(secs, |_| hz, &[(1, 1.0)], 0.3)
    }

    /// `secs` of silence on either side of `x`, then faint noise everywhere.
    fn padded(x: Vec<f32>, secs: f64, seed: u64) -> Vec<f32> {
        let mut out = vec![0.0; n(secs)];
        out.extend(x);
        out.extend(vec![0.0; n(secs)]);
        add_noise(&mut out, -70.0, seed);
        out
    }

    fn voiced_f0s<'a>(frames: impl IntoIterator<Item = &'a Frame>) -> Vec<f64> {
        frames.into_iter().filter_map(|f| f.f0_hz).collect()
    }

    fn frame(i: usize, f0_hz: Option<f64>, rms_db: f64) -> Frame {
        Frame { t: (i * HOP + FRAME / 2) as f64 / SR as f64, voiced: f0_hz.is_some(), f0_hz, rms_db }
    }

    #[test]
    fn sine_tones_get_their_pitch() {
        for (seed, hz) in [100.0, 150.0, 220.0, 300.0].into_iter().enumerate() {
            let x = padded(sine(2.0, hz), 0.5, seed as u64);
            let floor = noise_floor_db(&x);
            assert!((-73.0..-67.0).contains(&floor), "{hz} Hz: noise floor {floor}");
            let fr = frames(&x, floor);
            assert_eq!(fr.len(), (x.len() - FRAME) / HOP + 1);
            let inside: Vec<&Frame> = fr.iter().filter(|f| f.t >= 0.52 && f.t < 2.48).collect();
            let voiced = inside.iter().filter(|f| f.voiced).count();
            assert!(voiced as f64 >= 0.98 * inside.len() as f64, "{hz} Hz: {voiced}/{} voiced", inside.len());
            let mut f0s = voiced_f0s(inside.iter().copied());
            let worst = f0s.iter().map(|f| (f / hz - 1.0).abs()).fold(0.0, f64::max);
            let med = median(&mut f0s).unwrap();
            assert!((med / hz - 1.0).abs() < 0.01, "{hz} Hz: median {med}");
            assert!(worst < 0.02, "{hz} Hz: a frame was {:.1}% off", worst * 100.0);
            assert!(fr.iter().filter(|f| f.t < 0.48 || f.t > 2.52).all(|f| !f.voiced), "{hz} Hz: noise was voiced");
        }
    }

    #[test]
    fn missing_fundamental_is_found() {
        let x = padded(tone(2.0, |_| 140.0, &[(2, 1.0), (3, 0.8), (4, 0.6), (5, 0.5), (6, 0.4)], 0.3), 0.5, 7);
        let fr = frames(&x, noise_floor_db(&x));
        let inside: Vec<&Frame> = fr.iter().filter(|f| f.t >= 0.52 && f.t < 2.48).collect();
        let mut f0s = voiced_f0s(inside.iter().copied());
        assert!(f0s.len() as f64 >= 0.95 * inside.len() as f64, "{}/{} voiced", f0s.len(), inside.len());
        let med = median(&mut f0s).unwrap();
        assert!((med / 140.0 - 1.0).abs() < 0.02, "median {med}");
    }

    #[test]
    fn white_noise_is_unvoiced() {
        let x = noise(n(3.0), -30.0, 11);
        let floor = noise_floor_db(&x);
        assert!((-31.0..-29.0).contains(&floor), "floor {floor}");
        assert!(frames(&x, floor).iter().all(|f| !f.voiced));
        // With a quiet floor every frame passes the loudness gate, so this is YIN's aperiodicity.
        let fr = frames(&x, -80.0);
        let unvoiced = fr.iter().filter(|f| !f.voiced).count();
        assert!(unvoiced as f64 >= 0.9 * fr.len() as f64, "{unvoiced}/{} unvoiced", fr.len());
        assert!(fr.iter().all(|f| (-31.5..-28.5).contains(&f.rms_db)));
    }

    #[test]
    fn silence_and_very_quiet_tracks() {
        let zeros = vec![0.0f32; n(2.0)];
        assert_eq!(noise_floor_db(&zeros), -120.0);
        let fr = frames(&zeros, noise_floor_db(&zeros));
        assert!(!fr.is_empty() && fr.iter().all(|f| !f.voiced && f.f0_hz.is_none() && f.rms_db == -120.0));

        let quiet = noise(n(2.0), -90.0, 3);
        let floor = noise_floor_db(&quiet);
        assert!((-91.0..-89.0).contains(&floor), "floor {floor}");
        assert!(frames(&quiet, floor).iter().all(|f| !f.voiced));

        assert_eq!(noise_floor_db(&[]), -120.0);
        assert!(frames(&[0.1; FRAME - 1], -120.0).is_empty());
        assert_eq!(frames(&[0.0; FRAME], -120.0).len(), 1);
    }

    #[test]
    fn frames_are_centred_every_hop() {
        let fr = frames(&vec![0.0; n(1.0)], -120.0);
        assert_eq!(fr.len(), 97);
        assert!((fr[0].t - 0.02).abs() < 1e-12 && (fr[1].t - 0.03).abs() < 1e-12);
    }

    #[test]
    fn glide_is_more_animated_than_a_steady_tone() {
        let voice = [(1, 1.0), (2, 0.6), (3, 0.4), (4, 0.25)];
        let steady = padded(tone(3.0, |_| 150.0, &voice, 0.3), 0.5, 5);
        let glide = padded(tone(3.0, |t| 150.0 * 2f64.powf(0.25 * (TAU * t / 1.5).sin()), &voice, 0.3), 0.5, 6);
        let features = |x: &[f32]| span_features(&frames(x, noise_floor_db(x)), 0.5, 3.5, 9).unwrap();
        let (s, g) = (features(&steady), features(&glide));
        assert!(s.pitch_var_st < 0.2, "steady {s:?}");
        assert!((1.6..2.6).contains(&g.pitch_var_st), "glide {g:?}"); // ±3 st sine: 3/√2 ≈ 2.1
        assert!((s.median_f0_hz / 150.0 - 1.0).abs() < 0.01);
        assert!(s.voiced_s > 2.9 && (s.words_per_s - 9.0 / s.voiced_s).abs() < 1e-12);
        assert!((s.energy_db - crate::audio::rms_dbfs(&steady[n(0.6)..n(3.4)])).abs() < 0.5, "steady {s:?}");
    }

    #[test]
    fn span_features_drop_octave_errors() {
        let up = 150.0 * 2f64.powf(1.0 / 12.0);
        let down = 150.0 * 2f64.powf(-1.0 / 12.0);
        let mut f0s = vec![150.0; 200];
        f0s.extend([up; 50]);
        f0s.extend([down; 50]);
        f0s.extend([300.0; 30]);
        f0s.extend([75.0; 10]);
        let mut fr: Vec<Frame> = (0..100).map(|i| frame(i, Some(400.0), -10.0)).collect(); // before the span
        fr.extend(f0s.iter().enumerate().map(|(i, &f)| frame(100 + i, Some(f), -20.0)));
        fr.extend((440..500).map(|i| frame(i, None, -60.0)));
        let (start, end) = (fr[100].t, fr[499].t + 1e-9);

        let v = span_features(&fr, start, end, 12).unwrap();
        assert_eq!(v.median_f0_hz, 150.0);
        assert!((v.voiced_s - 3.0).abs() < 1e-12, "{v:?}");
        assert!((v.pitch_var_st - (1.0f64 / 3.0).sqrt()).abs() < 1e-9, "{v:?}");
        assert!((v.words_per_s - 4.0).abs() < 1e-12, "{v:?}");
        assert!((v.energy_db + 20.0).abs() < 1e-12, "unvoiced frames left out of energy: {v:?}");

        let mut halves = vec![frame(0, Some(150.0), -20.0); 140];
        halves.extend(vec![frame(0, Some(300.0), -20.0); 60]);
        for (i, f) in halves.iter_mut().enumerate() {
            f.t = frame(i, None, 0.0).t;
        }
        assert_eq!(span_features(&halves, 0.0, 10.0, 5), None, "1.4 s left once octave errors go");
    }

    #[test]
    fn span_features_need_enough_voiced_speech() {
        let track = |voiced: usize| -> Vec<Frame> {
            (0..1000).map(|i| frame(i, (i < voiced).then_some(120.0), if i < voiced { -25.0 } else { -70.0 })).collect()
        };
        assert_eq!(span_features(&track(149), 0.0, 10.0, 10), None);
        let v = span_features(&track(150), 0.0, 10.0, 10).unwrap();
        assert_eq!((v.voiced_s, v.median_f0_hz, v.pitch_var_st, v.energy_db), (1.5, 120.0, 0.0, -25.0));
        let v = span_features(&track(200), 0.0, 10.0, 10).unwrap();
        assert!((v.words_per_s - 5.0).abs() < 1e-12, "10 words over 2 voiced seconds, not the 10 s span");
        assert_eq!(span_features(&track(1000), 5.0, 6.0, 10), None, "only 1 s inside the span");
        assert_eq!(span_features(&track(1000), 6.0, 5.0, 10), None);
        assert_eq!(span_features(&[], 0.0, 10.0, 10), None);
    }

    #[test]
    fn span_bounds_are_half_open() {
        let fr: Vec<Frame> = (0..400).map(|i| frame(i, Some(if i < 200 { 100.0 } else { 200.0 }), -20.0)).collect();
        let v = span_features(&fr, fr[0].t, fr[200].t, 0).unwrap();
        assert_eq!((v.median_f0_hz, v.voiced_s), (100.0, 2.0));
        let v = span_features(&fr, fr[200].t, fr[399].t + 1.0, 0).unwrap();
        assert_eq!((v.median_f0_hz, v.voiced_s), (200.0, 2.0));
    }

    #[test]
    fn bursts_from_audio() {
        let voice = [(1, 1.0), (2, 0.5), (3, 0.3)];
        let mut x = Vec::new();
        for (gap, len) in [(1.0, 0.3), (1.0, 0.8), (1.0, 2.5)] {
            x.extend(vec![0.0; n(gap)]);
            x.extend(tone(len, |_| 180.0, &voice, 0.3));
        }
        x.extend(vec![0.0; n(1.0)]);
        add_noise(&mut x, -70.0, 21);
        let fr = frames(&x, noise_floor_db(&x));
        let end = x.len() as f64 / SR as f64;

        let short = voiced_bursts(&fr, 0.0, end, 0.15, 1.5);
        assert_eq!(short.len(), 2, "{short:?}");
        for (&(s, e), (want_s, want_len)) in short.iter().zip([(1.0, 0.3), (2.3, 0.8)]) {
            assert!((s - want_s).abs() < 0.04 && (e - s - want_len).abs() < 0.06, "{short:?}");
        }
        let all = voiced_bursts(&fr, 0.0, end, 0.15, 10.0);
        assert_eq!(all.len(), 3, "{all:?}");
        assert!((all[2].1 - all[2].0 - 2.5).abs() < 0.06, "{all:?}");
        assert_eq!(voiced_bursts(&fr, 2.0, end, 0.15, 1.5).len(), 1, "only the 0.8 s burst starts after 2 s");
    }

    #[test]
    fn short_dropouts_dont_split_a_burst() {
        let voice = [(1, 1.0), (2, 0.5), (3, 0.3)];
        let build = |dropout: f64| {
            let mut x = vec![0.0; n(1.0)];
            x.extend(tone(0.5, |_| 200.0, &voice, 0.3));
            x.extend(vec![0.0; n(dropout)]);
            x.extend(tone(0.5, |_| 200.0, &voice, 0.3));
            x.extend(vec![0.0; n(1.0)]);
            add_noise(&mut x, -70.0, 9);
            let fr = frames(&x, noise_floor_db(&x));
            voiced_bursts(&fr, 0.0, 10.0, 0.15, 1.5)
        };
        let merged = build(0.1);
        assert_eq!(merged.len(), 1, "{merged:?}");
        assert!((merged[0].1 - merged[0].0 - 1.1).abs() < 0.06, "{merged:?}");
        assert_eq!(build(0.5).len(), 2);
    }

    #[test]
    fn burst_gap_threshold_is_exact() {
        let with_gap = |gap: usize| -> Vec<(f64, f64)> {
            let fr: Vec<Frame> = (0..50 + gap + 50 + 100)
                .map(|i| frame(i, (i < 50 || (50 + gap..100 + gap).contains(&i)).then_some(150.0), -20.0))
                .collect();
            voiced_bursts(&fr, 0.0, 100.0, 0.0, 10.0)
        };
        assert_eq!(with_gap(19).len(), 1, "0.19 s gap merges");
        let split = with_gap(20);
        assert_eq!(split.len(), 2, "0.2 s gap splits");
        assert!((split[0].1 - split[0].0 - 0.5).abs() < 1e-9 && (split[1].0 - split[0].1 - 0.2).abs() < 1e-9);
        assert!(voiced_bursts(&[], 0.0, 1.0, 0.0, 1.0).is_empty());
    }

    #[test]
    fn robust_z_scores() {
        let v: Vec<f64> = (1..=9).map(f64::from).collect();
        let z = robust_z(&v);
        let scale = 1.4826 * 2.0; // median 5, MAD 2
        for (zi, vi) in z.iter().zip(&v) {
            assert!((zi - (vi - 5.0) / scale).abs() < 1e-12);
        }
        assert_eq!(z[4], 0.0);
        assert!((z[8] - 1.3490).abs() < 1e-4);

        assert_eq!(robust_z(&v[..7]), vec![0.0; 7], "fewer than 8");
        assert_eq!(robust_z(&[5.0, 5.0, 5.0, 5.0, 5.0, 5.0, 5.0, 5.0, 9.0]), vec![0.0; 9], "MAD 0");
        assert!(robust_z(&[]).is_empty());

        let mut with_nan = v.clone();
        with_nan.insert(3, f64::NAN);
        with_nan.push(f64::INFINITY);
        let zn = robust_z(&with_nan);
        assert_eq!(zn.len(), 11);
        assert_eq!((zn[3], zn[10]), (0.0, 0.0));
        let kept: Vec<f64> = zn.iter().enumerate().filter(|&(i, _)| i != 3 && i != 10).map(|(_, &z)| z).collect();
        assert_eq!(kept, z);
        let mut seven = v[..7].to_vec();
        seven.push(f64::NAN);
        assert_eq!(robust_z(&seven), vec![0.0; 8], "a NaN isn't an eighth value");
        assert_eq!(robust_z(&[f64::NAN; 10]), vec![0.0; 10]);
    }

    /// tests/fixtures/out/strong (made by the pipeline tests), here or in a checkout this worktree lives in.
    fn strong_fixture() -> Option<PathBuf> {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .map(|d| d.join("tests/fixtures/out/strong"))
            .find(|d| ["mic.wav", "system.wav", "expected.json"].iter().all(|f| d.join(f).exists()))
    }

    fn read_wav(path: &Path) -> Vec<f32> {
        let mut r = hound::WavReader::open(path).unwrap();
        assert_eq!((r.spec().sample_rate, r.spec().channels), (SR, 1));
        r.samples::<f32>().map(|s| s.unwrap()).collect()
    }

    #[derive(Deserialize)]
    struct Line {
        speaker: String,
        start: f64,
        end: f64,
        text: String,
    }

    #[test]
    #[cfg_attr(debug_assertions, ignore = "slow in debug builds: run with --release")]
    fn real_voices_from_fixtures() {
        let Some(dir) = strong_fixture() else {
            println!("skipping: no tests/fixtures/out/strong (run the pipeline tests to make it)");
            return;
        };
        let lines: Vec<Line> = serde_json::from_str(&std::fs::read_to_string(dir.join("expected.json")).unwrap()).unwrap();
        let mut medians = Vec::new();
        for (speaker, wav) in [("you", "mic.wav"), ("interviewer", "system.wav")] {
            let x = read_wav(&dir.join(wav));
            let floor = noise_floor_db(&x);
            let fr = frames(&x, floor);
            let spans: Vec<&Line> = lines.iter().filter(|l| l.speaker == speaker).collect();
            let features: Vec<VoiceFeatures> = spans
                .iter()
                .filter_map(|l| span_features(&fr, l.start, l.end, l.text.split_whitespace().count()))
                .collect();
            assert!(features.len() * 4 >= spans.len() * 3, "{speaker}: {}/{} spans measured", features.len(), spans.len());
            for v in &features {
                let all = [v.voiced_s, v.median_f0_hz, v.pitch_var_st, v.energy_db, v.words_per_s];
                assert!(all.iter().all(|x| x.is_finite()), "{speaker}: {v:?}");
                assert!((60.0..=400.0).contains(&v.median_f0_hz) && v.words_per_s > 0.5, "{speaker}: {v:?}");
            }
            let med = |get: fn(&VoiceFeatures) -> f64| median(&mut features.iter().map(get).collect::<Vec<_>>()).unwrap();
            let f0 = med(|v| v.median_f0_hz);
            println!(
                "{speaker} ({wav}): floor {floor:.1} dB, {}/{} spans, median f0 {f0:.1} Hz, pitch var {:.2} st, \
                 energy {:.1} dB, {:.2} words/voiced s",
                features.len(),
                spans.len(),
                med(|v| v.pitch_var_st),
                med(|v| v.energy_db),
                med(|v| v.words_per_s),
            );
            medians.push(f0);
        }
        assert!(medians[0] > medians[1], "Samantha {:.1} Hz should be above Daniel {:.1} Hz", medians[0], medians[1]);
        assert!((140.0..260.0).contains(&medians[0]) && (80.0..150.0).contains(&medians[1]), "{medians:?}");
    }

    /// Speech-like: syllables at ~4 Hz with a drifting pitch and harmonics, pauses, and room noise.
    fn speechlike(secs: f64) -> Vec<f32> {
        let voice = [(1, 1.0), (2, 0.7), (3, 0.5), (4, 0.3), (5, 0.2)];
        let mut x = tone(
            secs,
            |t| 160.0 * 2f64.powf(0.4 * (TAU * t / 2.7).sin() + 0.2 * (TAU * t / 0.9).sin()),
            &voice,
            0.25,
        );
        for (i, s) in x.iter_mut().enumerate() {
            let t = i as f64 / SR as f64;
            let syllable = (TAU * 4.0 * t).sin().max(0.0);
            let pause = if t % 7.0 > 5.5 { 0.0 } else { 1.0 };
            *s *= (syllable * pause) as f32;
        }
        add_noise(&mut x, -60.0, 17);
        x
    }

    #[test]
    #[cfg_attr(debug_assertions, ignore = "timing is only meaningful with --release")]
    fn ten_minutes_in_well_under_a_few_seconds() {
        let x = speechlike(600.0);
        let started = Instant::now();
        let fr = frames(&x, noise_floor_db(&x));
        let took = started.elapsed().as_secs_f64();
        let voiced = fr.iter().filter(|f| f.voiced).count();
        println!("10 min speech-like: {} frames ({voiced} voiced) in {took:.3} s", fr.len());
        assert!(voiced * 3 > fr.len(), "{voiced}/{} voiced: not speech-like enough to time", fr.len());
        assert!(took < 3.0, "took {took:.2} s");

        // Worst case: a floor so low that every frame gets the full pitch search.
        let started = Instant::now();
        frames(&x, -200.0);
        let took = started.elapsed().as_secs_f64();
        println!("10 min, pitch search on every frame: {took:.3} s");
        assert!(took < 3.0, "took {took:.2} s");
    }
}
