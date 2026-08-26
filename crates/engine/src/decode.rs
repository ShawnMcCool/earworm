use crate::buffer::{SongBuffer, CHANNELS, SAMPLE_RATE};
use crate::error::{Error, Result};
use rubato::{
    Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction,
};
use std::fs::File;
use std::io::Read;
use std::path::Path;
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

const RESAMPLE_CHUNK: usize = 1024;

/// Decode any supported file to the canonical in-memory format:
/// interleaved stereo f32 at 48 kHz.
///
/// Tries symphonia (pure-Rust) first. Symphonia's demuxers reject many
/// real-world containers — incomplete mp4 box parsing (e.g. `sl descriptor
/// predefined not mp4`), and no matroska/webm support — so on a demux/codec
/// failure we fall back to ffmpeg (if installed) to remux the audio to a WAV
/// and decode that. Missing files (`Io`) never trigger the fallback.
pub fn decode_file(path: &Path) -> Result<SongBuffer> {
    match decode_symphonia(path) {
        Ok(buf) => Ok(buf),
        Err(primary @ (Error::Decode(_) | Error::Unsupported(_))) => {
            decode_via_ffmpeg(path, &primary)
        }
        Err(e) => Err(e),
    }
}

/// Last resort for containers symphonia can't demux: let ffmpeg extract the
/// audio track to a canonical 48 kHz stereo WAV, then decode that WAV (which
/// symphonia reads reliably). `primary` is symphonia's original error, kept in
/// the message so the failure is legible when ffmpeg is absent too.
fn decode_via_ffmpeg(src: &Path, primary: &Error) -> Result<SongBuffer> {
    let tmp = tempfile::Builder::new()
        .prefix("dredge-ffmpeg-")
        .suffix(".wav")
        .tempfile()
        .map_err(|e| Error::Decode(format!("{primary}; ffmpeg temp file: {e}")))?;
    let output = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-y", "-i"])
        .arg(src)
        .args(["-vn", "-ac", "2", "-ar"])
        .arg(SAMPLE_RATE.to_string())
        .args(["-c:a", "pcm_s16le"])
        .arg(tmp.path())
        .output();
    let output = match output {
        Ok(o) => o,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(Error::Unsupported(format!(
                "{primary} — install ffmpeg to load this file"
            )));
        }
        Err(e) => {
            return Err(Error::Decode(format!(
                "{primary}; ffmpeg failed to start: {e}"
            )))
        }
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let tail = stderr.lines().last().unwrap_or("").trim();
        return Err(Error::Decode(format!("{primary}; ffmpeg: {tail}")));
    }
    decode_symphonia(tmp.path())
}

/// Pure-Rust decode via symphonia. See `decode_file` for the ffmpeg fallback.
fn decode_symphonia(path: &Path) -> Result<SongBuffer> {
    let file = File::open(path)?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|e| Error::Decode(e.to_string()))?;
    let mut format = probed.format;
    let codecs = symphonia::default::get_codecs();
    // Pick the first track the audio-codec registry can decode, NOT
    // `default_track()`: in a video container (mp4/mov) the default track is
    // usually the video track, which has no audio decoder. This skips it and
    // grabs the audio track — we only ever want a file's audio.
    let track = format
        .tracks()
        .iter()
        .find(|t| codecs.get_codec(t.codec_params.codec).is_some())
        .ok_or_else(|| Error::Decode("no decodable audio track".into()))?;
    let track_id = track.id;
    let mut decoder = codecs
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| Error::Decode(e.to_string()))?;

    let mut src_rate: Option<u32> = track.codec_params.sample_rate;
    let mut src_channels: Option<usize> = track.codec_params.channels.map(|c| c.count());
    let mut interleaved: Vec<f32> = Vec::new();
    // Pre-size from the declared frame count when known, so the decode loop
    // doesn't repeatedly grow/realloc a multi-MB buffer.
    if let (Some(nf), Some(c)) = (track.codec_params.n_frames, src_channels) {
        interleaved.reserve(nf as usize * c);
    }
    // Allocate the sample buffer once (on the first decoded frame) and reuse it
    // — per-packet allocation churned thousands of small Vecs across a song.
    let mut sample_buf: Option<SampleBuffer<f32>> = None;

    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(symphonia::core::errors::Error::IoError(e))
                if e.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break
            }
            Err(symphonia::core::errors::Error::ResetRequired) => break,
            Err(e) => return Err(Error::Decode(e.to_string())),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            // Skip over recoverable per-packet decode errors.
            Err(symphonia::core::errors::Error::DecodeError(_)) => continue,
            Err(e) => return Err(Error::Decode(e.to_string())),
        };
        let spec = *decoded.spec();
        src_rate.get_or_insert(spec.rate);
        src_channels.get_or_insert(spec.channels.count());
        let sb = sample_buf
            .get_or_insert_with(|| SampleBuffer::<f32>::new(decoded.capacity() as u64, spec));
        sb.copy_interleaved_ref(decoded);
        interleaved.extend_from_slice(sb.samples());
    }

    let ch = src_channels.ok_or_else(|| Error::Decode("unknown channel count".into()))?;
    let rate = src_rate.ok_or_else(|| Error::Decode("unknown sample rate".into()))?;
    if ch == 0 {
        return Err(Error::Unsupported("zero channels".into()));
    }

    let data = if rate == SAMPLE_RATE {
        // No resampling needed: downmix straight to the canonical interleaved
        // stereo buffer, skipping the planar split + re-interleave round-trip.
        to_stereo_interleaved(&interleaved, ch)
    } else {
        // rubato needs planar input; split, resample, then re-interleave.
        let (left, right) = to_stereo_planar(&interleaved, ch);
        let (left, right) = resample_stereo(&left, &right, rate)?;
        let mut data = Vec::with_capacity(left.len() * CHANNELS);
        for (l, r) in left.iter().zip(right.iter()) {
            data.push(*l);
            data.push(*r);
        }
        data
    };
    Ok(SongBuffer { data })
}

/// Downmix source-interleaved audio directly to interleaved stereo.
/// Mono → duplicate; stereo → passthrough; >2 ch → L = mean(even chans),
/// R = mean(odd chans). Mirrors `to_stereo_planar` but emits interleaved.
fn to_stereo_interleaved(interleaved: &[f32], ch: usize) -> Vec<f32> {
    let frames = interleaved.len() / ch;
    match ch {
        1 => {
            let mut out = Vec::with_capacity(frames * CHANNELS);
            for &s in &interleaved[..frames] {
                out.push(s);
                out.push(s);
            }
            out
        }
        2 => interleaved[..frames * CHANNELS].to_vec(),
        n => {
            let mut out = Vec::with_capacity(frames * CHANNELS);
            let evens = n.div_ceil(2) as f32;
            let odds = (n / 2).max(1) as f32;
            for fr in interleaved.chunks_exact(n) {
                let l: f32 = fr.iter().step_by(2).sum();
                let r: f32 = fr.iter().skip(1).step_by(2).sum();
                out.push(l / evens);
                out.push(r / odds);
            }
            out
        }
    }
}

/// Mono → duplicate; stereo → split; >2 ch → L = mean(even chans), R = mean(odd chans).
fn to_stereo_planar(interleaved: &[f32], ch: usize) -> (Vec<f32>, Vec<f32>) {
    let frames = interleaved.len() / ch;
    let mut left = Vec::with_capacity(frames);
    let mut right = Vec::with_capacity(frames);
    match ch {
        1 => {
            left.extend_from_slice(interleaved);
            right.extend_from_slice(interleaved);
        }
        2 => {
            for fr in interleaved.as_chunks::<2>().0 {
                left.push(fr[0]);
                right.push(fr[1]);
            }
        }
        n => {
            let evens = n.div_ceil(2) as f32;
            let odds = (n / 2) as f32;
            for fr in interleaved.chunks_exact(n) {
                let l: f32 = fr.iter().step_by(2).sum();
                let r: f32 = fr.iter().skip(1).step_by(2).sum();
                left.push(l / evens);
                right.push(r / odds.max(1.0));
            }
        }
    }
    (left, right)
}

fn resample_stereo(left: &[f32], right: &[f32], src_rate: u32) -> Result<(Vec<f32>, Vec<f32>)> {
    let params = SincInterpolationParameters {
        sinc_len: 256,
        f_cutoff: 0.95,
        interpolation: SincInterpolationType::Linear,
        oversampling_factor: 256,
        window: WindowFunction::BlackmanHarris2,
    };
    let ratio = SAMPLE_RATE as f64 / src_rate as f64;
    // f32 resampler: feed the decoded f32 samples directly, no f32→f64→f32
    // round-trip (the old f64 path allocated two full-length f64 copies of the
    // whole song — a large transient spike on every non-48k open).
    let mut resampler = SincFixedIn::<f32>::new(ratio, 2.0, params, RESAMPLE_CHUNK, 2)
        .map_err(|e| Error::Decode(format!("resampler init: {e}")))?;

    let mut out_l: Vec<f32> = Vec::with_capacity((left.len() as f64 * ratio) as usize + 1024);
    let mut out_r: Vec<f32> = Vec::with_capacity(out_l.capacity());

    let mut pos = 0;
    while pos + RESAMPLE_CHUNK <= left.len() {
        let chunk = [
            &left[pos..pos + RESAMPLE_CHUNK],
            &right[pos..pos + RESAMPLE_CHUNK],
        ];
        let out = resampler
            .process(&chunk, None)
            .map_err(|e| Error::Decode(format!("resample: {e}")))?;
        out_l.extend_from_slice(&out[0]);
        out_r.extend_from_slice(&out[1]);
        pos += RESAMPLE_CHUNK;
    }
    if pos < left.len() {
        let chunk = [&left[pos..], &right[pos..]];
        let out = resampler
            .process_partial(Some(&chunk), None)
            .map_err(|e| Error::Decode(format!("resample tail: {e}")))?;
        out_l.extend_from_slice(&out[0]);
        out_r.extend_from_slice(&out[1]);
    }
    // Drain the resampler's internal delay line.
    let out = resampler
        .process_partial::<&[f32]>(None, None)
        .map_err(|e| Error::Decode(format!("resample flush: {e}")))?;
    out_l.extend_from_slice(&out[0]);
    out_r.extend_from_slice(&out[1]);

    // Trim the leading delay and the zero-padded tail so the output length
    // matches the source duration. Copy the kept range out instead of
    // draining from the front (which memmoves the whole multi-MB buffer).
    let delay = resampler.output_delay();
    let expected = (left.len() as f64 * ratio).round() as usize;
    let n = out_l.len().min(out_r.len());
    let start = delay.min(n);
    let end = (delay + expected).min(n);
    Ok((out_l[start..end].to_vec(), out_r[start..end].to_vec()))
}

/// Decode `src` (any supported container, including MP4/MOV video files —
/// symphonia takes the default audio track and ignores video) to the canonical
/// 48 kHz stereo WAV at `dst`. This is how external tools (analysis, Demucs)
/// receive audio: symphonia is the single decode authority, so they read plain
/// PCM via libsndfile and never need ffmpeg.
pub fn decode_to_wav(src: &Path, dst: &Path) -> Result<()> {
    let buf = decode_file(src)?;
    crate::capture::write_wav(dst, &buf.data)
}

/// blake3 hash of the file contents (streaming, 1 MiB chunks), hex string.
pub fn file_hash(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}
