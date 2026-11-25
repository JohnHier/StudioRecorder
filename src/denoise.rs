//! Voice isolation with DeepFilterNet3 (ArchiefRecorder): removes tape hiss,
//! hum and crackle from recorded interviews and keeps the voice.
//!
//! The model is built into the program (crate `deep-filter-rt`, pure Rust,
//! no download). It works at 48 kHz; other sample rates are converted to
//! 48 kHz and back. The result is a cleaned working copy in the temp folder,
//! exactly as long as the source and time-aligned with it, so IN/UIT points
//! and cuts stay where they are. The original is never changed.

use crate::peaks::Peaks;
use crate::wav::{WavReader, WavWriter};
use deep_filter_rt::tract::{DfParams, DfTract, RuntimeParams};
use ndarray::Array2;
use rubato::{FftFixedInOut, Resampler};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

/// The model's sample rate.
const DF_RATE: u32 = 48_000;

/// How strongly the noise is removed: the most the model may attenuate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Strength {
    Light,
    Medium,
    Strong,
}

impl Strength {
    pub fn atten_db(self) -> f32 {
        match self {
            Strength::Light => 12.0,
            Strength::Medium => 24.0,
            Strength::Strong => 100.0, // no limit
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Strength::Light => "Licht",
            Strength::Medium => "Middel",
            Strength::Strong => "Sterk",
        }
    }
    pub fn hint(self) -> &'static str {
        match self {
            Strength::Light => "Ruis 12 dB zachter; klinkt het meest natuurlijk",
            Strength::Medium => "Ruis 24 dB zachter",
            Strength::Strong => "Ruis helemaal weg (zoals DaVinci Voice Isolation); kan wat 'droog' klinken",
        }
    }
}

/// Where the cleaned copy of `src` goes (one per source file and strength).
pub fn iso_path(src: &Path, strength: Strength) -> PathBuf {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    src.hash(&mut h);
    if let Ok(m) = std::fs::metadata(src) {
        m.len().hash(&mut h);
        m.modified().ok().hash(&mut h);
    }
    let stem = src.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let stem: String = stem.chars().take(40).collect();
    crate::decode::cache_dir().join(format!("{stem}-{:016x}-stem-{:?}.wav", h.finish(), strength).to_lowercase())
}

/// One processing step on interleaved samples.
trait Stage {
    fn push(&mut self, input: &[f32], out: &mut Vec<f32>) -> Result<(), String>;
}

/// Sample-rate conversion in fixed chunks.
struct Resample {
    r: FftFixedInOut<f32>,
    ch: usize,
    buf: Vec<f32>,
    planar_in: Vec<Vec<f32>>,
    planar_out: Vec<Vec<f32>>,
}

impl Resample {
    fn new(from: u32, to: u32, ch: usize) -> Result<Resample, String> {
        let r = FftFixedInOut::<f32>::new(from as usize, to as usize, 1024, ch).map_err(|e| e.to_string())?;
        let (ni, no) = (r.input_frames_next(), r.output_frames_next());
        Ok(Resample { r, ch, buf: Vec::new(), planar_in: vec![vec![0.0; ni]; ch], planar_out: vec![vec![0.0; no]; ch] })
    }
    /// Delay in output frames.
    fn delay(&self) -> usize {
        self.r.output_delay()
    }
}

impl Stage for Resample {
    fn push(&mut self, input: &[f32], out: &mut Vec<f32>) -> Result<(), String> {
        self.buf.extend_from_slice(input);
        let ni = self.r.input_frames_next();
        let ch = self.ch;
        let mut used = 0;
        while (self.buf.len() - used) / ch >= ni {
            for (i, f) in self.buf[used..used + ni * ch].chunks_exact(ch).enumerate() {
                for c in 0..ch {
                    self.planar_in[c][i] = f[c];
                }
            }
            let (_, no) = self.r.process_into_buffer(&self.planar_in, &mut self.planar_out, None).map_err(|e| e.to_string())?;
            for i in 0..no {
                for c in 0..ch {
                    out.push(self.planar_out[c][i]);
                }
            }
            used += ni * ch;
        }
        self.buf.drain(..used);
        Ok(())
    }
}

/// DeepFilterNet3 in frames of 10 ms.
struct Df {
    df: DfTract,
    ch: usize,
    buf: Vec<f32>,
    inp: Array2<f32>,
    enh: Array2<f32>,
}

impl Df {
    fn new(ch: usize, strength: Strength) -> Result<Df, String> {
        let rp = RuntimeParams::default_with_ch(ch).with_atten_lim(strength.atten_db());
        let df = DfTract::new(DfParams::default(), &rp).map_err(|e| format!("Stemisolatie kan niet starten: {e}"))?;
        let hop = df.hop_size;
        Ok(Df { df, ch, buf: Vec::new(), inp: Array2::zeros((ch, hop)), enh: Array2::zeros((ch, hop)) })
    }
}

impl Stage for Df {
    fn push(&mut self, input: &[f32], out: &mut Vec<f32>) -> Result<(), String> {
        self.buf.extend_from_slice(input);
        let (ch, hop) = (self.ch, self.df.hop_size);
        let mut used = 0;
        while (self.buf.len() - used) / ch >= hop {
            for (i, f) in self.buf[used..used + hop * ch].chunks_exact(ch).enumerate() {
                for c in 0..ch {
                    self.inp[[c, i]] = f[c];
                }
            }
            self.df.process(self.inp.view(), self.enh.view_mut()).map_err(|e| format!("Stemisolatie: {e}"))?;
            for i in 0..hop {
                for c in 0..ch {
                    let v = self.enh[[c, i]];
                    out.push(if v.is_finite() { v } else { 0.0 });
                }
            }
            used += hop * ch;
        }
        self.buf.drain(..used);
        Ok(())
    }
}

/// Clean `src` into `dst` (WAV, 24-bit, same rate, channels and length).
/// Returns the waveform overview of the result. `progress` receives permille.
pub fn render(src: &Path, dst: &Path, strength: Strength, progress: &AtomicU32, cancel: &AtomicBool) -> Result<Peaks, String> {
    let mut r = WavReader::open(src).map_err(|e| format!("Kan bestand niet openen: {e}"))?;
    let (rate, ch, total) = (r.fmt.sample_rate, r.fmt.channels as usize, r.frames);
    if !(1..=2).contains(&ch) {
        return Err("Stemisolatie werkt met mono- en stereobestanden".into());
    }
    // processing chain, with its delay in output frames
    let mut stages: Vec<Box<dyn Stage>> = Vec::new();
    let df = Df::new(ch, strength)?;
    let df_delay = df.df.delay() as f64;
    let delay = if rate == DF_RATE {
        stages.push(Box::new(df));
        df_delay
    } else {
        let up = Resample::new(rate, DF_RATE, ch)?;
        let down = Resample::new(DF_RATE, rate, ch)?;
        let d = (up.delay() as f64 + df_delay) * rate as f64 / DF_RATE as f64 + down.delay() as f64;
        stages.push(Box::new(up));
        stages.push(Box::new(df));
        stages.push(Box::new(down));
        d
    };
    let mut skip = (delay.round() as usize) * ch;

    if let Some(d) = dst.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    let part = dst.with_extension("part");
    let mut w = WavWriter::create(&part, rate, ch as u16, 24).map_err(|e| format!("Kan werkkopie niet maken: {e}"))?;
    let mut peaks = Peaks::new(ch, rate);
    let mut written = 0u64;
    let mut pos = 0u64;
    let mut inbuf = Vec::new();
    let zeros = vec![0f32; 4800 * ch];
    let result = (|| -> Result<(), String> {
        while written < total {
            if cancel.load(Ordering::Relaxed) {
                return Err("Geannuleerd".into());
            }
            // next block of source (or silence to flush the delay at the end)
            let block: &[f32] = if pos < total {
                let n = r.read_f32(pos, ((total - pos) as usize).min(48_000), &mut inbuf).map_err(|e| e.to_string())?;
                if n == 0 {
                    pos = total;
                    &zeros
                } else {
                    pos += n as u64;
                    &inbuf[..n * ch]
                }
            } else {
                &zeros
            };
            let mut data = block.to_vec();
            for s in stages.iter_mut() {
                let mut out = Vec::with_capacity(data.len() + 4096);
                s.push(&data, &mut out)?;
                data = out;
            }
            let start = skip.min(data.len());
            skip -= start;
            let room = ((total - written) as usize) * ch;
            let take = &data[start..data.len().min(start + room)];
            if !take.is_empty() {
                w.write_f32(take).map_err(|e| e.to_string())?;
                peaks.push(take);
                written += (take.len() / ch) as u64;
            }
            progress.store((written * 1000 / total.max(1)) as u32, Ordering::Relaxed);
        }
        Ok(())
    })();
    match result {
        Ok(()) => {
            w.finalize().map_err(|e| e.to_string())?;
            std::fs::rename(&part, dst).map_err(|e| e.to_string())?;
            Ok(peaks)
        }
        Err(e) => {
            drop(w);
            let _ = std::fs::remove_file(&part);
            Err(e)
        }
    }
}

/// The cleaned copy, made earlier or now.
pub fn cleaned(src: &Path, strength: Strength, progress: &AtomicU32, cancel: &AtomicBool) -> Result<(PathBuf, Peaks), String> {
    let dst = iso_path(src, strength);
    if dst.exists() {
        if let Ok(mut rd) = WavReader::open(&dst) {
            if let Ok(src_r) = WavReader::open(src) {
                if rd.frames == src_r.frames {
                    if let Ok(p) = Peaks::from_reader(&mut rd, progress, cancel) {
                        return Ok((dst, p));
                    }
                }
            }
        }
    }
    let p = render(src, &dst, strength, progress, cancel)?;
    Ok((dst, p))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join("studiorec_denoise_tests");
        std::fs::create_dir_all(&d).unwrap();
        d.join(name)
    }

    /// "Voice": a gliding tone with pauses; "tape": hiss over everything.
    fn make(name: &str, rate: u32, ch: usize, secs: f32) -> PathBuf {
        let p = tmp(name);
        let mut w = WavWriter::create(&p, rate, ch as u16, 24).unwrap();
        let n = (rate as f32 * secs) as usize;
        let mut v = Vec::with_capacity(n * ch);
        let mut seed = 11u32;
        let mut phase = 0f32;
        for i in 0..n {
            let t = i as f32 / rate as f32;
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let hiss = ((seed >> 8) as f32 / 16_777_216.0 - 0.5) * 0.04;
            let talking = (t % 2.0) < 1.2 && t > 1.0;
            let f0 = 140.0 + 40.0 * (t * 3.0).sin();
            phase += 2.0 * std::f32::consts::PI * f0 / rate as f32;
            // voice-like: harmonics of a gliding pitch with a syllable envelope
            let env = if talking { (t * 5.0 * std::f32::consts::PI).sin().abs() } else { 0.0 };
            let voice: f32 = (1..12).map(|k| (phase * k as f32).sin() / k as f32).sum::<f32>() * 0.15 * env;
            for _ in 0..ch {
                v.push(voice + hiss);
            }
        }
        w.write_f32(&v).unwrap();
        w.finalize().unwrap();
        p
    }

    fn rms(r: &mut WavReader, a: f32, b: f32) -> f32 {
        let rate = r.fmt.sample_rate as f32;
        let ch = r.fmt.channels as usize;
        let mut v = Vec::new();
        let (s, e) = ((a * rate) as u64, (b * rate) as u64);
        r.read_f32(s, (e - s) as usize, &mut v).unwrap();
        let ms = v.iter().map(|&x| (x as f64).powi(2)).sum::<f64>() / v.len().max(1) as f64;
        let _ = ch;
        (10.0 * ms.max(1e-20).log10()) as f32
    }

    fn check(rate: u32, ch: usize) {
        let src = make(&format!("voice_{rate}_{ch}.wav"), rate, ch, 6.0);
        let dst = tmp(&format!("voice_{rate}_{ch}_clean.wav"));
        render(&src, &dst, Strength::Strong, &AtomicU32::new(0), &AtomicBool::new(false)).unwrap();
        let mut a = WavReader::open(&src).unwrap();
        let mut b = WavReader::open(&dst).unwrap();
        assert_eq!(b.frames, a.frames, "same length");
        assert_eq!((b.fmt.sample_rate, b.fmt.channels), (rate, ch as u16));
        // the pause (only hiss) gets much quieter
        let (p0, p1) = (rms(&mut a, 0.2, 0.9), rms(&mut b, 0.2, 0.9));
        eprintln!("{rate} Hz {ch} ch: pause {p0:.1} -> {p1:.1} dB");
        assert!(p1 < p0 - 15.0, "hiss in the pause: {p0} -> {p1}");
        // time-aligned: the cleaned signal correlates best with the source at lag 0
        let mut x = Vec::new();
        let mut y = Vec::new();
        let (s, n) = ((1.0 * rate as f32) as u64, rate as usize);
        a.read_f32(s, n, &mut x).unwrap();
        b.read_f32(s, n, &mut y).unwrap();
        let corr = |lag: i64| -> f64 {
            (200..n - 200)
                .map(|i| x[i * ch] as f64 * y[((i as i64 + lag) as usize) * ch] as f64)
                .sum()
        };
        let best = (-48i64..=48).max_by(|&p, &q| corr(p).partial_cmp(&corr(q)).unwrap()).unwrap();
        assert!(best.abs() <= 2, "{rate} Hz: misaligned by {best} samples");
    }

    #[test]
    fn cleans_48k_stereo_aligned() {
        check(48_000, 2);
    }

    #[test]
    fn cleans_44k1_mono_aligned() {
        check(44_100, 1);
    }
}
