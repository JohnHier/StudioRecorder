//! Opening MP3 and FLAC: decoded once to a WAV working copy in the temp
//! folder, so the editor (waveform, cutting, playback, export) works the same
//! as for WAV. The original file is never changed.

use crate::wav::WavWriter;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

pub fn extension(p: &Path) -> String {
    p.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase()
}

pub fn needs_decoding(p: &Path) -> bool {
    matches!(extension(p).as_str(), "mp3" | "flac")
}

fn cache_dir() -> PathBuf {
    std::env::temp_dir().join("StudioRecorder")
}

/// Remove working copies older than a week.
pub fn clean_cache() {
    let Ok(rd) = std::fs::read_dir(cache_dir()) else { return };
    let week = std::time::Duration::from_secs(7 * 24 * 3600);
    for e in rd.flatten() {
        let old = e.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age > week);
        if old {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// Decode `src` to a WAV working copy (reused when the file is unchanged).
/// `progress` receives permille.
pub fn working_copy(src: &Path, progress: &AtomicU32) -> Result<PathBuf, String> {
    let meta = std::fs::metadata(src).map_err(|e| format!("Kan bestand niet openen: {e}"))?;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    src.hash(&mut h);
    meta.len().hash(&mut h);
    meta.modified().ok().hash(&mut h);
    let stem = src.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "audio".into());
    let stem: String = stem.chars().take(60).collect();
    let dir = cache_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("Kan tijdelijke map niet maken: {e}"))?;
    let out = dir.join(format!("{stem}_{:016x}.wav", h.finish()));
    if out.exists() {
        progress.store(1000, Ordering::Relaxed);
        return Ok(out);
    }
    let part = out.with_extension("wav.part");
    match decode_to_wav(src, &part, progress) {
        Ok(()) => {
            std::fs::rename(&part, &out).map_err(|e| e.to_string())?;
            Ok(out)
        }
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            Err(e)
        }
    }
}

fn decode_to_wav(src: &Path, dst: &Path, progress: &AtomicU32) -> Result<(), String> {
    let ext = extension(src);
    let file = std::fs::File::open(src).map_err(|e| format!("Kan bestand niet openen: {e}"))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    hint.with_extension(&ext);
    let fmt_opts = FormatOptions { enable_gapless: true, ..Default::default() };
    let probed = symphonia::default::get_probe()
        .format(&hint, mss, &fmt_opts, &MetadataOptions::default())
        .map_err(|e| format!("Bestandsformaat niet herkend: {e}"))?;
    let mut format = probed.format;
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != symphonia::core::codecs::CODEC_TYPE_NULL)
        .ok_or("Geen audiospoor gevonden")?;
    let track_id = track.id;
    let params = track.codec_params.clone();
    let rate = params.sample_rate.ok_or("Samplerate onbekend")?;
    let src_ch = params.channels.map(|c| c.count()).unwrap_or(2).max(1);
    let out_ch = src_ch.min(2);
    // FLAC keeps its own depth (lossless); MP3 is decoded at 24 bit.
    let out_bits: u16 = match (ext.as_str(), params.bits_per_sample) {
        ("flac", Some(b)) if b <= 16 => 16,
        _ => 24,
    };
    let total = params.n_frames.unwrap_or(0);
    let mut decoder = symphonia::default::get_codecs()
        .make(&params, &DecoderOptions::default())
        .map_err(|e| format!("Kan dit bestand niet decoderen: {e}"))?;
    let mut w = WavWriter::create(dst, rate, out_ch as u16, out_bits).map_err(|e| format!("Kan werkkopie niet maken: {e}"))?;
    let shift = 32 - out_bits as u32;
    let mut sb: Option<SampleBuffer<i32>> = None;
    let mut out = Vec::new();
    let mut done: u64 = 0;
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(SymError::ResetRequired) => break,
            Err(e) => return Err(format!("Leesfout: {e}")),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let buf = match decoder.decode(&packet) {
            Ok(b) => b,
            // A damaged MP3 frame: skip it, like every player does.
            Err(SymError::DecodeError(_)) => continue,
            Err(e) => return Err(format!("Decodeerfout: {e}")),
        };
        let spec = *buf.spec();
        let need = buf.capacity() as u64;
        if sb.as_ref().is_none_or(|b| b.capacity() < need as usize * spec.channels.count()) {
            sb = Some(SampleBuffer::<i32>::new(need, spec));
        }
        let b = sb.as_mut().unwrap();
        b.copy_interleaved_ref(buf);
        let n = spec.channels.count().max(1);
        out.clear();
        for f in b.samples().chunks_exact(n) {
            for &s in f.iter().take(out_ch) {
                out.push(s >> shift);
            }
        }
        w.write_i32(&out).map_err(|e| format!("Schrijffout: {e}"))?;
        done += (out.len() / out_ch) as u64;
        if total > 0 {
            progress.store((done * 1000 / total).min(999) as u32, Ordering::Relaxed);
        }
    }
    if done == 0 {
        return Err("Dit bestand bevat geen audio".into());
    }
    w.finalize().map_err(|e| format!("Schrijffout: {e}"))?;
    progress.store(1000, Ordering::Relaxed);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::export::{ExportFormat, ExportJob, export};
    use crate::wav::WavReader;
    use std::sync::atomic::AtomicBool;

    fn src_wav(name: &str, bits: u16, frames: usize) -> PathBuf {
        let d = std::env::temp_dir().join("studiorec_tests");
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join(name);
        let mut w = WavWriter::create(&p, 48000, 2, bits).unwrap();
        let v: Vec<f32> = (0..frames * 2).map(|i| ((i / 2) as f32 * 0.013 + (i % 2) as f32).sin() * 0.7).collect();
        w.write_f32(&v).unwrap();
        w.finalize().unwrap();
        p
    }

    fn exported(src: &Path, fmt: ExportFormat) -> PathBuf {
        let dst = src.with_extension(fmt.ext());
        let mut r = WavReader::open(src).unwrap();
        let job = ExportJob { src, dst: &dst, format: fmt, segments: vec![(0, r.frames)], fade_ms: 0, crossfade_ms: 10, tags: vec![] };
        export(&job, &AtomicU32::new(0), &AtomicBool::new(false)).unwrap();
        let _ = r.read_f32(0, 1, &mut Vec::new());
        dst
    }

    #[test]
    fn flac_opens_bit_exact() {
        for bits in [16u16, 24] {
            let wav = src_wav(&format!("dec_src{bits}.wav"), bits, 100_000);
            let flac = exported(&wav, ExportFormat::Flac);
            let copy = working_copy(&flac, &AtomicU32::new(0)).unwrap();
            let mut a = WavReader::open(&wav).unwrap();
            let mut b = WavReader::open(&copy).unwrap();
            assert_eq!(b.fmt.bits, bits);
            assert_eq!(a.frames, b.frames);
            let (mut x, mut y) = (Vec::new(), Vec::new());
            a.read_i32(0, 100_000, &mut x).unwrap();
            b.read_i32(0, 100_000, &mut y).unwrap();
            assert!(x == y, "{bits}-bit FLAC not identical after decoding");
            let _ = std::fs::remove_file(copy);
        }
    }

    #[test]
    fn mp3_opens_with_right_length() {
        let wav = src_wav("dec_mp3src.wav", 24, 48_000 * 6);
        let mp3 = exported(&wav, ExportFormat::Mp3);
        let copy = working_copy(&mp3, &AtomicU32::new(0)).unwrap();
        let mut b = WavReader::open(&copy).unwrap();
        assert_eq!(b.fmt.sample_rate, 48000);
        assert_eq!(b.fmt.channels, 2);
        // gapless decoding: within one MP3 frame of the original length
        let diff = (b.frames as i64 - 48_000 * 6).abs();
        assert!(diff <= 1152, "length differs by {diff} frames");
        let mut v = Vec::new();
        b.read_f32(48_000, 4800, &mut v).unwrap();
        let peak = v.iter().fold(0f32, |m, s| m.max(s.abs()));
        assert!(peak > 0.5 && peak < 0.8, "peak {peak}");
        // second open reuses the working copy
        let again = working_copy(&mp3, &AtomicU32::new(0)).unwrap();
        assert_eq!(again, copy);
        let _ = std::fs::remove_file(copy);
    }
}
