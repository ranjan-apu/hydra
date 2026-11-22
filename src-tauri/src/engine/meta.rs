//! Sidecar meta "<destDir>/<filename>.hydra.json" - EXACT CONTRACT schema:
//! { "url", "size", "chunkSize", "chunks": [ { "start", "end", "done" } ] }.
//! Saves are atomic (temp file in the same directory, then rename). Resume
//! validation checks url + size and that the chunks contiguously cover
//! [0, size-1]; clamping bounds each done-offset and resets a chunk whose
//! claimed bytes are not on disk.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MetaFile {
    pub url: String,
    pub size: u64,
    pub chunk_size: u64,
    pub chunks: Vec<MetaChunk>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MetaChunk {
    pub start: u64,
    /// Inclusive.
    pub end: u64,
    /// Bytes of this chunk already safely on disk.
    pub done: u64,
}

/// Atomic save: serialize, write to a temp file next to the target, then rename
/// over it. A crash mid-write never corrupts the previous meta.
pub fn save_atomic(path: &Path, meta: &MetaFile) -> std::io::Result<()> {
    let json = serde_json::to_string(meta)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let tmp = tmp_path(path);
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(json.as_bytes())?;
        f.write_all(b"\n")?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)
}

/// Same-directory temp path for the atomic save.
fn tmp_path(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_os_string();
    s.push(".tmp");
    PathBuf::from(s)
}

/// Load a sidecar; None when missing or corrupt.
pub fn load(path: &Path) -> Option<MetaFile> {
    let text = fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// url + size must match and the chunks must contiguously cover [0, size-1].
pub fn is_compatible(meta: &MetaFile, url: &str, size: u64) -> bool {
    if meta.url != url || meta.size != size {
        return false;
    }
    if meta.size == 0 {
        return meta.chunks.is_empty();
    }
    if meta.chunks.is_empty() {
        return false;
    }
    if meta.chunks[0].start != 0 {
        return false;
    }
    let mut expected_start = 0u64;
    for c in &meta.chunks {
        if c.start != expected_start || c.end < c.start {
            return false;
        }
        match c.end.checked_add(1) {
            Some(next) => expected_start = next,
            None => return false,
        }
    }
    expected_start == meta.size
}

/// Clamp each chunk.done into [0, end-start+1]; if the .part file is shorter
/// than start+done the claimed bytes are not on disk, so reset that chunk to 0.
pub fn clamp_dones(meta: &mut MetaFile, part_len: u64) {
    for c in &mut meta.chunks {
        let len = c.end.saturating_sub(c.start) + 1;
        let mut done = c.done.min(len);
        if part_len < c.start.saturating_add(done) {
            done = 0;
        }
        c.done = done;
    }
}

/// Sum of chunk dones (post-clamp byte count already on disk).
pub fn bytes_done(meta: &MetaFile) -> u64 {
    meta.chunks.iter().map(|c| c.done).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("hydra-unit-meta-{}-{}", tag, std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn exact_schema_round_trip() {
        let m = MetaFile {
            url: "https://nexus/dld/file?token=1".into(),
            size: 7772412965,
            chunk_size: 1943103242,
            chunks: vec![MetaChunk { start: 0, end: 1943103241, done: 1943103242 }],
        };
        let json = serde_json::to_string(&m).unwrap();
        assert_eq!(
            json,
            "{\"url\":\"https://nexus/dld/file?token=1\",\"size\":7772412965,\"chunkSize\":1943103242,\"chunks\":[{\"start\":0,\"end\":1943103241,\"done\":1943103242}]}"
        );
        let back: MetaFile = serde_json::from_str(&json).unwrap();
        assert_eq!(back.size, m.size);
        assert_eq!(back.chunk_size, m.chunk_size);
        assert_eq!(back.chunks, m.chunks);
    }

    #[test]
    fn save_atomic_writes_loads_and_leaves_no_tmp() {
        let dir = temp_dir("save");
        let path = dir.join("x.hydra.json");
        let m = MetaFile {
            url: "u".into(),
            size: 10,
            chunk_size: 5,
            chunks: vec![
                MetaChunk { start: 0, end: 4, done: 1 },
                MetaChunk { start: 5, end: 9, done: 0 },
            ],
        };
        save_atomic(&path, &m).unwrap();
        assert!(path.exists());
        assert!(!dir.join("x.hydra.json.tmp").exists());
        let l = load(&path).unwrap();
        assert_eq!(l.chunks.len(), 2);
        assert_eq!(l.chunks[0].done, 1);
        // Overwrite is fine (temp + rename again).
        save_atomic(&path, &m).unwrap();
        assert!(load(&path).is_some());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_missing_and_corrupt_is_none() {
        let dir = temp_dir("load");
        assert!(load(&dir.join("nope.json")).is_none());
        let p = dir.join("bad.json");
        fs::write(&p, "{nope").unwrap();
        assert!(load(&p).is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn compatibility_rules() {
        let base = || MetaFile {
            url: "u".into(),
            size: 10,
            chunk_size: 5,
            chunks: vec![
                MetaChunk { start: 0, end: 4, done: 0 },
                MetaChunk { start: 5, end: 9, done: 0 },
            ],
        };
        assert!(is_compatible(&base(), "u", 10));
        assert!(!is_compatible(&base(), "other", 10));
        assert!(!is_compatible(&base(), "u", 11));

        let mut gap = base();
        gap.chunks[1].start = 6;
        assert!(!is_compatible(&gap, "u", 10));

        let mut wrong_end = base();
        wrong_end.chunks[1].end = 8;
        assert!(!is_compatible(&wrong_end, "u", 10));

        let mut no_zero_start = base();
        no_zero_start.chunks[0].start = 1;
        assert!(!is_compatible(&no_zero_start, "u", 10));

        let mut overlap = base();
        overlap.chunks[0].end = 5;
        assert!(!is_compatible(&overlap, "u", 10));

        let mut inverted = base();
        inverted.chunks[1].end = 3;
        assert!(!is_compatible(&inverted, "u", 10));

        let empty = MetaFile { url: "u".into(), size: 0, chunk_size: 0, chunks: vec![] };
        assert!(is_compatible(&empty, "u", 0));
        let _ = empty;
    }

    #[test]
    fn clamp_rules() {
        let mut m = MetaFile {
            url: "u".into(),
            size: 100,
            chunk_size: 50,
            chunks: vec![
                MetaChunk { start: 0, end: 49, done: 60 },
                MetaChunk { start: 50, end: 99, done: 30 },
            ],
        };
        // part file only 70 bytes long: chunk 0 fits, chunk 1's claim does not.
        clamp_dones(&mut m, 70);
        assert_eq!(m.chunks[0].done, 50); // clamped to len
        assert_eq!(m.chunks[1].done, 0); // part_len < 50+30
        // Boundary: exactly long enough keeps the claim.
        let mut m2 = MetaFile {
            url: "u".into(),
            size: 100,
            chunk_size: 50,
            chunks: vec![
                MetaChunk { start: 0, end: 49, done: 50 },
                MetaChunk { start: 50, end: 99, done: 30 },
            ],
        };
        clamp_dones(&mut m2, 80);
        assert_eq!(m2.chunks[1].done, 30);
        // Zero stays zero.
        let mut m3 = MetaFile {
            url: "u".into(),
            size: 10,
            chunk_size: 10,
            chunks: vec![MetaChunk { start: 0, end: 9, done: 0 }],
        };
        clamp_dones(&mut m3, 0);
        assert_eq!(m3.chunks[0].done, 0);
    }

    #[test]
    fn bytes_done_sums() {
        let m = MetaFile {
            url: "u".into(),
            size: 100,
            chunk_size: 50,
            chunks: vec![
                MetaChunk { start: 0, end: 49, done: 10 },
                MetaChunk { start: 50, end: 99, done: 7 },
            ],
        };
        assert_eq!(bytes_done(&m), 17);
    }
}
