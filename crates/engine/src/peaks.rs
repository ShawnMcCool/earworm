use crate::buffer::{SongBuffer, CHANNELS};
use serde::{Deserialize, Serialize};

pub const FRAMES_PER_BUCKET: usize = 1024;

/// Per-bucket (min, max) over both channels — what the waveform draws.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Peaks {
    pub frames_per_bucket: usize,
    pub buckets: Vec<(f32, f32)>,
}

pub fn compute_peaks(buf: &SongBuffer) -> Peaks {
    let buckets = buf
        .data
        .chunks(FRAMES_PER_BUCKET * CHANNELS)
        .map(|chunk| {
            chunk
                .iter()
                .fold((f32::MAX, f32::MIN), |(lo, hi), s| (lo.min(*s), hi.max(*s)))
        })
        .collect();
    Peaks {
        frames_per_bucket: FRAMES_PER_BUCKET,
        buckets,
    }
}

/// Binary cache format magic. The cache is a packed little-endian blob, not
/// JSON — encoding/parsing thousands of float pairs as text was ~3x larger and
/// far slower than a flat `[f32]` dump. Bump the tag (and the file extension) to
/// invalidate older caches.
const MAGIC: &[u8; 4] = b"EWP1";

fn cache_path(file_hash: &str) -> Option<std::path::PathBuf> {
    Some(
        dirs::cache_dir()?
            .join("dredge/peaks")
            .join(format!("{file_hash}.peaks")),
    )
}

fn encode(p: &Peaks) -> Vec<u8> {
    let mut out = Vec::with_capacity(12 + p.buckets.len() * 8);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&(p.frames_per_bucket as u32).to_le_bytes());
    out.extend_from_slice(&(p.buckets.len() as u32).to_le_bytes());
    for &(lo, hi) in &p.buckets {
        out.extend_from_slice(&lo.to_le_bytes());
        out.extend_from_slice(&hi.to_le_bytes());
    }
    out
}

fn decode(bytes: &[u8]) -> Option<Peaks> {
    if bytes.len() < 12 || &bytes[..4] != MAGIC {
        return None;
    }
    let frames_per_bucket = u32::from_le_bytes(bytes[4..8].try_into().ok()?) as usize;
    let count = u32::from_le_bytes(bytes[8..12].try_into().ok()?) as usize;
    let payload = &bytes[12..];
    if payload.len() != count * 8 {
        return None;
    }
    let buckets = payload
        .as_chunks::<8>()
        .0
        .iter()
        .map(|c| {
            let lo = f32::from_le_bytes(c[0..4].try_into().unwrap());
            let hi = f32::from_le_bytes(c[4..8].try_into().unwrap());
            (lo, hi)
        })
        .collect();
    Some(Peaks {
        frames_per_bucket,
        buckets,
    })
}

/// Delete the cached peaks for a song hash. A missing cache (or no cache dir)
/// is a no-op — deletion cleanup must not fail on an absent file.
pub fn remove_cache(file_hash: &str) -> std::io::Result<()> {
    let Some(path) = cache_path(file_hash) else {
        return Ok(());
    };
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Cache under ~/.cache/dredge/peaks/<file_hash>.peaks; load if present.
pub fn load_or_compute(buf: &SongBuffer, file_hash: &str) -> std::io::Result<Peaks> {
    let path = cache_path(file_hash).ok_or_else(|| std::io::Error::other("no cache dir"))?;
    if let Ok(bytes) = std::fs::read(&path) {
        if let Some(peaks) = decode(&bytes) {
            return Ok(peaks);
        }
        // unreadable / stale format → fall through and recompute
    }
    let peaks = compute_peaks(buf);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, encode(&peaks))?;
    Ok(peaks)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peaks_capture_min_and_max_per_bucket() {
        // bucket 0: constant 0.5; bucket 1: constant -0.25
        let mut data = vec![0.5f32; FRAMES_PER_BUCKET * CHANNELS];
        data.extend(vec![-0.25f32; FRAMES_PER_BUCKET * CHANNELS]);
        let p = compute_peaks(&SongBuffer { data });
        assert_eq!(p.buckets.len(), 2);
        assert_eq!(p.buckets[0], (0.5, 0.5));
        assert_eq!(p.buckets[1], (-0.25, -0.25));
    }

    #[test]
    fn partial_final_bucket_included() {
        let data = vec![0.1f32; (FRAMES_PER_BUCKET + 10) * CHANNELS];
        let p = compute_peaks(&SongBuffer { data });
        assert_eq!(p.buckets.len(), 2);
    }

    #[test]
    fn cache_roundtrip() {
        let data = vec![0.3f32; FRAMES_PER_BUCKET * CHANNELS];
        let buf = SongBuffer { data };
        let hash = format!("test-{}", std::process::id());
        let first = load_or_compute(&buf, &hash).unwrap();
        // second call must hit the cache file (delete buf data influence: pass empty buffer)
        let cached = load_or_compute(&SongBuffer { data: vec![] }, &hash).unwrap();
        assert_eq!(first, cached);
        // cleanup
        let _ = std::fs::remove_file(cache_path(&hash).unwrap());
    }

    #[test]
    fn binary_encode_decode_roundtrip() {
        let p = Peaks {
            frames_per_bucket: FRAMES_PER_BUCKET,
            buckets: vec![(-1.0, 1.0), (0.0, 0.5), (-0.25, -0.1)],
        };
        assert_eq!(decode(&encode(&p)), Some(p));
        assert_eq!(decode(b"not a peaks blob"), None);
        assert_eq!(decode(&[]), None);
    }

    #[test]
    fn remove_cache_deletes_then_noops() {
        let buf = SongBuffer {
            data: vec![0.2f32; FRAMES_PER_BUCKET * CHANNELS],
        };
        let hash = format!("rm-{}", std::process::id());
        load_or_compute(&buf, &hash).unwrap();
        let path = cache_path(&hash).unwrap();
        assert!(path.exists());

        remove_cache(&hash).unwrap();
        assert!(!path.exists());

        // second remove on the missing file is a clean no-op
        remove_cache(&hash).unwrap();
    }
}
