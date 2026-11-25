//! Volume leveling for digitising old tapes (ArchiefRecorder).
//!
//! Works after the recording, so it can look at the whole file:
//! 1. `analyze` measures the loudness (EBU R128 / ITU-R BS.1770, K-weighted)
//!    in blocks of 100 ms.
//! 2. `GainCurve::compute` turns that into a smooth gain per block: towards a
//!    target loudness, limited to a maximum boost and cut, never boosting
//!    silence or tape hiss (gate), and smoothed both forwards and backwards so
//!    the gain already moves before a loud or quiet passage starts.
//! 3. `Limiter` is a look-ahead peak limiter: it sees peaks coming and turns
//!    the volume down just before them, so nothing ever exceeds the ceiling.
//!
//! The original recording is never changed: gain and limiter are applied
//! while exporting and while listening.

use crate::wav::WavReader;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

/// Length of one analysis block.
pub const BLOCK_MS: u32 = 100;
/// Window over which loudness is measured ("short-term", 3 s).
const WINDOW_BLOCKS: usize = 30;

/// How quickly the leveler follows changes in volume.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Speed {
    Calm,
    Normal,
    Fast,
}

impl Speed {
    /// How far the leveler looks before and after, in seconds.
    pub fn window(self) -> f32 {
        match self {
            Speed::Calm => 20.0,
            Speed::Normal => 8.0,
            Speed::Fast => 3.0,
        }
    }
    /// Time constant of the smoothing, in seconds.
    pub fn seconds(self) -> f32 {
        self.window() / 2.0
    }
    pub fn label(self) -> &'static str {
        match self {
            Speed::Calm => "Rustig",
            Speed::Normal => "Normaal",
            Speed::Fast => "Snel",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct LevelParams {
    /// Target loudness in LUFS.
    pub target_lufs: f32,
    /// Never louder than this (dB).
    pub max_boost_db: f32,
    /// Never softer than this (dB, positive number).
    pub max_cut_db: f32,
    /// Passages quieter than this (LUFS) count as silence or hiss: not boosted.
    pub gate_lufs: f32,
    pub speed: Speed,
}

impl Default for LevelParams {
    fn default() -> Self {
        LevelParams { target_lufs: -16.0, max_boost_db: 12.0, max_cut_db: 12.0, gate_lufs: -45.0, speed: Speed::Normal }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct LimiterParams {
    /// Highest allowed sample peak, in dBFS.
    pub ceiling_db: f32,
    pub lookahead_ms: f32,
    pub release_ms: f32,
}

impl Default for LimiterParams {
    fn default() -> Self {
        LimiterParams { ceiling_db: -1.0, lookahead_ms: 5.0, release_ms: 150.0 }
    }
}

/// What to apply while exporting or listening.
#[derive(Clone, Default)]
pub struct LevelPlan {
    pub curve: Option<Arc<GainCurve>>,
    pub limiter: Option<LimiterParams>,
}

// ---------------------------------------------------------------------------
// Analysis
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct Biquad {
    b: [f64; 3],
    a: [f64; 2],
    z: [f64; 2],
}

impl Biquad {
    fn new(b: [f64; 3], a0: f64, a1: f64, a2: f64) -> Biquad {
        Biquad { b: [b[0] / a0, b[1] / a0, b[2] / a0], a: [a1 / a0, a2 / a0], z: [0.0; 2] }
    }
    #[inline]
    fn run(&mut self, x: f64) -> f64 {
        // transposed direct form II
        let y = self.b[0] * x + self.z[0];
        self.z[0] = self.b[1] * x - self.a[0] * y + self.z[1];
        self.z[1] = self.b[2] * x - self.a[1] * y;
        y
    }
}

/// The two K-weighting filters of BS.1770 for any sample rate.
fn k_weighting(rate: u32) -> [Biquad; 2] {
    let fs = rate as f64;
    // stage 1: high shelf (head effects)
    let (g, q, fc) = (3.999_843_853_973_347, 0.707_175_236_955_419_3, 1_681.974_450_955_531_9);
    let a = 10f64.powf(g / 40.0);
    let w0 = 2.0 * std::f64::consts::PI * fc / fs;
    let (cw, alpha) = (w0.cos(), w0.sin() / (2.0 * q));
    let sa = a.sqrt();
    let shelf = Biquad::new(
        [
            a * ((a + 1.0) + (a - 1.0) * cw + 2.0 * sa * alpha),
            -2.0 * a * ((a - 1.0) + (a + 1.0) * cw),
            a * ((a + 1.0) + (a - 1.0) * cw - 2.0 * sa * alpha),
        ],
        (a + 1.0) - (a - 1.0) * cw + 2.0 * sa * alpha,
        2.0 * ((a - 1.0) - (a + 1.0) * cw),
        (a + 1.0) - (a - 1.0) * cw - 2.0 * sa * alpha,
    );
    // stage 2: high pass (RLB)
    let (q, fc) = (0.500_327_037_323_877_3, 38.135_470_876_024_44);
    let w0 = 2.0 * std::f64::consts::PI * fc / fs;
    let (cw, alpha) = (w0.cos(), w0.sin() / (2.0 * q));
    let hp = Biquad::new([(1.0 + cw) / 2.0, -(1.0 + cw), (1.0 + cw) / 2.0], 1.0 + alpha, -2.0 * cw, 1.0 - alpha);
    [shelf, hp]
}

/// Loudness measurement of a whole file: K-weighted mean square per 100 ms block.
#[derive(Clone)]
pub struct Analysis {
    pub rate: u32,
    pub block_frames: usize,
    /// Sum over channels of the mean square, per block.
    pub ms: Vec<f32>,
}

/// Measures from any source of interleaved float samples.
pub struct Analyzer {
    filters: Vec<[Biquad; 2]>,
    ch: usize,
    block_frames: usize,
    acc: f64,
    n: usize,
    out: Analysis,
}

impl Analyzer {
    pub fn new(rate: u32, channels: usize) -> Analyzer {
        let ch = channels.max(1);
        let block_frames = (rate as usize * BLOCK_MS as usize / 1000).max(1);
        Analyzer {
            filters: (0..ch).map(|_| k_weighting(rate)).collect(),
            ch,
            block_frames,
            acc: 0.0,
            n: 0,
            out: Analysis { rate, block_frames, ms: Vec::new() },
        }
    }

    pub fn push(&mut self, interleaved: &[f32]) {
        for frame in interleaved.chunks_exact(self.ch) {
            for (c, &x) in frame.iter().enumerate() {
                let f = &mut self.filters[c];
                let s1 = f[0].run(x as f64);
                let y = f[1].run(s1);
                self.acc += y * y;
            }
            self.n += 1;
            if self.n == self.block_frames {
                self.out.ms.push((self.acc / self.n as f64) as f32);
                self.acc = 0.0;
                self.n = 0;
            }
        }
    }

    pub fn finish(mut self) -> Analysis {
        if self.n > 0 {
            self.out.ms.push((self.acc / self.n as f64) as f32);
        }
        self.out
    }
}

/// Measure a whole WAV file. `progress` receives permille.
pub fn analyze(r: &mut WavReader, progress: &AtomicU32, cancel: &AtomicBool) -> Result<Analysis, String> {
    let ch = r.fmt.channels as usize;
    let mut an = Analyzer::new(r.fmt.sample_rate, ch);
    let total = r.frames;
    let mut pos = 0u64;
    let mut buf = Vec::new();
    while pos < total {
        if cancel.load(Ordering::Relaxed) {
            return Err("Geannuleerd".into());
        }
        let n = r.read_f32(pos, ((total - pos) as usize).min(1 << 16), &mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        an.push(&buf[..n * ch]);
        pos += n as u64;
        progress.store((pos * 1000 / total.max(1)) as u32, Ordering::Relaxed);
    }
    Ok(an.finish())
}

fn lufs(ms: f64) -> f32 {
    (-0.691 + 10.0 * (ms.max(1e-12)).log10()) as f32
}

impl Analysis {
    /// Short-term loudness (3 s window, centred) per block, in LUFS.
    pub fn short_term(&self) -> Vec<f32> {
        let n = self.ms.len();
        let mut pre = vec![0f64; n + 1];
        for (i, &m) in self.ms.iter().enumerate() {
            pre[i + 1] = pre[i] + m as f64;
        }
        let half = WINDOW_BLOCKS / 2;
        (0..n)
            .map(|i| {
                let a = i.saturating_sub(half);
                let b = (i + half).min(n);
                lufs((pre[b] - pre[a]) / (b - a).max(1) as f64)
            })
            .collect()
    }

    /// Integrated loudness (gated, BS.1770-4) of the whole file.
    pub fn integrated(&self) -> f32 {
        // 400 ms blocks with 75 % overlap = 4 of our 100 ms blocks, step 1
        let n = self.ms.len();
        if n < 4 {
            return lufs(self.ms.iter().map(|&x| x as f64).sum::<f64>() / n.max(1) as f64);
        }
        let blocks: Vec<f64> = (0..=n - 4).map(|i| self.ms[i..i + 4].iter().map(|&x| x as f64).sum::<f64>() / 4.0).collect();
        let abs: Vec<f64> = blocks.iter().copied().filter(|&m| lufs(m) > -70.0).collect();
        if abs.is_empty() {
            return -70.0;
        }
        let rel = lufs(abs.iter().sum::<f64>() / abs.len() as f64) - 10.0;
        let gated: Vec<f64> = abs.iter().copied().filter(|&m| lufs(m) > rel).collect();
        lufs(gated.iter().sum::<f64>() / gated.len().max(1) as f64)
    }
}

// ---------------------------------------------------------------------------
// Gain curve
// ---------------------------------------------------------------------------

/// Gain per 100 ms block of the source file.
pub struct GainCurve {
    pub block_frames: usize,
    /// Gain in dB per block (for drawing).
    pub db: Vec<f32>,
    lin: Vec<f32>,
}

impl GainCurve {
    pub fn compute(an: &Analysis, p: &LevelParams) -> GainCurve {
        let n = an.ms.len();
        // Momentary loudness (400 ms, centred) decides what is programme.
        let mut pre = vec![0f64; n + 1];
        for (i, &m) in an.ms.iter().enumerate() {
            pre[i + 1] = pre[i] + m as f64;
        }
        let momentary: Vec<f32> = (0..n)
            .map(|i| {
                let (a, b) = (i.saturating_sub(2), (i + 2).min(n));
                lufs((pre[b] - pre[a]) / (b - a).max(1) as f64)
            })
            .collect();
        // Loudness measured over programme blocks only, so silence and
        // fade-outs don't make the music look softer than it is.
        let gated = |active: &[bool], half: usize| -> Vec<Option<f32>> {
            let mut sum = vec![0f64; n + 1];
            let mut cnt = vec![0usize; n + 1];
            for i in 0..n {
                sum[i + 1] = sum[i] + if active[i] { an.ms[i] as f64 } else { 0.0 };
                cnt[i + 1] = cnt[i] + active[i] as usize;
            }
            (0..n)
                .map(|i| {
                    let (a, b) = (i.saturating_sub(half), (i + half).min(n));
                    let c = cnt[b] - cnt[a];
                    (active[i] && c > 0).then(|| lufs((sum[b] - sum[a]) / c as f64))
                })
                .collect()
        };
        // 1. silence and tape hiss (absolute gate) don't count
        let active: Vec<bool> = momentary.iter().map(|&m| m >= p.gate_lufs).collect();
        let st = gated(&active, WINDOW_BLOCKS / 2);
        // 2. level = the loudest short-term loudness within the window before and
        //    after: quiet passages and fade-outs shorter than the window keep
        //    their dynamics; only longer differences (between songs, tapes,
        //    recordings) are evened out.
        let half = ((p.speed.window() * 1000.0 / BLOCK_MS as f32) as usize).max(1);
        let mut env: Vec<Option<f32>> = vec![None; n];
        let mut dq: VecDeque<(usize, f32)> = VecDeque::new();
        let mut next_in = 0usize;
        for (i, e) in env.iter_mut().enumerate() {
            while next_in < n && next_in <= i + half {
                if let Some(v) = st[next_in] {
                    while dq.back().is_some_and(|&(_, b)| b <= v) {
                        dq.pop_back();
                    }
                    dq.push_back((next_in, v));
                }
                next_in += 1;
            }
            while dq.front().is_some_and(|&(j, _)| j + half < i) {
                dq.pop_front();
            }
            if active[i] {
                *e = dq.front().map(|&(_, v)| v);
            }
        }
        let mut want: Vec<Option<f32>> =
            env.iter().map(|l| l.map(|l| (p.target_lufs - l).clamp(-p.max_cut_db.abs(), p.max_boost_db.max(0.0)))).collect();
        // 2. silence: the lower of the gains before and after (never boost the gap)
        let mut prev: Vec<Option<f32>> = vec![None; n];
        let mut last = None;
        for i in 0..n {
            if want[i].is_some() {
                last = want[i];
            }
            prev[i] = last;
        }
        let mut next = None;
        for i in (0..n).rev() {
            if want[i].is_some() {
                next = want[i];
            } else {
                want[i] = Some(match (prev[i], next) {
                    (Some(a), Some(b)) => a.min(b),
                    (Some(a), None) => a,
                    (None, Some(b)) => b,
                    (None, None) => 0.0,
                });
            }
        }
        let mut db: Vec<f32> = want.into_iter().map(|g| g.unwrap_or(0.0)).collect();
        // 3. smooth forwards and backwards (zero phase: looks ahead and behind)
        if n > 1 {
            let k = 1.0 - (-(BLOCK_MS as f32 / 1000.0) / p.speed.seconds()).exp();
            for i in 1..n {
                db[i] = db[i - 1] + k * (db[i] - db[i - 1]);
            }
            for i in (0..n - 1).rev() {
                db[i] = db[i + 1] + k * (db[i] - db[i + 1]);
            }
        }
        let lin = db.iter().map(|&d| 10f32.powf(d / 20.0)).collect();
        GainCurve { block_frames: an.block_frames, db, lin }
    }

    /// Linear gain at a source frame (interpolated between block centres).
    #[inline]
    pub fn gain_at(&self, frame: u64) -> f32 {
        let n = self.lin.len();
        if n == 0 {
            return 1.0;
        }
        let x = frame as f64 / self.block_frames as f64 - 0.5;
        if x <= 0.0 {
            return self.lin[0];
        }
        let i = x.floor() as usize;
        if i + 1 >= n {
            return self.lin[n - 1];
        }
        let t = (x - i as f64) as f32;
        self.lin[i] + (self.lin[i + 1] - self.lin[i]) * t
    }

    /// Gain in dB at a source frame (for drawing).
    pub fn db_at(&self, frame: u64) -> f32 {
        20.0 * self.gain_at(frame).max(1e-6).log10()
    }

    /// Apply to integer samples read from source frame `start` (may exceed full scale; the limiter follows).
    pub fn apply_i32(&self, start: u64, buf: &mut [i32], ch: usize) {
        for (i, f) in buf.chunks_exact_mut(ch).enumerate() {
            let g = self.gain_at(start + i as u64) as f64;
            for s in f.iter_mut() {
                *s = (*s as f64 * g).round().clamp(i32::MIN as f64, i32::MAX as f64) as i32;
            }
        }
    }

    pub fn apply_f32(&self, start: u64, buf: &mut [f32], ch: usize) {
        for (i, f) in buf.chunks_exact_mut(ch).enumerate() {
            let g = self.gain_at(start + i as u64);
            for s in f.iter_mut() {
                *s *= g;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Look-ahead peak limiter
// ---------------------------------------------------------------------------

/// Sees each peak `lookahead` ahead and lowers the gain smoothly before it, so
/// the output never exceeds the ceiling. Output = input delayed by
/// lookahead − 1 frame; `process` + `flush` return exactly as many frames as went in.
pub struct Limiter {
    ch: usize,
    l: usize,
    ceiling: f32,
    release: f32,
    /// delayed input frames (ring, `l` frames)
    delay: Vec<f32>,
    /// required gain per frame, sliding minimum over `l` frames
    mins: VecDeque<(u64, f32)>,
    /// last `l` minimum values, for the moving average
    box_ring: Vec<f64>,
    box_sum: f64,
    /// frames pushed into the delay line (including flush padding)
    n: u64,
    gain: f32,
    /// real input frames, and frames handed out
    input_n: u64,
    emitted: u64,
}

impl Limiter {
    pub fn new(rate: u32, channels: usize, p: &LimiterParams) -> Limiter {
        let ch = channels.max(1);
        let l = ((p.lookahead_ms.max(0.5) * rate as f32 / 1000.0).round() as usize).max(2);
        let rel_frames = (p.release_ms.max(1.0) * rate as f32 / 1000.0).max(1.0);
        Limiter {
            ch,
            l,
            ceiling: 10f32.powf(p.ceiling_db.min(0.0) / 20.0),
            release: 1.0 - (-1.0 / rel_frames).exp(),
            delay: vec![0.0; l * ch],
            mins: VecDeque::new(),
            box_ring: vec![1.0; l],
            box_sum: l as f64,
            n: 0,
            gain: 1.0,
            input_n: 0,
            emitted: 0,
        }
    }

    /// Delay in frames.
    pub fn latency(&self) -> usize {
        self.l - 1
    }

    fn step(&mut self, frame: &[f32], out: &mut Vec<f32>) {
        let (ch, l) = (self.ch, self.l);
        let peak = frame.iter().fold(0f32, |m, &x| m.max(x.abs()));
        let need = if peak > self.ceiling { self.ceiling / peak } else { 1.0 };
        let n = self.n;
        // sliding minimum of `need` over the last l frames
        while self.mins.back().is_some_and(|&(_, v)| v >= need) {
            self.mins.pop_back();
        }
        self.mins.push_back((n, need));
        while self.mins.front().is_some_and(|&(i, _)| i + l as u64 <= n) {
            self.mins.pop_front();
        }
        let m = self.mins.front().map(|&(_, v)| v).unwrap_or(1.0) as f64;
        // moving average of the minimum over l frames: a smooth ramp that is
        // always at or below what the frame leaving the delay line needs
        let slot = (n % l as u64) as usize;
        self.box_sum += m - self.box_ring[slot];
        self.box_ring[slot] = m;
        if n % 65_536 == 0 {
            self.box_sum = self.box_ring.iter().sum();
        }
        let s = (self.box_sum / l as f64).min(1.0) as f32;
        self.gain = if s < self.gain { s } else { self.gain + (s - self.gain) * self.release };
        // delay line: frame n in, frame n − (l−1) out
        self.delay[slot * ch..slot * ch + ch].copy_from_slice(frame);
        self.n += 1;
        if self.n < l as u64 {
            return; // delay line still filling
        }
        self.emitted += 1;
        let o = (self.n % l as u64) as usize;
        let g = self.gain;
        out.extend(self.delay[o * ch..o * ch + ch].iter().map(|&x| x * g));
    }

    pub fn process(&mut self, interleaved: &[f32]) -> Vec<f32> {
        let mut out = Vec::with_capacity(interleaved.len());
        let ch = self.ch;
        for f in interleaved.chunks_exact(ch) {
            self.input_n += 1;
            self.step(f, &mut out);
        }
        out
    }

    /// The last frames still in the delay line.
    pub fn flush(&mut self) -> Vec<f32> {
        let mut out = Vec::new();
        let zero = vec![0.0; self.ch];
        while self.emitted < self.input_n {
            self.step(&zero, &mut out);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(rate: u32, secs: f32, freq: f32, amp: f32, ch: usize) -> Vec<f32> {
        let n = (rate as f32 * secs) as usize;
        let mut v = Vec::with_capacity(n * ch);
        for i in 0..n {
            let s = amp * (2.0 * std::f32::consts::PI * freq * i as f32 / rate as f32).sin();
            for _ in 0..ch {
                v.push(s);
            }
        }
        v
    }

    fn analysis_of(parts: &[(f32, f32)], rate: u32) -> Analysis {
        let mut an = Analyzer::new(rate, 2);
        for &(secs, amp) in parts {
            an.push(&sine(rate, secs, 997.0, amp, 2));
        }
        an.finish()
    }

    #[test]
    fn loudness_of_reference_sine() {
        // 1 kHz sine, −20 dBFS peak, in both channels → about −20 LUFS (BS.1770 calibration)
        for rate in [44_100, 48_000] {
            let an = analysis_of(&[(10.0, 0.1)], rate);
            let i = an.integrated();
            assert!((i - (-20.0)).abs() < 0.3, "{rate}: {i}");
        }
    }

    #[test]
    fn curve_lifts_quiet_and_lowers_loud() {
        let p = LevelParams::default(); // target −16
        // 60 s at about −30 LUFS, then 60 s at about −10 LUFS
        let an = analysis_of(&[(60.0, 0.0316), (60.0, 0.316)], 48_000);
        let c = GainCurve::compute(&an, &p);
        let at = |s: f32| c.db_at((s * 48_000.0) as u64);
        assert!((at(20.0) - 12.0).abs() < 0.3, "quiet part capped at +12: {}", at(20.0));
        assert!((at(100.0) - (-6.0)).abs() < 0.3, "loud part −6: {}", at(100.0));
        // smooth: no jumps between 100 ms blocks
        for w in c.db.windows(2) {
            assert!((w[1] - w[0]).abs() < 0.5);
        }
        // look-ahead: the gain already goes down before the loud part starts
        assert!(at(59.0) < at(40.0) - 3.0);
    }

    #[test]
    fn silence_and_hiss_are_not_boosted() {
        let p = LevelParams::default();
        // programme at −20 LUFS (gain +4), 20 s of "hiss" at −60, programme again
        let an = analysis_of(&[(20.0, 0.1), (20.0, 0.001), (20.0, 0.1)], 48_000);
        let c = GainCurve::compute(&an, &p);
        let music = c.db_at(10 * 48_000);
        assert!((music - 4.0).abs() < 0.7, "music gain {music}");
        // the gap gets no more gain than the music around it (the hiss stays 40 dB down)
        let max_gap = (20..40).map(|s| c.db_at(s as u64 * 48_000)).fold(f32::MIN, f32::max);
        assert!(max_gap <= music + 0.1, "gap boosted to {max_gap} dB, music {music} dB");
    }

    #[test]
    fn fade_out_tail_is_not_lifted() {
        let p = LevelParams::default();
        let rate = 48_000;
        // 20 s music at −20 LUFS, then a 10 s fade to silence, then 10 s silence
        let mut an = Analyzer::new(rate, 2);
        an.push(&sine(rate, 20.0, 997.0, 0.1, 2));
        let fade = sine(rate, 10.0, 997.0, 0.1, 2);
        let n = fade.len() / 2;
        let faded: Vec<f32> = fade.iter().enumerate().map(|(i, &x)| x * (1.0 - (i / 2) as f32 / n as f32).powi(3)).collect();
        an.push(&faded);
        an.push(&vec![0.0; rate as usize * 20]);
        let c = GainCurve::compute(&an.finish(), &p);
        let music = c.db_at(10 * rate as u64);
        let tail = (20..40).map(|s| c.db_at(s as u64 * rate as u64)).fold(f32::MIN, f32::max);
        assert!(tail <= music + 0.5, "fade-out lifted to {tail} dB, music {music} dB");
    }

    #[test]
    fn limiter_keeps_peaks_under_ceiling_and_length() {
        let rate = 48_000;
        let p = LimiterParams::default();
        let ceiling = 10f32.powf(p.ceiling_db / 20.0);
        // music-like: sine with sudden loud bursts up to +6 dBFS
        let mut input = Vec::new();
        let mut seed = 7u32;
        for i in 0..rate * 4 {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let burst = if (i / 4000) % 7 == 3 { 2.0 } else { 0.5 };
            let x = burst * (i as f32 * 0.05).sin() + ((seed >> 9) as f32 / 8_388_608.0 - 1.0) * 0.05;
            input.push(x);
            input.push(-x * 0.8);
        }
        let mut lim = Limiter::new(rate, 2, &p);
        let mut out = Vec::new();
        for chunk in input.chunks(2 * 1000 + 6) {
            out.extend(lim.process(chunk));
        }
        out.extend(lim.flush());
        assert_eq!(out.len(), input.len(), "same number of frames out as in");
        let peak = out.iter().fold(0f32, |m, &x| m.max(x.abs()));
        assert!(peak <= ceiling + 1e-6, "peak {peak} > ceiling {ceiling}");
    }

    #[test]
    fn limiter_leaves_quiet_audio_alone() {
        let rate = 44_100;
        let input = sine(rate, 1.0, 440.0, 0.5, 1);
        let mut lim = Limiter::new(rate, 1, &LimiterParams::default());
        let mut out = lim.process(&input);
        out.extend(lim.flush());
        assert_eq!(out.len(), input.len());
        // very short input: everything still comes out
        let mut l2 = Limiter::new(rate, 1, &LimiterParams::default());
        let mut o2 = l2.process(&[0.1, 0.2, 0.3]);
        o2.extend(l2.flush());
        assert_eq!(o2, vec![0.1, 0.2, 0.3]);
        for (a, b) in input.iter().zip(&out) {
            assert!((a - b).abs() < 1e-6);
        }
    }
}
