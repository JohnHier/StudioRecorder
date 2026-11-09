//! WAV reading and crash-safe WAV writing.
//!
//! The writer always produces integer PCM. It reserves a 28-byte `JUNK`
//! chunk right after the RIFF header, so that a recording that grows past
//! 4 GB can be turned into an RF64 file in place (the EBU Tech 3306 approach)
//! without rewriting any audio. The header is refreshed regularly while
//! recording, so a crash or power cut only loses the last moments.

use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

pub const HEADER_LEN: u64 = 80;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WavFormat {
    pub sample_rate: u32,
    pub channels: u16,
    pub bits: u16,
    pub float: bool,
}

impl WavFormat {
    pub fn bytes_per_sample(&self) -> usize {
        (self.bits as usize).div_ceil(8)
    }
    pub fn block_align(&self) -> usize {
        self.bytes_per_sample() * self.channels as usize
    }
}

// ---------------------------------------------------------------------------
// Writer
// ---------------------------------------------------------------------------

pub struct WavWriter {
    out: BufWriter<File>,
    path: PathBuf,
    fmt: WavFormat,
    data_bytes: u64,
    is_rf64: bool,
    scratch: Vec<u8>,
}

impl WavWriter {
    /// Create a new integer-PCM WAV file (16 or 24 bit).
    pub fn create(path: &Path, sample_rate: u32, channels: u16, bits: u16) -> io::Result<Self> {
        assert!(bits == 16 || bits == 24, "only 16/24 bit supported");
        let file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .read(true)
            .open(path)?;
        let fmt = WavFormat { sample_rate, channels, bits, float: false };
        let mut w = WavWriter {
            out: BufWriter::with_capacity(1 << 18, file),
            path: path.to_path_buf(),
            fmt,
            data_bytes: 0,
            is_rf64: false,
            scratch: Vec::new(),
        };
        let header = w.header_bytes();
        w.out.write_all(&header)?;
        w.out.flush()?;
        Ok(w)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn frames_written(&self) -> u64 {
        self.data_bytes / self.fmt.block_align() as u64
    }

    /// Write interleaved float samples (-1.0..1.0); they are clipped and
    /// quantised to the file's bit depth.
    pub fn write_f32(&mut self, samples: &[f32]) -> io::Result<()> {
        self.scratch.clear();
        match self.fmt.bits {
            24 => {
                self.scratch.reserve(samples.len() * 3);
                for &s in samples {
                    let v = f32_to_i24(s);
                    let b = v.to_le_bytes();
                    self.scratch.extend_from_slice(&b[0..3]);
                }
            }
            _ => {
                self.scratch.reserve(samples.len() * 2);
                for &s in samples {
                    let v = f32_to_i16(s);
                    self.scratch.extend_from_slice(&v.to_le_bytes());
                }
            }
        }
        self.out.write_all(&self.scratch)?;
        self.data_bytes += self.scratch.len() as u64;
        Ok(())
    }

    /// Write interleaved integer samples that already have the file's bit depth.
    pub fn write_i32(&mut self, samples: &[i32]) -> io::Result<()> {
        self.scratch.clear();
        let bps = self.fmt.bytes_per_sample();
        self.scratch.reserve(samples.len() * bps);
        for &v in samples {
            let b = v.to_le_bytes();
            self.scratch.extend_from_slice(&b[0..bps]);
        }
        self.out.write_all(&self.scratch)?;
        self.data_bytes += self.scratch.len() as u64;
        Ok(())
    }

    fn header_bytes(&self) -> Vec<u8> {
        let mut h = Vec::with_capacity(HEADER_LEN as usize);
        let data = self.data_bytes;
        let pad = data & 1;
        let riff_size = 4 + (8 + 28) + (8 + 16) + 8 + data + pad;
        let rf64 = self.is_rf64;
        h.extend_from_slice(if rf64 { b"RF64" } else { b"RIFF" });
        h.extend_from_slice(&(if rf64 { u32::MAX } else { riff_size as u32 }).to_le_bytes());
        h.extend_from_slice(b"WAVE");
        if rf64 {
            h.extend_from_slice(b"ds64");
            h.extend_from_slice(&28u32.to_le_bytes());
            h.extend_from_slice(&riff_size.to_le_bytes());
            h.extend_from_slice(&data.to_le_bytes());
            h.extend_from_slice(&(data / self.fmt.block_align() as u64).to_le_bytes());
            h.extend_from_slice(&0u32.to_le_bytes());
        } else {
            h.extend_from_slice(b"JUNK");
            h.extend_from_slice(&28u32.to_le_bytes());
            h.extend_from_slice(&[0u8; 28]);
        }
        let f = self.fmt;
        h.extend_from_slice(b"fmt ");
        h.extend_from_slice(&16u32.to_le_bytes());
        h.extend_from_slice(&1u16.to_le_bytes()); // PCM
        h.extend_from_slice(&f.channels.to_le_bytes());
        h.extend_from_slice(&f.sample_rate.to_le_bytes());
        h.extend_from_slice(&(f.sample_rate * f.block_align() as u32).to_le_bytes());
        h.extend_from_slice(&(f.block_align() as u16).to_le_bytes());
        h.extend_from_slice(&f.bits.to_le_bytes());
        h.extend_from_slice(b"data");
        h.extend_from_slice(&(if rf64 { u32::MAX } else { data as u32 }).to_le_bytes());
        debug_assert_eq!(h.len() as u64, HEADER_LEN);
        h
    }

    /// Flush buffered audio and rewrite the header so the file on disk is a
    /// valid WAV file of everything recorded so far.
    pub fn refresh_header(&mut self, sync_to_disk: bool) -> io::Result<()> {
        self.out.flush()?;
        // Switch to RF64 well before the 32-bit sizes would overflow.
        if !self.is_rf64 && self.data_bytes + HEADER_LEN + 1 >= u32::MAX as u64 - (1 << 20) {
            self.is_rf64 = true;
        }
        let header = self.header_bytes();
        let file = self.out.get_mut();
        let end = file.stream_position()?;
        file.seek(SeekFrom::Start(0))?;
        file.write_all(&header)?;
        file.seek(SeekFrom::Start(end))?;
        if sync_to_disk {
            file.sync_data()?;
        }
        Ok(())
    }

    /// Finish the file (pad byte + final header).
    pub fn finalize(mut self) -> io::Result<u64> {
        if self.data_bytes & 1 == 1 {
            self.out.write_all(&[0])?;
        }
        self.refresh_header(true)?;
        Ok(self.frames_written())
    }
}

#[inline]
pub fn f32_to_i24(s: f32) -> i32 {
    let v = (s.clamp(-1.0, 1.0) * 8_388_607.0).round();
    v as i32
}

#[inline]
pub fn f32_to_i16(s: f32) -> i16 {
    (s.clamp(-1.0, 1.0) * 32_767.0).round() as i16
}

// ---------------------------------------------------------------------------
// Reader
// ---------------------------------------------------------------------------

pub struct WavReader {
    file: File,
    pub fmt: WavFormat,
    data_offset: u64,
    pub frames: u64,
    raw: Vec<u8>,
}

fn bad(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_string())
}

impl WavReader {
    pub fn open(path: &Path) -> io::Result<Self> {
        let mut file = File::open(path)?;
        let file_len = file.metadata()?.len();
        let mut hdr = [0u8; 12];
        file.read_exact(&mut hdr)?;
        let kind = &hdr[0..4];
        if (kind != b"RIFF" && kind != b"RF64" && kind != b"BW64") || &hdr[8..12] != b"WAVE" {
            return Err(bad("Geen WAV-bestand"));
        }
        let mut pos = 12u64;
        let mut fmt: Option<WavFormat> = None;
        let mut ds64_data: Option<u64> = None;
        let mut data: Option<(u64, u64)> = None;
        let mut own_layout = false;
        while pos + 8 <= file_len {
            file.seek(SeekFrom::Start(pos))?;
            let mut ch = [0u8; 8];
            file.read_exact(&mut ch)?;
            let id = [ch[0], ch[1], ch[2], ch[3]];
            let size = u32::from_le_bytes([ch[4], ch[5], ch[6], ch[7]]) as u64;
            let body = pos + 8;
            match &id {
                b"fmt " => {
                    let mut b = vec![0u8; size.min(64) as usize];
                    file.read_exact(&mut b)?;
                    if b.len() < 16 {
                        return Err(bad("Ongeldig fmt-blok"));
                    }
                    let mut tag = u16::from_le_bytes([b[0], b[1]]);
                    let channels = u16::from_le_bytes([b[2], b[3]]);
                    let sample_rate = u32::from_le_bytes([b[4], b[5], b[6], b[7]]);
                    let bits = u16::from_le_bytes([b[14], b[15]]);
                    if tag == 0xFFFE && b.len() >= 26 {
                        tag = u16::from_le_bytes([b[24], b[25]]);
                    }
                    let float = match tag {
                        1 => false,
                        3 => true,
                        _ => return Err(bad("Niet-ondersteund WAV-formaat (alleen PCM)")),
                    };
                    if channels == 0 || sample_rate == 0 || !matches!(bits, 8 | 16 | 24 | 32 | 64) {
                        return Err(bad("Ongeldig WAV-formaat"));
                    }
                    fmt = Some(WavFormat { sample_rate, channels, bits, float });
                }
                b"JUNK" if pos == 12 && size == 28 => own_layout = true,
                b"ds64" => {
                    let mut b = [0u8; 16];
                    file.read_exact(&mut b)?;
                    ds64_data = Some(u64::from_le_bytes(b[8..16].try_into().unwrap()));
                }
                b"data" => {
                    let mut len = size;
                    if size == u32::MAX as u64 {
                        if let Some(d) = ds64_data {
                            len = d;
                        }
                    }
                    let avail = file_len.saturating_sub(body);
                    // A crashed recording may have a stale size: trust the file length.
                    if len == 0 || len > avail || size == u32::MAX as u64 && ds64_data.is_none() {
                        len = avail;
                    } else if own_layout && kind == b"RIFF" && pos + 8 == HEADER_LEN {
                        // Written by our own recorder: the header may lag behind
                        // the audio after a crash, so use everything on disk.
                        len = avail;
                    }
                    data = Some((body, len));
                    break;
                }
                _ => {}
            }
            pos = body + size + (size & 1);
        }
        let fmt = fmt.ok_or_else(|| bad("fmt-blok ontbreekt"))?;
        let (data_offset, data_len) = data.ok_or_else(|| bad("data-blok ontbreekt"))?;
        let frames = data_len / fmt.block_align() as u64;
        Ok(WavReader { file, fmt, data_offset, frames, raw: Vec::new() })
    }

    fn read_raw(&mut self, start: u64, count: usize) -> io::Result<usize> {
        let start = start.min(self.frames);
        let count = count.min((self.frames - start) as usize);
        let ba = self.fmt.block_align();
        self.raw.resize(count * ba, 0);
        self.file.seek(SeekFrom::Start(self.data_offset + start * ba as u64))?;
        let mut got = 0;
        while got < self.raw.len() {
            let n = self.file.read(&mut self.raw[got..])?;
            if n == 0 {
                break;
            }
            got += n;
        }
        let frames = got / ba;
        self.raw.truncate(frames * ba);
        Ok(frames)
    }

    /// Read `count` frames starting at `start` as interleaved f32.
    pub fn read_f32(&mut self, start: u64, count: usize, out: &mut Vec<f32>) -> io::Result<usize> {
        let frames = self.read_raw(start, count)?;
        out.clear();
        let f = self.fmt;
        let raw = &self.raw;
        out.reserve(frames * f.channels as usize);
        match (f.bits, f.float) {
            (8, false) => out.extend(raw.iter().map(|&b| (b as f32 - 128.0) / 128.0)),
            (16, false) => out.extend(raw.chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0)),
            (24, false) => out.extend(raw.chunks_exact(3).map(|c| i24_from(c) as f32 / 8_388_608.0)),
            (32, false) => out.extend(raw.chunks_exact(4).map(|c| i32::from_le_bytes([c[0], c[1], c[2], c[3]]) as f32 / 2_147_483_648.0)),
            (32, true) => out.extend(raw.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))),
            (64, true) => out.extend(raw.chunks_exact(8).map(|c| f64::from_le_bytes(c.try_into().unwrap()) as f32)),
            _ => return Err(bad("Niet-ondersteunde bitdiepte")),
        }
        Ok(frames)
    }

    /// Integer bit depth used when exporting this file losslessly.
    pub fn export_bits(&self) -> u16 {
        match (self.fmt.bits, self.fmt.float) {
            (8, false) | (16, false) => 16,
            _ => 24,
        }
    }

    /// Read frames as integers at [`export_bits`] depth (lossless for 16/24-bit sources).
    pub fn read_i32(&mut self, start: u64, count: usize, out: &mut Vec<i32>) -> io::Result<usize> {
        let f = self.fmt;
        if f.float || f.bits == 32 || f.bits == 64 {
            let mut tmp = Vec::new();
            let n = self.read_f32(start, count, &mut tmp)?;
            out.clear();
            out.extend(tmp.iter().map(|&s| f32_to_i24(s)));
            return Ok(n);
        }
        let frames = self.read_raw(start, count)?;
        out.clear();
        let raw = &self.raw;
        match f.bits {
            8 => out.extend(raw.iter().map(|&b| ((b as i32) - 128) << 8)),
            16 => out.extend(raw.chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]]) as i32)),
            24 => out.extend(raw.chunks_exact(3).map(i24_from)),
            _ => return Err(bad("Niet-ondersteunde bitdiepte")),
        }
        Ok(frames)
    }
}

#[inline]
fn i24_from(c: &[u8]) -> i32 {
    ((c[0] as i32) | ((c[1] as i32) << 8) | ((c[2] as i32) << 16)) << 8 >> 8
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join("studiorec_tests");
        std::fs::create_dir_all(&d).unwrap();
        d.join(name)
    }

    #[test]
    fn roundtrip_24bit_stereo() {
        let p = tmp("rt24.wav");
        let mut w = WavWriter::create(&p, 48000, 2, 24).unwrap();
        let mut samples = Vec::new();
        for i in 0..48000 {
            let s = (i as f32 * 0.01).sin() * 0.8;
            samples.push(s);
            samples.push(-s);
        }
        w.write_f32(&samples).unwrap();
        assert_eq!(w.finalize().unwrap(), 48000);
        let mut r = WavReader::open(&p).unwrap();
        assert_eq!(r.fmt, WavFormat { sample_rate: 48000, channels: 2, bits: 24, float: false });
        assert_eq!(r.frames, 48000);
        let mut out = Vec::new();
        r.read_f32(100, 10, &mut out).unwrap();
        assert_eq!(out.len(), 20);
        assert!((out[0] - samples[200]).abs() < 1e-6);
        let mut ints = Vec::new();
        r.read_i32(0, 48000, &mut ints).unwrap();
        assert_eq!(ints[2], f32_to_i24(samples[2]));
    }

    #[test]
    fn crashed_file_is_recovered() {
        let p = tmp("crash.wav");
        let mut w = WavWriter::create(&p, 48000, 1, 24).unwrap();
        w.write_f32(&vec![0.25; 1000]).unwrap();
        w.refresh_header(false).unwrap();
        // More audio after the last header refresh, then "crash" (no finalize).
        w.write_f32(&vec![0.5; 3_000_000]).unwrap();
        w.out.flush().unwrap();
        drop(w);
        let mut r = WavReader::open(&p).unwrap();
        assert_eq!(r.frames, 3_001_000);
        let mut out = Vec::new();
        r.read_f32(3_000_999, 5, &mut out).unwrap();
        assert_eq!(out.len(), 1);
        assert!((out[0] - 0.5).abs() < 1e-5);
    }

    #[test]
    fn rf64_header_is_readable() {
        let p = tmp("rf64.wav");
        let mut w = WavWriter::create(&p, 48000, 2, 24).unwrap();
        w.write_f32(&vec![0.3; 9600]).unwrap();
        w.is_rf64 = true; // what happens automatically after ~4 GB
        w.finalize().unwrap();
        let bytes = std::fs::read(&p).unwrap();
        assert_eq!(&bytes[0..4], b"RF64");
        assert_eq!(&bytes[12..16], b"ds64");
        let r = WavReader::open(&p).unwrap();
        assert_eq!(r.frames, 4800);
    }

    #[test]
    fn header_is_valid_for_other_tools() {
        let p = tmp("hdr.wav");
        let mut w = WavWriter::create(&p, 44100, 2, 16).unwrap();
        w.write_f32(&vec![0.1; 882]).unwrap();
        w.finalize().unwrap();
        let bytes = std::fs::read(&p).unwrap();
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize, bytes.len() - 8);
        assert_eq!(&bytes[76..80], &(882u32 * 2).to_le_bytes());
    }
}
