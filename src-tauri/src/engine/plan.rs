//! Chunk-plan math and Content-Range parsing (CONTRACT "start_download" /
//! sidecar semantics).

/// One download chunk; `end` is inclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkSpec {
    pub start: u64,
    pub end: u64,
}

impl ChunkSpec {
    pub fn len(&self) -> u64 {
        self.end - self.start + 1
    }
}

/// ceil(size / connections); 0 for a zero-size file or zero connections.
pub fn chunk_size(size: u64, connections: usize) -> u64 {
    if size == 0 || connections == 0 {
        return 0;
    }
    let n = connections as u64;
    (size + n - 1) / n
}

/// CONTRACT fresh plan: chunk i has start = i*chunkSize and
/// end = min((i+1)*chunkSize - 1, size - 1); chunks whose start would exceed
/// size-1 are dropped, so the last chunk is clamped and small files may yield
/// fewer chunks than connections.
pub fn build_chunks(size: u64, connections: usize) -> Vec<ChunkSpec> {
    if size == 0 || connections == 0 {
        return Vec::new();
    }
    let cs = chunk_size(size, connections);
    let mut out = Vec::new();
    let mut start = 0u64;
    while start < size {
        let end = std::cmp::min(start + cs - 1, size - 1);
        out.push(ChunkSpec { start, end });
        start = end + 1;
    }
    out
}

/// A parsed Content-Range header value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContentRange {
    pub start: u64,
    /// None when the server answers open-ended ("bytes S-*/T").
    pub end: Option<u64>,
    /// None for "bytes S-E/*" or a missing total.
    pub total: Option<u64>,
}

/// Parse "bytes S-E/T" (case-insensitive unit, T may be "*" or absent).
/// Returns None for anything malformed.
pub fn parse_content_range(value: &str) -> Option<ContentRange> {
    let v = value.trim();
    let lower = v.to_ascii_lowercase();
    if !lower.starts_with("bytes") {
        return None;
    }
    let rest = v[5..].trim_start();
    let (range_part, total_part) = match rest.find('/') {
        Some(i) => (&rest[..i], Some(rest[i + 1..].trim())),
        None => (rest, None),
    };
    let (s, e) = range_part.split_once('-')?;
    let start = s.trim().parse::<u64>().ok()?;
    let end_str = e.trim();
    let end = if end_str == "*" {
        None
    } else {
        Some(end_str.parse::<u64>().ok()?)
    };
    let total = match total_part {
        None => None,
        Some("*") => None,
        Some(t) => Some(t.parse::<u64>().ok()?),
    };
    Some(ContentRange { start, end, total })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_size_math() {
        assert_eq!(chunk_size(10, 3), 4);
        assert_eq!(chunk_size(9, 3), 3);
        assert_eq!(chunk_size(7, 16), 1);
        assert_eq!(chunk_size(0, 4), 0);
        assert_eq!(chunk_size(5, 0), 0);
        // CONTRACT example: 7,772,412,965 bytes / 4 connections.
        assert_eq!(chunk_size(7772412965, 4), 1943103242);
    }

    #[test]
    fn chunks_rounding_and_last_clamp() {
        let c = build_chunks(10, 3);
        assert_eq!(
            c,
            vec![
                ChunkSpec { start: 0, end: 3 },
                ChunkSpec { start: 4, end: 7 },
                ChunkSpec { start: 8, end: 9 },
            ]
        );
    }

    #[test]
    fn single_connection_covers_whole_file() {
        let c = build_chunks(123, 1);
        assert_eq!(c, vec![ChunkSpec { start: 0, end: 122 }]);
    }

    #[test]
    fn zero_size_has_no_chunks() {
        assert!(build_chunks(0, 4).is_empty());
    }

    #[test]
    fn small_file_fewer_chunks_than_connections() {
        let c = build_chunks(3, 16);
        assert_eq!(
            c,
            vec![
                ChunkSpec { start: 0, end: 0 },
                ChunkSpec { start: 1, end: 1 },
                ChunkSpec { start: 2, end: 2 },
            ]
        );
    }

    #[test]
    fn big_non_divisible_plan_is_contiguous_and_covers() {
        let c = build_chunks(7772412965, 4);
        assert_eq!(c.len(), 4);
        assert_eq!(c[0], ChunkSpec { start: 0, end: 1943103241 });
        assert_eq!(c[3], ChunkSpec { start: 5829309726, end: 7772412964 });
        let mut next = 0u64;
        for ch in &c {
            assert_eq!(ch.start, next);
            assert!(ch.end >= ch.start);
            next = ch.end + 1;
        }
        assert_eq!(next, 7772412965);
    }

    #[test]
    fn parse_content_range_formats() {
        // The reference CDN shape from recon.
        let cr = parse_content_range("bytes 0-1048575/7772412965").unwrap();
        assert_eq!(cr.start, 0);
        assert_eq!(cr.end, Some(1048575));
        assert_eq!(cr.total, Some(7772412965));

        let cr = parse_content_range("bytes 0-0/*").unwrap();
        assert_eq!(cr.start, 0);
        assert_eq!(cr.end, Some(0));
        assert_eq!(cr.total, None);

        let cr = parse_content_range("bytes 5-9/100").unwrap();
        assert_eq!(cr.start, 5);
        assert_eq!(cr.end, Some(9));
        assert_eq!(cr.total, Some(100));

        // Missing total tolerated.
        let cr = parse_content_range("bytes 5-9").unwrap();
        assert_eq!(cr.start, 5);
        assert_eq!(cr.end, Some(9));
        assert_eq!(cr.total, None);

        // Case-insensitive unit and tight spacing.
        let cr = parse_content_range("BYTES 1-2/3").unwrap();
        assert_eq!(cr.start, 1);
        assert_eq!(cr.end, Some(2));
        assert_eq!(cr.total, Some(3));
    }

    #[test]
    fn parse_content_range_rejects_garbage() {
        assert!(parse_content_range("notbytes 1-2/3").is_none());
        assert!(parse_content_range("bytes x-y/3").is_none());
        assert!(parse_content_range("bytes 1/3").is_none());
        assert!(parse_content_range("").is_none());
        assert!(parse_content_range("bytes -1/5").is_none());
        assert!(parse_content_range("bytes 0-0/99999999999999999999999").is_none());
    }
}
