//! Audio input: device listing, level monitoring and recording to disk.
//!
//! The input stream runs as long as a device is selected, so the level meters
//! work before you press record. Audio travels from the driver callback over a
//! channel to a worker thread that does the metering, waveform peaks and disk
//! writing, keeping the real-time callback as light as possible.

use crate::peaks::Peaks;
use crate::wav::WavWriter;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SampleFormat, SizedSample};
use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, TrySendError, bounded, unbounded};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq)]
pub struct InputChoice {
    pub id: String,
    pub name: String,
    pub loopback: bool,
}

pub const DEMO_ID: &str = "demo:demo";

fn demo_mode() -> bool {
    std::env::var("STUDIOREC_DEMO").is_ok()
}

pub fn device_name(d: &cpal::Device) -> String {
    d.description().map(|x| x.name().to_string()).unwrap_or_else(|_| "Onbekend apparaat".into())
}

/// All capture devices, plus (on Windows) loopback of every output device.
pub fn list_inputs() -> Vec<InputChoice> {
    let mut v = Vec::new();
    if demo_mode() {
        v.push(InputChoice { id: DEMO_ID.into(), name: "Demo-signaal (test)".into(), loopback: false });
    }
    let host = cpal::default_host();
    if let Ok(devs) = host.input_devices() {
        for d in devs {
            if let Ok(id) = d.id() {
                v.push(InputChoice { id: id.to_string(), name: device_name(&d), loopback: false });
            }
        }
    }
    if cfg!(windows) {
        if let Ok(devs) = host.output_devices() {
            for d in devs {
                if let Ok(id) = d.id() {
                    let id = id.to_string();
                    if v.iter().any(|x| x.id == id) {
                        continue;
                    }
                    v.push(InputChoice {
                        id,
                        name: format!("Computergeluid: {}", device_name(&d)),
                        loopback: true,
                    });
                }
            }
        }
    }
    v
}

pub fn default_input_id() -> Option<String> {
    if demo_mode() {
        return Some(DEMO_ID.into());
    }
    cpal::default_host().default_input_device().and_then(|d| d.id().ok()).map(|i| i.to_string())
}

pub fn find_device(id: &str) -> Option<cpal::Device> {
    let host = cpal::default_host();
    let parsed: cpal::DeviceId = id.parse().ok()?;
    host.device_by_id(&parsed)
}

/// State shared between the worker thread and the UI.
pub struct Shared {
    pub sample_rate: u32,
    pub channels: usize,
    /// Peak level since the UI last looked, per channel (f32 bits).
    meter: [AtomicU32; 2],
    pub recording: AtomicBool,
    /// Recording paused: audio is monitored but not written.
    pub paused: AtomicBool,
    pub rec_frames: AtomicU64,
    pub dropouts: AtomicU64,
    pub peaks: Mutex<Peaks>,
    pub error: Mutex<Option<String>>,
    pub write_error: Mutex<Option<String>>,
    pub last_signal: Mutex<Instant>,
}

impl Shared {
    /// Take the peak levels (linear 0..1) collected since the last call.
    pub fn take_meter(&self) -> [f32; 2] {
        [
            f32::from_bits(self.meter[0].swap(0, Ordering::Relaxed)),
            f32::from_bits(self.meter[1].swap(0, Ordering::Relaxed)),
        ]
    }
    fn bump_meter(&self, ch: usize, v: f32) {
        let m = &self.meter[ch];
        let mut cur = m.load(Ordering::Relaxed);
        while f32::from_bits(cur) < v {
            match m.compare_exchange_weak(cur, v.to_bits(), Ordering::Relaxed, Ordering::Relaxed) {
                Ok(_) => break,
                Err(c) => cur = c,
            }
        }
    }
}

enum Cmd {
    Start(PathBuf, Sender<Result<(), String>>),
    Stop(Sender<RecResult>),
}

pub struct RecResult {
    pub path: Option<PathBuf>,
    pub frames: u64,
    pub peaks: Option<Peaks>,
    pub error: Option<String>,
}

pub struct Engine {
    pub shared: Arc<Shared>,
    _stream: Option<cpal::Stream>,
    demo_stop: Option<Arc<AtomicBool>>,
    cmd: Sender<Cmd>,
    worker: Option<JoinHandle<()>>,
}

impl Engine {
    /// Open an input device and start monitoring levels.
    pub fn open(choice: &InputChoice) -> Result<Engine, String> {
        let (tx, rx) = bounded::<Vec<f32>>(4096);
        if choice.id == DEMO_ID {
            let rate = 48000;
            let shared = new_shared(rate, 2);
            let stop = Arc::new(AtomicBool::new(false));
            spawn_demo(tx, rate, stop.clone());
            let (cmd, worker) = spawn_worker(rx, shared.clone(), 2);
            return Ok(Engine {
                shared,
                _stream: None,
                demo_stop: Some(stop),
                cmd,
                worker: Some(worker),
            });
        }
        let device = find_device(&choice.id).ok_or("Apparaat niet gevonden. Is het aangesloten?")?;
        let config = pick_config(&device, choice.loopback)?;
        let dev_channels = config.channels() as usize;
        let rec_channels = dev_channels.min(2);
        let rate = config.sample_rate();
        let shared = new_shared(rate, rec_channels);
        let fmt = config.sample_format();
        let stream_cfg: cpal::StreamConfig = config.into();
        let err_shared = shared.clone();
        let stream = match fmt {
            SampleFormat::F32 => build::<f32>(&device, &stream_cfg, tx, dev_channels, rec_channels, err_shared),
            SampleFormat::I16 => build::<i16>(&device, &stream_cfg, tx, dev_channels, rec_channels, err_shared),
            SampleFormat::I32 => build::<i32>(&device, &stream_cfg, tx, dev_channels, rec_channels, err_shared),
            SampleFormat::I24 => build::<cpal::I24>(&device, &stream_cfg, tx, dev_channels, rec_channels, err_shared),
            SampleFormat::U16 => build::<u16>(&device, &stream_cfg, tx, dev_channels, rec_channels, err_shared),
            SampleFormat::F64 => build::<f64>(&device, &stream_cfg, tx, dev_channels, rec_channels, err_shared),
            other => return Err(format!("Sampleformaat {other:?} wordt niet ondersteund")),
        }
        .map_err(|e| format!("Kan apparaat niet openen: {e}"))?;
        stream.play().map_err(|e| format!("Kan opname niet starten: {e}"))?;
        let (cmd, worker) = spawn_worker(rx, shared.clone(), rec_channels);
        Ok(Engine {
            shared,
            _stream: Some(stream),
            demo_stop: None,
            cmd,
            worker: Some(worker),
        })
    }

    pub fn start_recording(&self, path: &Path) -> Result<(), String> {
        let (tx, rx) = bounded(1);
        self.cmd.send(Cmd::Start(path.to_path_buf(), tx)).map_err(|_| "Audio-thread gestopt".to_string())?;
        rx.recv_timeout(Duration::from_secs(5)).map_err(|_| "Geen reactie van audio-thread".to_string())?
    }

    pub fn stop_recording(&self) -> RecResult {
        let (tx, rx) = bounded(1);
        if self.cmd.send(Cmd::Stop(tx)).is_err() {
            return RecResult { path: None, frames: 0, peaks: None, error: Some("Audio-thread gestopt".into()) };
        }
        rx.recv_timeout(Duration::from_secs(30)).unwrap_or(RecResult {
            path: None,
            frames: 0,
            peaks: None,
            error: Some("Opname kon niet netjes worden afgesloten".into()),
        })
    }

    pub fn set_paused(&self, on: bool) {
        self.shared.paused.store(on, Ordering::Relaxed);
    }

    pub fn is_paused(&self) -> bool {
        self.is_recording() && self.shared.paused.load(Ordering::Relaxed)
    }

    pub fn is_recording(&self) -> bool {
        self.shared.recording.load(Ordering::Relaxed)
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        if self.is_recording() {
            let _ = self.stop_recording();
        }
        if let Some(s) = &self.demo_stop {
            s.store(true, Ordering::Relaxed);
        }
        self._stream = None; // closes the device; the worker sees the channel close
        let (dummy, _) = unbounded();
        self.cmd = dummy;
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
    }
}

fn new_shared(sample_rate: u32, channels: usize) -> Arc<Shared> {
    Arc::new(Shared {
        sample_rate,
        channels,
        meter: [AtomicU32::new(0), AtomicU32::new(0)],
        recording: AtomicBool::new(false),
        paused: AtomicBool::new(false),
        rec_frames: AtomicU64::new(0),
        dropouts: AtomicU64::new(0),
        peaks: Mutex::new(Peaks::new(channels, sample_rate)),
        error: Mutex::new(None),
        write_error: Mutex::new(None),
        last_signal: Mutex::new(Instant::now()),
    })
}

/// Prefer 48 kHz in a float or high-resolution integer format; otherwise use
/// the device default.
fn pick_config(device: &cpal::Device, loopback: bool) -> Result<cpal::SupportedStreamConfig, String> {
    if loopback {
        return device.default_output_config().map_err(|e| format!("Geen geschikt formaat: {e}"));
    }
    let default = device.default_input_config().map_err(|e| format!("Geen geschikt formaat: {e}"))?;
    if default.sample_rate() == 48000 {
        return Ok(default);
    }
    if let Ok(ranges) = device.supported_input_configs() {
        let rank = |f: SampleFormat| match f {
            SampleFormat::F32 => 0,
            SampleFormat::I32 => 1,
            SampleFormat::I24 => 2,
            SampleFormat::I16 => 3,
            _ => 9,
        };
        let mut best: Option<cpal::SupportedStreamConfig> = None;
        for r in ranges {
            if r.min_sample_rate() <= 48000 && r.max_sample_rate() >= 48000 && rank(r.sample_format()) < 9 {
                if r.channels() < default.channels().min(2) {
                    continue;
                }
                let c = r.with_sample_rate(48000);
                let better = match &best {
                    None => true,
                    Some(b) => {
                        rank(c.sample_format()) < rank(b.sample_format())
                            || (c.channels() == default.channels() && b.channels() != default.channels())
                    }
                };
                if better {
                    best = Some(c);
                }
            }
        }
        if let Some(b) = best {
            return Ok(b);
        }
    }
    Ok(default)
}

fn build<T>(
    device: &cpal::Device,
    cfg: &cpal::StreamConfig,
    tx: Sender<Vec<f32>>,
    dev_ch: usize,
    rec_ch: usize,
    shared: Arc<Shared>,
) -> Result<cpal::Stream, cpal::Error>
where
    T: SizedSample + Send + 'static,
    f32: FromSample<T>,
{
    let err_shared = shared.clone();
    device.build_input_stream::<T, _, _>(
        cfg.clone(),
        move |data: &[T], _| {
            let frames = data.len() / dev_ch;
            let mut v = Vec::with_capacity(frames * rec_ch);
            for f in data.chunks_exact(dev_ch) {
                for c in 0..rec_ch {
                    v.push(f32::from_sample(f[c]));
                }
            }
            if let Err(TrySendError::Full(_)) = tx.try_send(v) {
                shared.dropouts.fetch_add(1, Ordering::Relaxed);
            }
        },
        move |err| {
            let msg = err.to_string();
            let lower = msg.to_lowercase();
            // Glitches are counted; real failures are shown to the user.
            if lower.contains("xrun") || lower.contains("underrun") || lower.contains("overrun") {
                err_shared.dropouts.fetch_add(1, Ordering::Relaxed);
            } else if let Ok(mut e) = err_shared.error.lock() {
                *e = Some(msg);
            }
        },
        None,
    )
}

fn spawn_worker(rx: Receiver<Vec<f32>>, shared: Arc<Shared>, channels: usize) -> (Sender<Cmd>, JoinHandle<()>) {
    let (cmd_tx, cmd_rx) = unbounded::<Cmd>();
    let h = std::thread::Builder::new()
        .name("rec-worker".into())
        .spawn(move || worker(rx, cmd_rx, shared, channels))
        .expect("thread");
    (cmd_tx, h)
}

fn worker(rx: Receiver<Vec<f32>>, cmd: Receiver<Cmd>, shared: Arc<Shared>, channels: usize) {
    let mut writer: Option<WavWriter> = None;
    let mut last_hdr = Instant::now();
    let mut last_sync = Instant::now();
    let finish = |writer: &mut Option<WavWriter>, shared: &Shared| -> RecResult {
        shared.recording.store(false, Ordering::Relaxed);
        let peaks = shared.peaks.lock().ok().map(|p| p.clone());
        match writer.take() {
            Some(w) => {
                let path = w.path().to_path_buf();
                match w.finalize() {
                    Ok(frames) => RecResult { path: Some(path), frames, peaks, error: None },
                    Err(e) => RecResult {
                        path: Some(path),
                        frames: 0,
                        peaks,
                        error: Some(format!("Fout bij afsluiten bestand: {e}")),
                    },
                }
            }
            None => RecResult { path: None, frames: 0, peaks: None, error: None },
        }
    };
    loop {
        while let Ok(c) = cmd.try_recv() {
            match c {
                Cmd::Start(path, reply) => {
                    if let Some(w) = writer.take() {
                        let _ = w.finalize();
                    }
                    match WavWriter::create(&path, shared.sample_rate, channels as u16, 24) {
                        Ok(w) => {
                            writer = Some(w);
                            if let Ok(mut p) = shared.peaks.lock() {
                                *p = Peaks::new(channels, shared.sample_rate);
                            }
                            shared.rec_frames.store(0, Ordering::Relaxed);
                            shared.dropouts.store(0, Ordering::Relaxed);
                            *shared.write_error.lock().unwrap() = None;
                            shared.paused.store(false, Ordering::Relaxed);
                            shared.recording.store(true, Ordering::Relaxed);
                            last_hdr = Instant::now();
                            last_sync = Instant::now();
                            let _ = reply.send(Ok(()));
                        }
                        Err(e) => {
                            let _ = reply.send(Err(format!("Kan bestand niet aanmaken: {e}")));
                        }
                    }
                }
                Cmd::Stop(reply) => {
                    // Drain what is already queued so the end isn't cut off.
                    while let Ok(chunk) = rx.try_recv() {
                        if let Some(w) = writer.as_mut() {
                            let _ = w.write_f32(&chunk);
                            if let Ok(mut p) = shared.peaks.lock() {
                                p.push(&chunk);
                            }
                        }
                    }
                    let _ = reply.send(finish(&mut writer, &shared));
                }
            }
        }
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(chunk) => {
                let mut any = false;
                for f in chunk.chunks_exact(channels) {
                    for (c, &s) in f.iter().enumerate() {
                        let a = s.abs();
                        shared.bump_meter(c.min(1), a);
                        if channels == 1 {
                            shared.bump_meter(1, a);
                        }
                        if a > 0.003 {
                            any = true; // about -50 dBFS
                        }
                    }
                }
                if any {
                    if let Ok(mut t) = shared.last_signal.lock() {
                        *t = Instant::now();
                    }
                }
                let paused = shared.paused.load(Ordering::Relaxed);
                if let (false, Some(w)) = (paused, writer.as_mut()) {
                    if let Err(e) = w.write_f32(&chunk) {
                        *shared.write_error.lock().unwrap() = Some(format!("Schrijffout: {e}"));
                    }
                    if let Ok(mut p) = shared.peaks.lock() {
                        p.push(&chunk);
                    }
                    shared.rec_frames.fetch_add((chunk.len() / channels) as u64, Ordering::Relaxed);
                    if last_hdr.elapsed() >= Duration::from_secs(1) {
                        let sync = last_sync.elapsed() >= Duration::from_secs(5);
                        if let Err(e) = w.refresh_header(sync) {
                            *shared.write_error.lock().unwrap() = Some(format!("Schrijffout: {e}"));
                        }
                        last_hdr = Instant::now();
                        if sync {
                            last_sync = Instant::now();
                        }
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                if writer.is_some() {
                    let _ = finish(&mut writer, &shared);
                }
                // Keep answering commands until the engine is dropped.
                match cmd.recv() {
                    Ok(Cmd::Stop(reply)) => {
                        let _ = reply.send(finish(&mut writer, &shared));
                    }
                    Ok(Cmd::Start(_, reply)) => {
                        let _ = reply.send(Err("Invoerapparaat is weggevallen".into()));
                    }
                    Err(_) => return,
                }
            }
        }
    }
}

/// Test signal for development without a sound card: speech-like bursts.
fn spawn_demo(tx: Sender<Vec<f32>>, rate: u32, stop: Arc<AtomicBool>) {
    std::thread::spawn(move || {
        let block = (rate / 100) as usize;
        let mut t: u64 = 0;
        let mut seed: u32 = 12345;
        let start = Instant::now();
        let mut sent: u64 = 0;
        while !stop.load(Ordering::Relaxed) {
            let due = (start.elapsed().as_secs_f64() * rate as f64) as u64;
            while sent + block as u64 <= due {
                let mut v = Vec::with_capacity(block * 2);
                for _ in 0..block {
                    let secs = t as f32 / rate as f32;
                    let env = ((secs * 2.7).sin() * 0.5 + 0.5) * ((secs * 0.37).sin() * 0.3 + 0.7);
                    let syll = ((secs * 9.0).sin().abs()).powf(0.6);
                    seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
                    let noise = ((seed >> 9) as f32 / 4_194_304.0) - 1.0;
                    let tone = (secs * 2.0 * std::f32::consts::PI * 180.0).sin() * 0.6
                        + (secs * 2.0 * std::f32::consts::PI * 360.0).sin() * 0.25;
                    let s = (tone * 0.7 + noise * 0.3) * env * syll * 0.7;
                    v.push(s);
                    v.push(s * 0.85);
                    t += 1;
                }
                if tx.send(v).is_err() {
                    return;
                }
                sent += block as u64;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    });
}

#[cfg(windows)]
pub fn keep_awake(on: bool) {
    use windows_sys::Win32::System::Power::{
        ES_CONTINUOUS, ES_SYSTEM_REQUIRED, SetThreadExecutionState,
    };
    unsafe {
        if on {
            SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED);
        } else {
            SetThreadExecutionState(ES_CONTINUOUS);
        }
    }
}

#[cfg(not(windows))]
pub fn keep_awake(_on: bool) {}
