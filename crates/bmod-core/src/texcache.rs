//! On-disk cache of encoded VTFs, so converting again with an unchanged texture
//! skips the slow DXT compression.
//!
//! Entries are keyed by a hash of the source image file's bytes and the encoder
//! settings, so an edited texture (even under the same name) is re-encoded.
//! The least recently used entries are deleted once the cache passes [`MAX_BYTES`].

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

use sha2::{Digest, Sha256};

use crate::vtf::TextureQuality;

/// Bump whenever the VTF encoder's output changes, so old entries aren't reused.
const ENCODER_VERSION: u32 = 1;
/// Size above which the least recently used entries are deleted.
pub const MAX_BYTES: u64 = 1 << 30;
const EXTENSION: &str = "vtf";
const TMP_EXTENSION: &str = "tmp";

/// How much is in a cache folder.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    pub entries: usize,
    pub bytes: u64,
}

/// The per-user cache folder, e.g. `~/.cache/bmod/textures` on Linux.
pub fn default_dir() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "bmod").map(|d| d.cache_dir().join("textures"))
}

/// Cache key for the image file contents `source` encoded at `quality`.
pub fn key(source: &[u8], quality: TextureQuality) -> String {
    let mut h = Sha256::new();
    h.update(ENCODER_VERSION.to_le_bytes());
    h.update(env!("CARGO_PKG_VERSION"));
    h.update(match quality {
        TextureQuality::High => b"high".as_slice(),
        TextureQuality::Fast => b"fast".as_slice(),
    });
    h.update(source);
    format!("{:x}", h.finalize())
}

fn entry(dir: &Path, key: &str) -> PathBuf {
    dir.join(format!("{key}.{EXTENSION}"))
}

/// The cached VTF for `key`, if any. Marks it as recently used.
pub fn get(dir: &Path, key: &str) -> Option<Vec<u8>> {
    let path = entry(dir, key);
    let bytes = fs::read(&path).ok()?;
    // Best effort: a failed touch only makes the entry look older when pruning.
    let _ = fs::File::options().write(true).open(&path).and_then(|f| f.set_modified(SystemTime::now()));
    Some(bytes)
}

/// Stores `vtf` under `key`, then trims the cache to [`MAX_BYTES`].
pub fn put(dir: &Path, key: &str, vtf: &[u8]) -> io::Result<()> {
    put_with_limit(dir, key, vtf, MAX_BYTES)
}

fn put_with_limit(dir: &Path, key: &str, vtf: &[u8], max_bytes: u64) -> io::Result<()> {
    static TMP_ID: AtomicU64 = AtomicU64::new(0);
    fs::create_dir_all(dir)?;
    // Write then rename, so a crash or a concurrent writer never leaves a partial entry.
    let tmp = dir.join(format!(
        "{key}.{}-{}.{TMP_EXTENSION}",
        std::process::id(),
        TMP_ID.fetch_add(1, Ordering::Relaxed)
    ));
    fs::write(&tmp, vtf)?;
    if let Err(e) = fs::rename(&tmp, entry(dir, key)) {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    prune(dir, max_bytes)
}

/// Number and total size of the entries in `dir`; zero if it doesn't exist.
pub fn usage(dir: &Path) -> io::Result<Usage> {
    let mut usage = Usage::default();
    for (_, len) in files(dir, &[EXTENSION])? {
        usage.entries += 1;
        usage.bytes += len;
    }
    Ok(usage)
}

/// Deletes every cached texture (and leftover temporary file) in `dir`, then
/// `dir` itself if nothing else is in it. Returns what was removed.
pub fn clear(dir: &Path) -> io::Result<Usage> {
    let mut removed = Usage::default();
    for (path, len) in files(dir, &[EXTENSION, TMP_EXTENSION])? {
        fs::remove_file(&path)?;
        if path.extension().is_some_and(|x| x == EXTENSION) {
            removed.entries += 1;
            removed.bytes += len;
        }
    }
    let _ = fs::remove_dir(dir); // fails, harmlessly, if anything else is in there
    Ok(removed)
}

/// Files in `dir` with one of `extensions`, with their sizes. Empty if `dir` doesn't exist.
fn files(dir: &Path, extensions: &[&str]) -> io::Result<Vec<(PathBuf, u64)>> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    Ok(entries
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().is_some_and(|x| extensions.iter().any(|ext| x == *ext)))
        .filter_map(|e| Some((e.path(), e.metadata().ok().filter(|m| m.is_file())?.len())))
        .collect())
}

/// Deletes the least recently used entries until the total is at most `max_bytes`.
fn prune(dir: &Path, max_bytes: u64) -> io::Result<()> {
    let mut entries: Vec<(SystemTime, u64, PathBuf)> = fs::read_dir(dir)?
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().is_some_and(|x| x == EXTENSION))
        .filter_map(|e| {
            let meta = e.metadata().ok()?;
            Some((meta.modified().ok()?, meta.len(), e.path()))
        })
        .collect();
    entries.sort_by_key(|e| std::cmp::Reverse(e.0)); // newest first
    let mut total = 0;
    for (_, len, path) in entries {
        total += len;
        if total > max_bytes {
            let _ = fs::remove_file(path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("bmod-texcache-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn keys_depend_on_contents_and_quality() {
        let k = key(b"png bytes", TextureQuality::High);
        assert_eq!(k, key(b"png bytes", TextureQuality::High));
        assert_ne!(k, key(b"png bytez", TextureQuality::High));
        assert_ne!(k, key(b"png bytes", TextureQuality::Fast));
    }

    #[test]
    fn stores_and_returns_entries() {
        let dir = temp_dir("roundtrip");
        assert_eq!(get(&dir, "abc"), None);
        put(&dir, "abc", b"vtf data").unwrap();
        assert_eq!(get(&dir, "abc").as_deref(), Some(b"vtf data".as_slice()));
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1, "temporary file left behind");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn reports_usage_and_clears() {
        let dir = temp_dir("clear");
        assert_eq!(usage(&dir).unwrap(), Usage::default(), "missing folder counts as empty");
        put(&dir, "a", &[0; 10]).unwrap();
        put(&dir, "b", &[0; 5]).unwrap();
        fs::write(dir.join("stray.1-0.tmp"), b"partial").unwrap();
        assert_eq!(usage(&dir).unwrap(), Usage { entries: 2, bytes: 15 });
        assert_eq!(clear(&dir).unwrap(), Usage { entries: 2, bytes: 15 });
        assert!(!dir.exists(), "emptied folder should be removed");
        assert_eq!(clear(&dir).unwrap(), Usage::default());

        // Files that aren't ours are left alone, and so is the folder.
        put(&dir, "a", &[0; 10]).unwrap();
        fs::write(dir.join("notes.txt"), b"mine").unwrap();
        clear(&dir).unwrap();
        assert!(dir.join("notes.txt").exists());
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn evicts_least_recently_used() {
        let dir = temp_dir("evict");
        let age = |k: &str, secs: u64| {
            let f = fs::File::options().write(true).open(entry(&dir, k)).unwrap();
            f.set_modified(SystemTime::now() - Duration::from_secs(secs)).unwrap();
        };
        put_with_limit(&dir, "old", &[0; 10], 100).unwrap();
        put_with_limit(&dir, "used", &[0; 10], 100).unwrap();
        age("old", 300);
        age("used", 200);
        get(&dir, "used"); // touched: now the newest of the two
        put_with_limit(&dir, "new", &[0; 10], 20).unwrap();
        assert!(get(&dir, "new").is_some());
        assert!(get(&dir, "used").is_some());
        assert!(get(&dir, "old").is_none());
        fs::remove_dir_all(dir).ok();
    }
}
