//! Export the kept parts of a WAV file (IN..UIT minus cuts) as FLAC or WAV.
//!
//! Joins between kept parts get a short crossfade so a cut never clicks.

use crate::wav::{WavReader, WavWriter};
use flacenc::bitsink::ByteSink;
use flacenc::component::{BitRepr, MetadataBlockData, Stream};
use flacenc::error::Verify;
use flacenc::source::{Context, Fill, FrameBuf};
use std::fs::File;
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ExportFormat {
    Flac,
    Wav,
    /// MP3 320 kbps CBR (highest MP3 quality) (AudioShare upload format; FLAC of an hour exceeds 200 MB).
    Mp3,
}

impl ExportFormat {
    pub fn ext(self) -> &'static str {
        match self {
            ExportFormat::Flac => "flac",
            ExportFormat::Wav => "wav",
            ExportFormat::Mp3 => "mp3",
        }
    }
}

pub struct ExportJob<'a> {
    pub src: &'a Path,
    pub dst: &'a Path,
    pub format: ExportFormat,
    /// Ranges of source frames to keep, in order, non-overlapping.
    pub segments: Vec<(u64, u64)>,
    /// Fade length in ms at the very start and end (0 = none).
    pub fade_ms: u32,
    /// Crossfade length in ms at each cut (join between segments).
    pub crossfade_ms: u32,
    /// Vorbis comments for FLAC, e.g. ("TITLE", "..").
    pub tags: Vec<(String, String)>,
    /// MP3 bitrate, mono output, leveler/limiter (defaults = StudioRecorder behaviour).
    pub opts: ExportOptions,
}

/// Extra export options. The defaults give exactly the StudioRecorder behaviour.
#[derive(Clone, Default)]
pub struct ExportOptions {
    /// MP3 bitrate in kbps (0 = 320).
    pub mp3_kbps: u32,
    /// LAME quality 0 (slowest, "Best") ..= 9; `None` = Best (as before).
    pub mp3_quality: Option<u8>,
    /// Mix stereo down to mono.
    pub mono: bool,
    /// Volume leveling and/or peak limiting.
    pub level: Option<std::sync::Arc<crate::level::LevelPlan>>,
}

const BLOCK: usize = 4096;

/// Crossfade length (frames) for each join, limited to half the shorter neighbour.
pub fn join_lengths(segs: &[(u64, u64)], xf: u64) -> Vec<u64> {
    segs.windows(2)
        .map(|w| xf.min((w[0].1 - w[0].0) / 2).min((w[1].1 - w[1].0) / 2))
        .collect()
}

/// Number of frames the export will contain.
pub fn output_frames(segs: &[(u64, u64)], xf: u64) -> u64 {
    let total: u64 = segs.iter().map(|(a, b)| b - a).sum();
    total - join_lengths(segs, xf).iter().sum::<u64>()
}

pub fn export(job: &ExportJob, progress: &AtomicU32, cancel: &AtomicBool) -> Result<(), String> {
    let mut r = WavReader::open(job.src).map_err(|e| format!("Bron niet leesbaar: {e}"))?;
    let segs: Vec<(u64, u64)> = job
        .segments
        .iter()
        .map(|&(a, b)| (a.min(r.frames), b.min(r.frames)))
        .filter(|(a, b)| a < b)
        .collect();
    if segs.is_empty() {
        return Err("Lege selectie".into());
    }
    let bits = r.export_bits();
    let ch = r.fmt.channels as usize;
    let rate = r.fmt.sample_rate;
    let xf = job.crossfade_ms as u64 * rate as u64 / 1000;
    let total = output_frames(&segs, xf);
    let fade = ((job.fade_ms as u64 * rate as u64) / 1000).min(total / 2);

    // Write to a temporary name first so a half-written export never looks finished.
    let tmp = job.dst.with_extension(format!("{}.part", job.format.ext()));
    let res = (|| -> Result<(), String> {
        let mono = job.opts.mono && ch == 2;
        let out_ch = if mono { 1 } else { ch };
        let mut sink: Box<dyn Sink> = match job.format {
            ExportFormat::Wav => Box::new(WavSink::new(&tmp, rate, out_ch, bits)?),
            ExportFormat::Flac => Box::new(FlacSink::new(&tmp, rate, out_ch, bits, &job.tags)?),
            ExportFormat::Mp3 => Box::new(Mp3Sink::new(&tmp, rate, out_ch, bits, &job.tags, &job.opts)?),
        };
        let full = (1i64 << (bits - 1)) as f32;
        let plan = job.opts.level.clone();
        let mut limiter = plan.as_ref().and_then(|p| p.limiter.map(|l| crate::level::Limiter::new(rate, ch, &l)));
        let gain = plan.as_ref().and_then(|p| p.curve.clone());
        let mut written = 0u64;
        let mut fbuf: Vec<f32> = Vec::new();
        let mut mono_buf: Vec<i32> = Vec::new();
        // Fade, mono and write one block of finished output frames.
        let mut finish_block = |block: &mut Vec<i32>, written: &mut u64| -> Result<(), String> {
            apply_fade(block, ch, *written, total, fade);
            *written += (block.len() / ch) as u64;
            progress.store((*written * 999 / total.max(1)) as u32, Ordering::Relaxed);
            if mono {
                mono_buf.clear();
                mono_buf.extend(block.chunks_exact(2).map(|f| ((f[0] as i64 + f[1] as i64) / 2) as i32));
                sink.write(&mono_buf)
            } else {
                sink.write(block)
            }
        };
        let gain_fn = gain.as_ref().map(|g| move |pos: u64, buf: &mut [i32], ch: usize| g.apply_i32(pos, buf, ch));
        let gain_ref: Option<&dyn Fn(u64, &mut [i32], usize)> = gain_fn.as_ref().map(|f| f as &dyn Fn(u64, &mut [i32], usize));
        stream_segments(&mut r, &segs, xf, ch, cancel, gain_ref, &mut |block: &mut Vec<i32>| {
            if let Some(lim) = limiter.as_mut() {
                fbuf.clear();
                fbuf.extend(block.iter().map(|&x| x as f32 / full));
                let out = lim.process(&fbuf);
                block.clear();
                block.extend(out.iter().map(|&x| quantize(x, full)));
                if block.is_empty() {
                    return Ok(());
                }
            } else if gain_ref.is_some() {
                // Leveled without limiter: never wrap around, clip at full scale.
                let m = full as i32 - 1;
                for x in block.iter_mut() {
                    *x = (*x).clamp(-m - 1, m);
                }
            }
            finish_block(block, &mut written)
        })?;
        if cancel.load(Ordering::Relaxed) {
            return Ok(());
        }
        if let Some(lim) = limiter.as_mut() {
            let mut rest: Vec<i32> = lim.flush().iter().map(|&x| quantize(x, full)).collect();
            if !rest.is_empty() {
                finish_block(&mut rest, &mut written)?;
            }
        }
        sink.finish()
    })();
    match res {
        Ok(()) if cancel.load(Ordering::Relaxed) => {
            let _ = std::fs::remove_file(&tmp);
            Err("Geannuleerd".into())
        }
        Ok(()) => {
            let _ = std::fs::remove_file(job.dst);
            std::fs::rename(&tmp, job.dst).map_err(|e| format!("Kan bestand niet hernoemen: {e}"))?;
            progress.store(1000, Ordering::Relaxed);
            Ok(())
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// Read the segments in order and hand out blocks of interleaved samples,
/// crossfading the end of each segment into the start of the next.
fn stream_segments(
    r: &mut WavReader,
    segs: &[(u64, u64)],
    xf: u64,
    ch: usize,
    cancel: &AtomicBool,
    gain: Option<&dyn Fn(u64, &mut [i32], usize)>,
    emit: &mut dyn FnMut(&mut Vec<i32>) -> Result<(), String>,
) -> Result<(), String> {
    // Leveler: gain per source frame, applied right after reading (before crossfades).
    let mut read = |r: &mut WavReader, start: u64, n: usize, out: &mut Vec<i32>| -> Result<usize, String> {
        let got = r.read_i32(start, n, out).map_err(|e| e.to_string())?;
        if let Some(g) = gain {
            g(start, &mut out[..got * ch], ch);
        }
        Ok(got)
    };
    let joins = join_lengths(segs, xf);
    let mut tail: Vec<i32> = Vec::new();
    let mut buf = Vec::new();
    for (k, &(a, b)) in segs.iter().enumerate() {
        let mut pos = a;
        if k > 0 {
            let n = joins[k - 1] as usize;
            if n > 0 {
                read(r, a, n, &mut buf)?;
                let mut mixed = Vec::with_capacity(n * ch);
                for i in 0..n {
                    let t = (i as f64 + 0.5) / n as f64;
                    for c in 0..ch {
                        let x = tail[i * ch + c] as f64 * (1.0 - t) + buf[i * ch + c] as f64 * t;
                        mixed.push(x.round() as i32);
                    }
                }
                emit(&mut mixed)?;
                pos = a + n as u64;
            }
        }
        let hold = if k + 1 < segs.len() { joins[k] } else { 0 };
        let body_end = b - hold;
        while pos < body_end {
            if cancel.load(Ordering::Relaxed) {
                return Ok(());
            }
            let n = read(r, pos, ((body_end - pos) as usize).min(65536), &mut buf)?;
            if n == 0 {
                return Err("Onverwacht einde van het bronbestand".into());
            }
            emit(&mut buf)?;
            pos += n as u64;
        }
        tail.clear();
        if hold > 0 {
            read(r, body_end, hold as usize, &mut tail)?;
        }
    }
    Ok(())
}

/// Back from normalised float to the integer sample range (clipped).
fn quantize(x: f32, full: f32) -> i32 {
    let v = (x * full).round();
    v.clamp(-full, full - 1.0) as i32
}

/// Apply linear fades to a block starting at output frame `pos`.
fn apply_fade(buf: &mut [i32], ch: usize, pos: u64, total: u64, fade: u64) {
    if fade == 0 {
        return;
    }
    for (i, f) in buf.chunks_exact_mut(ch).enumerate() {
        let p = pos + i as u64;
        let g = if p < fade {
            p as f64 / fade as f64
        } else if p >= total - fade {
            (total - 1 - p) as f64 / fade as f64
        } else {
            continue;
        };
        for s in f.iter_mut() {
            *s = (*s as f64 * g).round() as i32;
        }
    }
}

trait Sink {
    fn write(&mut self, block: &[i32]) -> Result<(), String>;
    fn finish(self: Box<Self>) -> Result<(), String>;
}

struct WavSink(WavWriter);

impl WavSink {
    fn new(dst: &Path, rate: u32, ch: usize, bits: u16) -> Result<Self, String> {
        Ok(WavSink(WavWriter::create(dst, rate, ch as u16, bits).map_err(|e| format!("Kan doelbestand niet maken: {e}"))?))
    }
}

impl Sink for WavSink {
    fn write(&mut self, block: &[i32]) -> Result<(), String> {
        self.0.write_i32(block).map_err(|e| format!("Schrijffout: {e}"))
    }
    fn finish(self: Box<Self>) -> Result<(), String> {
        self.0.finalize().map(|_| ()).map_err(|e| format!("Schrijffout: {e}"))
    }
}

/// Latin-1 bytes for the ID3 tag LAME writes (other characters become '?').
fn latin1(s: &str) -> Vec<u8> {
    s.chars().map(|c| if (c as u32) < 256 { c as u32 as u8 } else { b'?' }).collect()
}

struct Mp3Sink {
    enc: mp3lame_encoder::Encoder,
    out: BufWriter<File>,
    ch: usize,
    scale: f32,
    left: Vec<f32>,
    right: Vec<f32>,
    buf: Vec<u8>,
}

impl Mp3Sink {
    fn new(dst: &Path, rate: u32, ch: usize, bits: u16, tags: &[(String, String)], opts: &ExportOptions) -> Result<Self, String> {
        use mp3lame_encoder::{Bitrate, Builder, Id3Tag, Quality};
        let e = |x: &dyn std::fmt::Debug| format!("MP3-encoder: {x:?}");
        let mut b = Builder::new().ok_or("MP3-encoder kon niet starten")?;
        b.set_num_channels(ch.min(2) as u8).map_err(|x| e(&x))?;
        b.set_sample_rate(rate).map_err(|x| e(&x))?;
        let brate = match opts.mp3_kbps {
            0 | 320 => Bitrate::Kbps320,
            256 => Bitrate::Kbps256,
            224 => Bitrate::Kbps224,
            192 => Bitrate::Kbps192,
            160 => Bitrate::Kbps160,
            128 => Bitrate::Kbps128,
            k => return Err(format!("MP3-bitrate {k} kbps wordt niet ondersteund")),
        };
        b.set_brate(brate).map_err(|x| e(&x))?;
        let q = match opts.mp3_quality.unwrap_or(0) {
            0 => Quality::Best,
            1 => Quality::SecondBest,
            2 => Quality::NearBest,
            3 => Quality::VeryNice,
            4 => Quality::Nice,
            _ => Quality::Good,
        };
        b.set_quality(q).map_err(|x| e(&x))?;
        let tag = |k: &str| latin1(tags.iter().find(|(t, _)| t == k).map(|(_, v)| v.as_str()).unwrap_or(""));
        let (title, artist, album, year, comment) = (tag("TITLE"), tag("ARTIST"), tag("ALBUM"), tag("DATE"), tag("DESCRIPTION"));
        let year: Vec<u8> = year.into_iter().take(4).collect();
        let _ = b.set_id3_tag(Id3Tag { title: &title, artist: &artist, album: &album, album_art: &[], year: &year, comment: &comment });
        let enc = b.build().map_err(|x| e(&x))?;
        let file = File::create(dst).map_err(|x| format!("Kan doelbestand niet maken: {x}"))?;
        Ok(Mp3Sink {
            enc,
            out: BufWriter::with_capacity(1 << 20, file),
            ch: ch.min(2),
            scale: 1.0 / (1u32 << (bits - 1)) as f32,
            left: Vec::new(),
            right: Vec::new(),
            buf: Vec::new(),
        })
    }
}

impl Sink for Mp3Sink {
    fn write(&mut self, block: &[i32]) -> Result<(), String> {
        use mp3lame_encoder::{DualPcm, MonoPcm};
        self.left.clear();
        self.right.clear();
        for f in block.chunks_exact(self.ch) {
            self.left.push(f[0] as f32 * self.scale);
            if self.ch == 2 {
                self.right.push(f[1] as f32 * self.scale);
            }
        }
        self.buf.clear();
        self.buf.reserve(mp3lame_encoder::max_required_buffer_size(self.left.len()));
        let r = if self.ch == 2 {
            self.enc.encode_to_vec(DualPcm { left: &self.left, right: &self.right }, &mut self.buf)
        } else {
            self.enc.encode_to_vec(MonoPcm(&self.left), &mut self.buf)
        };
        r.map_err(|x| format!("MP3-encoder: {x:?}"))?;
        self.out.write_all(&self.buf).map_err(|x| format!("Schrijffout: {x}"))
    }

    fn finish(mut self: Box<Self>) -> Result<(), String> {
        self.buf.clear();
        self.buf.reserve(16 * 1024);
        self.enc
            .flush_to_vec::<mp3lame_encoder::FlushNoGap>(&mut self.buf)
            .map_err(|x| format!("MP3-encoder: {x:?}"))?;
        self.out.write_all(&self.buf).map_err(|x| format!("Schrijffout: {x}"))?;
        self.out.flush().map_err(|x| x.to_string())?;
        self.out.get_ref().sync_all().map_err(|x| x.to_string())
    }
}

fn vorbis_comment(tags: &[(String, String)]) -> Vec<u8> {
    let vendor = format!("{} {}", crate::edition::app_name(), crate::edition::version());
    let mut v = Vec::new();
    v.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
    v.extend_from_slice(vendor.as_bytes());
    v.extend_from_slice(&(tags.len() as u32).to_le_bytes());
    for (k, val) in tags {
        let s = format!("{}={}", k.to_uppercase(), val);
        v.extend_from_slice(&(s.len() as u32).to_le_bytes());
        v.extend_from_slice(s.as_bytes());
    }
    v
}

struct FlacSink {
    cfg: flacenc::error::Verified<flacenc::config::Encoder>,
    stream: Stream,
    out: BufWriter<File>,
    header_len: usize,
    ctx: Context,
    ch: usize,
    pending: Vec<i32>,
    frame_no: usize,
    threads: usize,
    min_fs: usize,
    max_fs: usize,
}

fn header_bytes(s: &Stream) -> Result<Vec<u8>, String> {
    let mut sink = ByteSink::new();
    s.write(&mut sink).map_err(|e| format!("{e:?}"))?;
    Ok(sink.as_slice().to_vec())
}

impl FlacSink {
    fn new(dst: &Path, rate: u32, ch: usize, bits: u16, tags: &[(String, String)]) -> Result<Self, String> {
        let bits = bits as usize;
        let mut cfg = flacenc::config::Encoder::default();
        cfg.block_size = BLOCK;
        cfg.multithread = false;
        let cfg = cfg.into_verified().map_err(|e| format!("{e:?}"))?;
        let mut stream = Stream::new(rate as usize, ch, bits).map_err(|e| format!("{e:?}"))?;
        stream.add_metadata_block(MetadataBlockData::new_unknown(4, &vorbis_comment(tags)).map_err(|e| format!("{e:?}"))?);
        let file = File::create(dst).map_err(|e| format!("Kan doelbestand niet maken: {e}"))?;
        let mut out = BufWriter::with_capacity(1 << 20, file);
        let hdr = header_bytes(&stream)?;
        out.write_all(&hdr).map_err(|e| e.to_string())?;
        Ok(FlacSink {
            cfg,
            stream,
            out,
            header_len: hdr.len(),
            ctx: Context::new(bits, ch),
            ch,
            pending: Vec::new(),
            frame_no: 0,
            threads: std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2).clamp(1, 16),
            min_fs: u32::MAX as usize,
            max_fs: 0,
        })
    }

    /// Encode `samples` (whole blocks, plus a short last block only at the end) in parallel.
    fn encode(&mut self, samples: &[i32]) -> Result<(), String> {
        if samples.is_empty() {
            return Ok(());
        }
        let ch = self.ch;
        self.ctx.fill_interleaved(samples).map_err(|e| format!("{e:?}"))?;
        let blocks: Vec<&[i32]> = samples.chunks(BLOCK * ch).collect();
        let per = blocks.len().div_ceil(self.threads).max(1);
        let info = self.stream.stream_info().clone();
        let cfg = &self.cfg;
        let frame_no = self.frame_no;
        let mut results: Vec<Vec<Result<Vec<u8>, String>>> = Vec::new();
        std::thread::scope(|sc| {
            let mut handles = Vec::new();
            for (t, group) in blocks.chunks(per).enumerate() {
                let info = &info;
                let first = frame_no + t * per;
                handles.push(sc.spawn(move || {
                    let mut fb = FrameBuf::with_size(ch, BLOCK).unwrap();
                    group
                        .iter()
                        .enumerate()
                        .map(|(k, blk)| {
                            fb.fill_interleaved(blk).map_err(|e| format!("{e:?}"))?;
                            let frame =
                                flacenc::encode_fixed_size_frame(cfg, &fb, first + k, info).map_err(|e| format!("{e:?}"))?;
                            let mut sink = ByteSink::new();
                            frame.write(&mut sink).map_err(|e| format!("{e:?}"))?;
                            Ok(sink.as_slice().to_vec())
                        })
                        .collect::<Vec<_>>()
                }));
            }
            for h in handles {
                results.push(h.join().unwrap_or_else(|_| vec![Err("Encoder-thread mislukt".into())]));
            }
        });
        for group in results {
            for fr in group {
                let bytes = fr?;
                self.min_fs = self.min_fs.min(bytes.len());
                self.max_fs = self.max_fs.max(bytes.len());
                self.out.write_all(&bytes).map_err(|e| format!("Schrijffout: {e}"))?;
                self.frame_no += 1;
            }
        }
        Ok(())
    }
}

impl Sink for FlacSink {
    fn write(&mut self, block: &[i32]) -> Result<(), String> {
        self.pending.extend_from_slice(block);
        let batch = BLOCK * self.ch * self.threads * 8;
        if self.pending.len() >= batch {
            let whole = self.pending.len() / (BLOCK * self.ch) * (BLOCK * self.ch);
            let rest = self.pending.split_off(whole);
            let take = std::mem::replace(&mut self.pending, rest);
            self.encode(&take)?;
        }
        Ok(())
    }

    fn finish(mut self: Box<Self>) -> Result<(), String> {
        let rest = std::mem::take(&mut self.pending);
        self.encode(&rest)?;
        let md5 = self.ctx.md5_digest();
        let total = self.ctx.total_samples();
        let info = self.stream.stream_info_mut();
        info.set_md5_digest(&md5);
        info.set_total_samples(total);
        info.set_block_sizes(BLOCK, BLOCK).map_err(|e| format!("{e:?}"))?;
        if self.max_fs > 0 {
            info.set_frame_sizes(self.min_fs, self.max_fs).map_err(|e| format!("{e:?}"))?;
        }
        let hdr2 = header_bytes(&self.stream)?;
        if hdr2.len() != self.header_len {
            return Err("Interne fout: koptekst wijzigde van grootte".into());
        }
        self.out.flush().map_err(|e| e.to_string())?;
        let mut file = self.out.into_inner().map_err(|e| e.to_string())?;
        file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
        file.write_all(&hdr2).map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join("studiorec_tests");
        std::fs::create_dir_all(&d).unwrap();
        d.join(name)
    }

    fn make_src(name: &str, frames: usize, ch: u16) -> PathBuf {
        let p = tmp(name);
        let mut w = WavWriter::create(&p, 48000, ch, 24).unwrap();
        let mut v = Vec::new();
        let mut seed = 1u32;
        for i in 0..frames {
            for c in 0..ch {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                let noise = (seed >> 8) as f32 / 16_777_216.0 - 0.5;
                v.push(((i as f32 * 0.03 + c as f32).sin() * 0.6) + noise * 0.05);
            }
        }
        w.write_f32(&v).unwrap();
        w.finalize().unwrap();
        p
    }

    #[test]
    fn flac_export_is_bit_exact() {
        let src = make_src("src_flac.wav", 200_000, 2);
        let dst = tmp("out.flac");
        let job = ExportJob {
            src: &src,
            dst: &dst,
            format: ExportFormat::Flac,
            segments: vec![(12_345, 187_654)],
            fade_ms: 0,
            crossfade_ms: 10,
            tags: vec![("TITLE".into(), "Test uitzending".into()), ("DATE".into(), "2026-10-11".into())],
            opts: ExportOptions::default(),
        };
        let prog = AtomicU32::new(0);
        export(&job, &prog, &AtomicBool::new(false)).unwrap();
        assert_eq!(prog.load(Ordering::Relaxed), 1000);

        let mut fr = claxon::FlacReader::open(&dst).unwrap();
        let info = fr.streaminfo();
        assert_eq!(info.sample_rate, 48000);
        assert_eq!(info.channels, 2);
        assert_eq!(info.bits_per_sample, 24);
        assert_eq!(info.samples, Some(187_654 - 12_345));
        assert_eq!(fr.get_tag("TITLE").next(), Some("Test uitzending"));
        let decoded: Vec<i32> = fr.samples().map(|s| s.unwrap()).collect();
        let mut r = WavReader::open(&src).unwrap();
        let mut orig = Vec::new();
        r.read_i32(12_345, 187_654 - 12_345, &mut orig).unwrap();
        assert_eq!(decoded.len(), orig.len());
        assert!(decoded == orig, "decoded audio differs from source");
    }

    #[test]
    fn cut_removes_middle_with_crossfade() {
        let src = make_src("src_cut.wav", 100_000, 1);
        let dst = tmp("cut.wav");
        // keep 0..30000 and 50000..100000, 10 ms (480 frames) crossfade
        let segs = vec![(0, 30_000), (50_000, 100_000)];
        let job = ExportJob {
            src: &src,
            dst: &dst,
            format: ExportFormat::Wav,
            segments: segs.clone(),
            fade_ms: 0,
            crossfade_ms: 10,
            tags: vec![],
            opts: ExportOptions::default(),
        };
        export(&job, &AtomicU32::new(0), &AtomicBool::new(false)).unwrap();
        let mut out = WavReader::open(&dst).unwrap();
        assert_eq!(out.frames, 80_000 - 480);
        assert_eq!(out.frames, output_frames(&segs, 480));
        let mut s = WavReader::open(&src).unwrap();
        let (mut o, mut x) = (Vec::new(), Vec::new());
        // before the join: identical to source
        out.read_i32(0, 29_520, &mut o).unwrap();
        s.read_i32(0, 29_520, &mut x).unwrap();
        assert!(o == x);
        // after the crossfade: identical to source from 50480 on
        out.read_i32(30_000, 1000, &mut o).unwrap();
        s.read_i32(50_480, 1000, &mut x).unwrap();
        assert!(o == x);
        // in the crossfade: between the two sources
        out.read_i32(29_520, 1, &mut o).unwrap();
        s.read_i32(29_520, 1, &mut x).unwrap();
        let mut y = Vec::new();
        s.read_i32(50_000, 1, &mut y).unwrap();
        let w = 0.5 / 480.0;
        let expect = (x[0] as f64 * (1.0 - w) + y[0] as f64 * w).round() as i32;
        assert_eq!(o[0], expect);
    }

    #[test]
    fn flac_with_several_cuts_decodes() {
        let src = make_src("src_cut2.wav", 300_000, 2);
        let dst = tmp("cut2.flac");
        let segs = vec![(1000, 50_000), (60_000, 61_000), (120_000, 299_000)];
        let job = ExportJob {
            src: &src,
            dst: &dst,
            format: ExportFormat::Flac,
            segments: segs.clone(),
            fade_ms: 10,
            crossfade_ms: 10,
            tags: vec![],
            opts: ExportOptions::default(),
        };
        export(&job, &AtomicU32::new(0), &AtomicBool::new(false)).unwrap();
        let mut fr = claxon::FlacReader::open(&dst).unwrap();
        assert_eq!(fr.streaminfo().samples, Some(output_frames(&segs, 480)));
        assert_eq!(fr.samples().count() as u64, output_frames(&segs, 480) * 2);
    }

    #[test]
    fn mp3_export_has_right_length() {
        let src = make_src("src_mp3.wav", 48_000 * 10, 2);
        let dst = tmp("out.mp3");
        let job = ExportJob {
            src: &src,
            dst: &dst,
            format: ExportFormat::Mp3,
            segments: vec![(0, 48_000 * 10)],
            fade_ms: 10,
            crossfade_ms: 10,
            tags: vec![("TITLE".into(), "Café test".into()), ("ARTIST".into(), "De Blik".into())],
            opts: ExportOptions::default(),
        };
        export(&job, &AtomicU32::new(0), &AtomicBool::new(false)).unwrap();
        let size = std::fs::metadata(&dst).unwrap().len();
        // 10 s at 320 kbps = 400 kB (+ tag/padding)
        assert!((380_000..450_000).contains(&size), "size {size}");
        let bytes = std::fs::read(&dst).unwrap();
        assert_eq!(&bytes[0..3], b"ID3");
    }

    /// Write a test "tape": 40 s soft music, 5 s hiss, 40 s loud music with peaks.
    fn make_tape(name: &str) -> PathBuf {
        let p = tmp(name);
        let mut w = WavWriter::create(&p, 48_000, 2, 24).unwrap();
        let mut v = Vec::new();
        let mut seed = 3u32;
        let mut noise = || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 8) as f32 / 16_777_216.0 - 0.5
        };
        for i in 0..48_000 * 85usize {
            let t = i as f32 / 48_000.0;
            let music = (t * 2.0 * std::f32::consts::PI * 330.0).sin() * 0.6 + (t * 2.0 * std::f32::consts::PI * 880.0).sin() * 0.3;
            let x = if t < 40.0 {
                music * 0.03 + noise() * 0.002 // soft (about −34 LUFS)
            } else if t < 45.0 {
                noise() * 0.002 // hiss between the songs
            } else {
                // loud, with short peaks well over full scale after +gain
                music * 0.5 * if (i / 12_000) % 9 == 4 { 1.8 } else { 1.0 } + noise() * 0.002
            };
            v.push(x);
            v.push(x * 0.9);
        }
        w.write_f32(&v).unwrap();
        w.finalize().unwrap();
        p
    }

    #[test]
    fn leveled_export_evens_out_and_never_clips() {
        use crate::level::*;
        let src = make_tape("tape.wav");
        let mut r = WavReader::open(&src).unwrap();
        let an = analyze(&mut r, &AtomicU32::new(0), &AtomicBool::new(false)).unwrap();
        let params = LevelParams::default();
        let curve = std::sync::Arc::new(GainCurve::compute(&an, &params));
        let lim = LimiterParams::default();
        let dst = tmp("tape_out.wav");
        let segs = vec![(48_000, 84 * 48_000)];
        let job = ExportJob {
            src: &src,
            dst: &dst,
            format: ExportFormat::Wav,
            segments: segs.clone(),
            fade_ms: 10,
            crossfade_ms: 10,
            tags: vec![],
            opts: ExportOptions { level: Some(std::sync::Arc::new(LevelPlan { curve: Some(curve), limiter: Some(lim) })), ..Default::default() },
        };
        export(&job, &AtomicU32::new(0), &AtomicBool::new(false)).unwrap();
        let mut out = WavReader::open(&dst).unwrap();
        assert_eq!(out.frames, output_frames(&segs, 480), "length unchanged");
        let mut all = Vec::new();
        out.read_f32(0, out.frames as usize, &mut all).unwrap();
        let peak = all.iter().fold(0f32, |m, &x| m.max(x.abs()));
        assert!(peak <= 10f32.powf(-1.0 / 20.0) + 1e-4, "peak {peak}");
        // loudness of soft and loud part after leveling: both near the target
        let part = |a: f32, b: f32| {
            let mut an = Analyzer::new(48_000, 2);
            an.push(&all[(a * 48_000.0) as usize * 2..(b * 48_000.0) as usize * 2]);
            an.finish().integrated()
        };
        let (soft, loud) = (part(10.0, 30.0), part(55.0, 75.0));
        // the same parts of the original (output starts 1 s into the source)
        let mut orig = Vec::new();
        WavReader::open(&src).unwrap().read_f32(0, 85 * 48_000, &mut orig).unwrap();
        let before = |a: f32, b: f32| {
            let mut an = Analyzer::new(48_000, 2);
            an.push(&orig[((a + 1.0) * 48_000.0) as usize * 2..((b + 1.0) * 48_000.0) as usize * 2]);
            an.finish().integrated()
        };
        let (soft0, loud0) = (before(10.0, 30.0), before(55.0, 75.0));
        eprintln!("soft {soft0:.1} -> {soft:.1} LUFS, loud {loud0:.1} -> {loud:.1} LUFS");
        assert!((soft - soft0 - 12.0).abs() < 1.0, "soft part lifted by the maximum of 12 dB: {soft0} -> {soft}");
        assert!((loud - (-16.0)).abs() < 1.5, "loud part near the target: {loud}");
        assert!((soft0 - loud0).abs() - (soft - loud).abs() > 10.0, "difference evened out");
        // hiss between the songs is not lifted above the soft part's gain (+12 dB max)
        let hiss = part(40.5, 43.5);
        assert!(hiss < -50.0, "hiss stays down: {hiss}");
    }

    #[test]
    fn mono_export_halves_channels() {
        let src = make_src("src_mono.wav", 48_000, 2);
        let dst = tmp("mono.wav");
        let job = ExportJob {
            src: &src,
            dst: &dst,
            format: ExportFormat::Wav,
            segments: vec![(0, 48_000)],
            fade_ms: 0,
            crossfade_ms: 10,
            tags: vec![],
            opts: ExportOptions { mono: true, ..Default::default() },
        };
        export(&job, &AtomicU32::new(0), &AtomicBool::new(false)).unwrap();
        let mut out = WavReader::open(&dst).unwrap();
        assert_eq!(out.fmt.channels, 1);
        assert_eq!(out.frames, 48_000);
        let (mut o, mut s) = (Vec::new(), Vec::new());
        out.read_i32(1000, 1, &mut o).unwrap();
        WavReader::open(&src).unwrap().read_i32(1000, 1, &mut s).unwrap();
        assert_eq!(o[0], ((s[0] as i64 + s[1] as i64) / 2) as i32);
    }

    /// cargo test --release bench_mp3 -- --ignored --nocapture
    #[test]
    #[ignore]
    fn bench_mp3() {
        let secs = 180;
        let src = make_src("bench_src.wav", 48_000 * secs, 2);
        for (kbps, q) in [(320, 0u8), (320, 2), (192, 0), (192, 2), (192, 3)] {
            let dst = tmp(&format!("bench_{kbps}_{q}.mp3"));
            let job = ExportJob {
                src: &src,
                dst: &dst,
                format: ExportFormat::Mp3,
                segments: vec![(0, 48_000 * secs as u64)],
                fade_ms: 0,
                crossfade_ms: 10,
                tags: vec![],
                opts: ExportOptions { mp3_kbps: kbps, mp3_quality: Some(q), ..Default::default() },
            };
            let t = std::time::Instant::now();
            export(&job, &AtomicU32::new(0), &AtomicBool::new(false)).unwrap();
            let el = t.elapsed().as_secs_f64();
            eprintln!("MP3 {kbps}k q{q}: {el:.2} s voor {secs} s audio = {:.0}x realtime, {} kB", secs as f64 / el, std::fs::metadata(&dst).unwrap().len() / 1000);
        }
    }

    #[test]
    fn wav_export_with_fade() {
        let src = make_src("src_wav.wav", 48_000, 1);
        let dst = tmp("out.wav");
        let job = ExportJob {
            src: &src,
            dst: &dst,
            format: ExportFormat::Wav,
            segments: vec![(1000, 41_000)],
            fade_ms: 10,
            crossfade_ms: 10,
            tags: vec![],
            opts: ExportOptions::default(),
        };
        export(&job, &AtomicU32::new(0), &AtomicBool::new(false)).unwrap();
        let mut r = WavReader::open(&dst).unwrap();
        assert_eq!(r.frames, 40_000);
        let mut v = Vec::new();
        r.read_i32(0, 40_000, &mut v).unwrap();
        assert_eq!(v[0], 0);
        assert_eq!(v[39_999], 0);
        let mut s = WavReader::open(&src).unwrap();
        let mut o = Vec::new();
        s.read_i32(1000 + 20_000, 1, &mut o).unwrap();
        assert_eq!(v[20_000], o[0]);
    }
}
