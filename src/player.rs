//! Playback of a list of ranges of a WAV file (skipping cut-out parts).

use crate::wav::WavReader;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use crossbeam_channel::{Receiver, Sender, bounded};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

pub struct Player {
    stream: Option<cpal::Stream>,
    stop: Arc<AtomicBool>,
    /// Current position in file frames (f64 bits).
    pos: Arc<AtomicU64>,
    finished: Arc<AtomicBool>,
    pub error: Option<String>,
}

impl Default for Player {
    fn default() -> Self {
        Player {
            stream: None,
            stop: Arc::new(AtomicBool::new(true)),
            pos: Arc::new(AtomicU64::new(0)),
            finished: Arc::new(AtomicBool::new(true)),
            error: None,
        }
    }
}

pub fn list_outputs() -> Vec<(String, String)> {
    let mut v = Vec::new();
    if let Ok(devs) = cpal::default_host().output_devices() {
        for d in devs {
            if let Ok(id) = d.id() {
                v.push((id.to_string(), crate::engine::device_name(&d)));
            }
        }
    }
    v
}

impl Player {
    pub fn is_playing(&self) -> bool {
        self.stream.is_some() && !self.finished.load(Ordering::Relaxed)
    }

    pub fn position(&self) -> u64 {
        f64::from_bits(self.pos.load(Ordering::Relaxed)) as u64
    }

    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.stream = None;
    }

    /// Housekeeping from the UI loop: release the device when playback ended.
    pub fn tick(&mut self) {
        if self.stream.is_some() && self.finished.load(Ordering::Relaxed) {
            self.stop();
        }
    }

    /// Play the given frame ranges of `path` one after another.
    pub fn play(&mut self, path: &Path, segments: &[(u64, u64)], output_id: Option<&str>) {
        self.stop();
        self.error = None;
        if let Err(e) = self.try_play(path, segments, output_id) {
            self.error = Some(e);
            self.finished.store(true, Ordering::Relaxed);
        }
    }

    fn try_play(&mut self, path: &Path, segments: &[(u64, u64)], output_id: Option<&str>) -> Result<(), String> {
        let mut reader = WavReader::open(path).map_err(|e| e.to_string())?;
        let file_rate = reader.fmt.sample_rate;
        let file_ch = reader.fmt.channels as usize;
        let segs: Vec<(u64, u64)> = segments
            .iter()
            .map(|&(a, b)| (a.min(reader.frames), b.min(reader.frames)))
            .filter(|(a, b)| a < b)
            .collect();
        if segs.is_empty() {
            return Err("Niets om af te spelen".into());
        }
        let host = cpal::default_host();
        let device = output_id
            .and_then(crate::engine::find_device)
            .or_else(|| host.default_output_device())
            .ok_or("Geen afspeelapparaat gevonden")?;
        let def = device.default_output_config().map_err(|e| e.to_string())?;
        let out_ch = def.channels() as usize;

        // Ask Windows to play at the file's rate (it converts); fall back to
        // the device rate with our own resampling.
        let attempts = [file_rate, def.sample_rate()];
        let stop = Arc::new(AtomicBool::new(false));
        let finished = Arc::new(AtomicBool::new(false));
        let pos = Arc::new(AtomicU64::new((segs[0].0 as f64).to_bits()));
        let mut last_err = String::new();
        for (i, &rate) in attempts.iter().enumerate() {
            if i == 1 && rate == file_rate {
                break;
            }
            let (tx, rx) = bounded::<(u64, Vec<f32>)>(16);
            let cfg = cpal::StreamConfig {
                channels: out_ch as u16,
                sample_rate: rate,
                buffer_size: cpal::BufferSize::Default,
            };
            let mut cb = Callback::new(rx, file_rate as f64 / rate as f64, file_ch, out_ch, pos.clone(), finished.clone());
            let res = device.build_output_stream::<f32, _, _>(cfg, move |out: &mut [f32], _| cb.fill(out), |_e| {}, None);
            match res {
                Ok(stream) => {
                    let stop_r = stop.clone();
                    let chunk = (file_rate as usize / 20).max(512);
                    let segs = segs.clone();
                    std::thread::spawn(move || feed(&mut reader, tx, &segs, chunk, stop_r));
                    stream.play().map_err(|e| e.to_string())?;
                    self.stream = Some(stream);
                    self.stop = stop;
                    self.pos = pos;
                    self.finished = finished;
                    return Ok(());
                }
                Err(e) => last_err = e.to_string(),
            }
        }
        Err(format!("Afspelen lukt niet: {last_err}"))
    }
}

/// Short fade at the joins between ranges so jumps over a cut don't click.
const JOIN_FADE_FRAMES: usize = 240;

fn feed(reader: &mut WavReader, tx: Sender<(u64, Vec<f32>)>, segs: &[(u64, u64)], chunk: usize, stop: Arc<AtomicBool>) {
    let ch = reader.fmt.channels as usize;
    let n_segs = segs.len();
    for (k, &(start, end)) in segs.iter().enumerate() {
        let mut pos = start;
        while pos < end && !stop.load(Ordering::Relaxed) {
            let n = ((end - pos) as usize).min(chunk);
            let mut v = Vec::new();
            match reader.read_f32(pos, n, &mut v) {
                Ok(0) | Err(_) => return,
                Ok(got) => {
                    // fade in after a join, fade out before a join
                    for f in 0..got {
                        let from_start = (pos - start) as usize + f;
                        let to_end = (end - pos) as usize - f - 1;
                        let mut g = 1.0f32;
                        if k > 0 && from_start < JOIN_FADE_FRAMES {
                            g = g.min(from_start as f32 / JOIN_FADE_FRAMES as f32);
                        }
                        if k + 1 < n_segs && to_end < JOIN_FADE_FRAMES {
                            g = g.min(to_end as f32 / JOIN_FADE_FRAMES as f32);
                        }
                        if g < 1.0 {
                            for s in &mut v[f * ch..(f + 1) * ch] {
                                *s *= g;
                            }
                        }
                    }
                    if tx.send((pos, v)).is_err() {
                        return;
                    }
                    pos += got as u64;
                }
            }
        }
    }
    // dropping tx signals the end
}

struct Callback {
    rx: Receiver<(u64, Vec<f32>)>,
    buf: Vec<f32>,
    buf_start: u64,
    idx: usize, // frame index into buf
    frac: f64,
    ratio: f64,
    file_ch: usize,
    out_ch: usize,
    pos: Arc<AtomicU64>,
    end_of_data: bool,
    finished: Arc<AtomicBool>,
}

impl Callback {
    fn new(
        rx: Receiver<(u64, Vec<f32>)>,
        ratio: f64,
        file_ch: usize,
        out_ch: usize,
        pos: Arc<AtomicU64>,
        finished: Arc<AtomicBool>,
    ) -> Self {
        Callback { rx, buf: Vec::new(), buf_start: 0, idx: 0, frac: 0.0, ratio, file_ch, out_ch, pos, end_of_data: false, finished }
    }

    /// Make sure frame `idx` is in the buffer (refilling it when needed).
    fn ensure(&mut self) -> bool {
        while self.idx * self.file_ch >= self.buf.len() {
            if self.end_of_data {
                return false;
            }
            match self.rx.try_recv() {
                Ok((start, v)) => {
                    self.idx -= self.buf.len() / self.file_ch;
                    self.buf = v;
                    self.buf_start = start;
                }
                Err(crossbeam_channel::TryRecvError::Empty) => return false, // underrun: silence
                Err(crossbeam_channel::TryRecvError::Disconnected) => {
                    self.end_of_data = true;
                    return false;
                }
            }
        }
        true
    }

    fn sample(&self, ch: usize) -> f32 {
        let fc = self.file_ch;
        let cur = self.buf[self.idx * fc + ch.min(fc - 1)];
        if self.ratio == 1.0 {
            return cur;
        }
        let next = if (self.idx + 1) * fc < self.buf.len() { self.buf[(self.idx + 1) * fc + ch.min(fc - 1)] } else { cur };
        cur + (next - cur) * self.frac as f32
    }

    fn fill(&mut self, out: &mut [f32]) {
        let oc = self.out_ch;
        for frame in out.chunks_exact_mut(oc) {
            if !self.ensure() {
                frame.fill(0.0);
                if self.end_of_data {
                    self.finished.store(true, Ordering::Relaxed);
                }
                continue;
            }
            match (self.file_ch, oc) {
                (1, _) => {
                    let s = self.sample(0);
                    frame.fill(s);
                }
                (_, 1) => frame[0] = (self.sample(0) + self.sample(1)) * 0.5,
                _ => {
                    frame[0] = self.sample(0);
                    frame[1] = self.sample(1);
                    for x in frame.iter_mut().skip(2) {
                        *x = 0.0;
                    }
                }
            }
            self.frac += self.ratio;
            let whole = self.frac.floor();
            self.idx += whole as usize;
            self.frac -= whole;
        }
        let p = self.buf_start as f64 + self.idx as f64 + self.frac;
        self.pos.store(p.to_bits(), Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cb(ratio: f64, file_ch: usize, out_ch: usize, chunks: Vec<(u64, Vec<f32>)>) -> Callback {
        let (tx, rx) = bounded(16);
        for c in chunks {
            tx.send(c).unwrap();
        }
        drop(tx);
        Callback::new(rx, ratio, file_ch, out_ch, Arc::new(AtomicU64::new(0)), Arc::new(AtomicBool::new(false)))
    }

    #[test]
    fn plays_all_frames_across_chunks_then_finishes() {
        // stereo file, 4-channel device
        let mut c = cb(1.0, 2, 4, vec![(0, vec![0.1, 0.2, 0.3, 0.4]), (2, vec![0.5, 0.6])]);
        let mut out = vec![9.0; 4 * 4];
        c.fill(&mut out);
        assert_eq!(&out[0..4], &[0.1, 0.2, 0.0, 0.0]);
        assert_eq!(&out[4..8], &[0.3, 0.4, 0.0, 0.0]);
        assert_eq!(&out[8..12], &[0.5, 0.6, 0.0, 0.0]);
        assert_eq!(&out[12..16], &[0.0; 4]);
        assert!(c.finished.load(Ordering::Relaxed));
    }

    #[test]
    fn position_jumps_over_cut() {
        // two ranges: 100..104 and 500..504
        let mut c = cb(1.0, 1, 2, vec![(100, vec![0.5; 4]), (500, vec![0.25; 4])]);
        let mut out = vec![0.0; 2 * 6];
        c.fill(&mut out);
        assert_eq!(&out[6..8], &[0.5, 0.5]);
        assert_eq!(&out[8..10], &[0.25, 0.25]);
        assert_eq!(f64::from_bits(c.pos.load(Ordering::Relaxed)), 502.0);
    }

    #[test]
    fn resampling_keeps_time() {
        // 44.1 kHz file on a 48 kHz device: position must follow file time.
        let ratio = 44100.0 / 48000.0;
        let mut c = cb(ratio, 1, 1, vec![(100, (0..44100).map(|i| i as f32 / 44100.0).collect())]);
        let mut out = vec![0.0; 48000];
        c.fill(&mut out);
        let pos = f64::from_bits(c.pos.load(Ordering::Relaxed));
        assert!((pos - (100.0 + 44100.0)).abs() < 1.0, "pos {pos}");
        for w in out.windows(2).take(47000) {
            assert!((w[1] - w[0]).abs() < 0.001);
        }
    }
}
