//! Compact min/max summary of audio for drawing waveforms of any length.

use crate::wav::WavReader;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

/// Peaks at two resolutions: fine bins of `bin_frames` frames and coarse bins
/// of `COARSE` fine bins each, so hours of audio draw quickly.
#[derive(Clone)]
pub struct Peaks {
    pub channels: usize,
    pub bin_frames: usize,
    /// idx = bin * channels + ch, value = (min, max)
    fine: Vec<(f32, f32)>,
    coarse: Vec<(f32, f32)>,
    // accumulation of the bin currently being filled
    acc: Vec<(f32, f32)>,
    acc_frames: usize,
    total_frames: u64,
}

pub const COARSE: usize = 100;

impl Peaks {
    pub fn new(channels: usize, sample_rate: u32) -> Self {
        let bin_frames = (sample_rate as usize / 100).max(1); // 10 ms
        Peaks {
            channels: channels.max(1),
            bin_frames,
            fine: Vec::new(),
            coarse: Vec::new(),
            acc: vec![(f32::MAX, f32::MIN); channels.max(1)],
            acc_frames: 0,
            total_frames: 0,
        }
    }

    pub fn total_frames(&self) -> u64 {
        self.total_frames
    }

    pub fn bins(&self) -> usize {
        self.fine.len() / self.channels
    }

    /// Add interleaved samples.
    pub fn push(&mut self, interleaved: &[f32]) {
        let ch = self.channels;
        for frame in interleaved.chunks_exact(ch) {
            for (c, &s) in frame.iter().enumerate() {
                let a = &mut self.acc[c];
                if s < a.0 {
                    a.0 = s;
                }
                if s > a.1 {
                    a.1 = s;
                }
            }
            self.acc_frames += 1;
            if self.acc_frames == self.bin_frames {
                self.close_bin();
            }
        }
        self.total_frames += (interleaved.len() / ch) as u64;
    }

    fn close_bin(&mut self) {
        let ch = self.channels;
        for c in 0..ch {
            self.fine.push(self.acc[c]);
            self.acc[c] = (f32::MAX, f32::MIN);
        }
        self.acc_frames = 0;
        let bins = self.bins();
        if bins % COARSE == 0 {
            let start = bins - COARSE;
            for c in 0..ch {
                let mut mm = (f32::MAX, f32::MIN);
                for b in start..bins {
                    let v = self.fine[b * ch + c];
                    mm.0 = mm.0.min(v.0);
                    mm.1 = mm.1.max(v.1);
                }
                self.coarse.push(mm);
            }
        }
    }

    /// Min/max of channel `ch` over the frame range [a, b).
    pub fn range(&self, ch: usize, a: u64, b: u64) -> Option<(f32, f32)> {
        let nch = self.channels;
        let ch = ch.min(nch - 1);
        let bf = self.bin_frames as u64;
        let bins = self.bins() as u64;
        let mut ba = a / bf;
        let be = b.div_ceil(bf).min(bins);
        if ba >= be {
            // Inside the bin still being filled (live view).
            if a / bf == bins && self.acc_frames > 0 {
                let v = self.acc[ch];
                return Some(v);
            }
            return None;
        }
        let mut mm = (f32::MAX, f32::MIN);
        let c = COARSE as u64;
        while ba < be {
            if ba % c == 0 && ba + c <= be && ((ba / c) as usize) < self.coarse.len() / nch {
                let v = self.coarse[(ba / c) as usize * nch + ch];
                mm.0 = mm.0.min(v.0);
                mm.1 = mm.1.max(v.1);
                ba += c;
            } else {
                let v = self.fine[ba as usize * nch + ch];
                mm.0 = mm.0.min(v.0);
                mm.1 = mm.1.max(v.1);
                ba += 1;
            }
        }
        Some(mm)
    }

    /// Scan a whole file. `progress` receives permille, `cancel` aborts.
    pub fn from_reader(r: &mut WavReader, progress: &AtomicU32, cancel: &AtomicBool) -> std::io::Result<Peaks> {
        let mut p = Peaks::new(r.fmt.channels as usize, r.fmt.sample_rate);
        let chunk = 65536;
        let mut pos = 0u64;
        let mut buf = Vec::new();
        while pos < r.frames {
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            let n = r.read_f32(pos, chunk, &mut buf)?;
            if n == 0 {
                break;
            }
            p.push(&buf);
            pos += n as u64;
            progress.store((pos * 1000 / r.frames.max(1)) as u32, Ordering::Relaxed);
        }
        Ok(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coarse_matches_fine() {
        let mut p = Peaks::new(2, 48000);
        let mut v = Vec::new();
        for i in 0..(480 * 250) {
            let s = ((i as f32) * 0.001).sin() * if i == 70_000 { 1.0 } else { 0.5 };
            v.push(s);
            v.push(-s * 0.5);
        }
        p.push(&v);
        assert_eq!(p.bins(), 250);
        let (mn, mx) = p.range(0, 0, 480 * 250).unwrap();
        assert!((mx - 1.0).abs() < 1e-6 || (mn + 1.0).abs() < 1e-6 || mx >= 0.5);
        // Same answer with and without coarse levels
        let direct = (0..250u64).fold((f32::MAX, f32::MIN), |acc, b| {
            let v = p.range(1, b * 480, (b + 1) * 480).unwrap();
            (acc.0.min(v.0), acc.1.max(v.1))
        });
        assert_eq!(p.range(1, 0, 480 * 250).unwrap(), direct);
    }
}
