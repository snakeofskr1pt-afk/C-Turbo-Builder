//! Small filesystem / path helpers shared by the proxy and the orchestrator.

use std::fs;
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Monotonic-ish unique token for temporary file names (pid + counter + time).
pub fn unique_token() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{}.{}.{}", std::process::id(), seq, nanos)
}

pub fn ensure_dir(p: &Path) -> io::Result<()> {
    fs::create_dir_all(p)
}

pub fn ensure_parent(p: &Path) -> io::Result<()> {
    match p.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => fs::create_dir_all(parent),
        _ => Ok(()),
    }
}

fn sibling_temp(path: &Path, tag: &str) -> PathBuf {
    let parent = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".to_string());
    parent.join(format!(".{name}.{tag}.{}.tmp", unique_token()))
}

/// Write a file atomically: never leave a half-written cache entry behind.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    ensure_parent(path)?;
    let tmp = sibling_temp(path, "w");
    {
        let mut file = fs::File::create(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    match fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// Copy a file atomically (temp file + rename) so a reader never sees a
/// partially written object file.
pub fn copy_atomic(src: &Path, dst: &Path) -> io::Result<()> {
    ensure_parent(dst)?;
    let tmp = sibling_temp(dst, "c");
    let result = fs::copy(src, &tmp).and_then(|_| fs::rename(&tmp, dst));
    match result {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// Collapse `.` and `..` without touching the filesystem.
pub fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push(component.as_os_str());
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Absolute, normalised path (still not resolving symlinks).
pub fn absolute(path: &Path) -> PathBuf {
    if path.is_absolute() {
        normalize(path)
    } else {
        match std::env::current_dir() {
            Ok(cwd) => normalize(&cwd.join(path)),
            Err(_) => normalize(path),
        }
    }
}

/// Stable string used inside cache keys: absolute, forward slashes, lowercased
/// on Windows (case-insensitive filesystem). Keeps keys reproducible between
/// runs and between relative/absolute spellings of the same file.
pub fn path_key(path: &Path) -> String {
    let text = absolute(path).to_string_lossy().replace('\\', "/");
    if cfg!(windows) {
        text.to_ascii_lowercase()
    } else {
        text
    }
}

/// Path with forward slashes, suitable for `#include "..."` in generated files.
pub fn include_literal(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// Where cached object files live. Override with `TURBO_CACHE_DIR`.
pub fn cache_dir() -> PathBuf {
    if let Some(value) = std::env::var_os("TURBO_CACHE_DIR") {
        if !value.is_empty() {
            return PathBuf::from(value);
        }
    }
    if cfg!(windows) {
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            if !local.is_empty() {
                return PathBuf::from(local).join("cpp-turbo-cache");
            }
        }
        if let Some(profile) = std::env::var_os("USERPROFILE") {
            if !profile.is_empty() {
                return PathBuf::from(profile).join("AppData/Local/cpp-turbo-cache");
            }
        }
    }
    if let Some(xdg) = std::env::var_os("XDG_CACHE_HOME") {
        if !xdg.is_empty() {
            return PathBuf::from(xdg).join("cpp-turbo-cache");
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        if !home.is_empty() {
            return PathBuf::from(home).join(".cache/cpp-turbo-cache");
        }
    }
    std::env::temp_dir().join("cpp-turbo-cache")
}

/// (size, modification time in nanoseconds since epoch) — cheap but effective
/// identity for the compiler binary, so a compiler upgrade busts the cache.
pub fn file_identity(path: &Path) -> io::Result<(u64, u128)> {
    let meta = fs::metadata(path)?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    Ok((meta.len(), mtime))
}

/// Recursively delete a directory, ignoring "not found".
pub fn remove_dir_if_exists(path: &Path) -> io::Result<()> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Number of threads the machine can actually run, with an env override.
pub fn default_jobs() -> usize {
    if let Ok(raw) = std::env::var("TURBO_JOBS") {
        if let Ok(n) = raw.trim().parse::<usize>() {
            if n > 0 {
                return n;
            }
        }
    }
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
}

/// Whether to print per-unit progress. `TURBO_VERBOSE=1` works too.
pub fn env_verbose() -> bool {
    matches!(
        std::env::var("TURBO_VERBOSE").as_deref(),
        Ok("1") | Ok("true") | Ok("yes")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_dot_segments() {
        let normalized = normalize(Path::new("a/b/../c/./d"));
        assert_eq!(normalized, Path::new("a").join("c").join("d"));
    }

    #[test]
    fn path_key_is_lowercase_on_windows() {
        let key = path_key(Path::new("Some/Dir/File.cpp"));
        if cfg!(windows) {
            assert_eq!(key, key.to_ascii_lowercase());
        }
        assert!(!key.contains('\\'));
    }

    #[test]
    fn atomic_write_then_read() {
        let dir = std::env::temp_dir().join(format!("turbo-util-{}", unique_token()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("nested/deep/out.bin");
        atomic_write(&file, b"payload").unwrap();
        assert_eq!(fs::read(&file).unwrap(), b"payload");
        // no temp leftovers
        let leftovers: Vec<_> = fs::read_dir(file.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "leftover temp files: {leftovers:?}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn copy_atomic_overwrites() {
        let dir = std::env::temp_dir().join(format!("turbo-util-copy-{}", unique_token()));
        fs::create_dir_all(&dir).unwrap();
        let src = dir.join("src.obj");
        let dst = dir.join("dst.obj");
        fs::write(&src, b"first").unwrap();
        fs::write(&dst, b"stale").unwrap();
        copy_atomic(&src, &dst).unwrap();
        assert_eq!(fs::read(&dst).unwrap(), b"first");
        let _ = fs::remove_dir_all(&dir);
    }
}
