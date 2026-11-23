//! The cache proxy ("fastcache"): hit → copy the cached object, miss → run the
//! real compiler and remember the result.

use crate::cachekey::{self, CacheKey};
use crate::compiler::CompilerSpec;
use crate::util;
use crate::{TurboError, TurboResult};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Object came from the cache; the compiler was not executed.
    Hit,
    /// Object was produced by the real compiler and stored in the cache.
    Miss,
    /// The compiler ran and failed; the exit code is propagated verbatim.
    Failed(i32),
}

impl Outcome {
    pub fn is_success(self) -> bool {
        !matches!(self, Outcome::Failed(_))
    }
}

#[derive(Debug, Clone)]
pub struct ProxyOptions {
    pub cache_dir: PathBuf,
    pub verbose: bool,
}

impl Default for ProxyOptions {
    fn default() -> Self {
        ProxyOptions {
            cache_dir: util::cache_dir(),
            verbose: util::env_verbose(),
        }
    }
}

impl ProxyOptions {
    pub fn new(cache_dir: PathBuf, verbose: bool) -> Self {
        ProxyOptions { cache_dir, verbose }
    }
}

/// Where the object for a key lives.
pub fn entry_path(cache_dir: &Path, key: &str) -> PathBuf {
    cache_dir.join(format!("{key}.obj"))
}

fn meta_path(cache_dir: &Path, key: &str) -> PathBuf {
    cache_dir.join(format!("{key}.meta"))
}

fn log(opts: &ProxyOptions, text: &str) {
    if opts.verbose {
        eprintln!("[turbo] {text}");
    }
}

/// Compile `source` into `object`, going through the cache.
///
/// * `Outcome::Failed(code)` — the compiler was executed and returned non-zero;
///   nothing is cached, so the next run retries the compiler.
/// * `Err(_)` — infrastructure problem (IO, cannot spawn compiler, broken cache).
pub fn compile(
    spec: &CompilerSpec,
    source: &Path,
    object: &Path,
    opts: &ProxyOptions,
) -> TurboResult<Outcome> {
    let started = Instant::now();
    let key = cachekey::compute(spec, source)?;
    if key.truncated {
        log(
            opts,
            &format!(
                "warning: include traversal hit a limit while hashing {}",
                source.display()
            ),
        );
    }
    let cached = entry_path(&opts.cache_dir, &key.hex);

    if is_usable(&cached) {
        util::copy_atomic(&cached, object)?;
        log(
            opts,
            &format!(
                "HIT   {}  ({} files hashed, {} ms)",
                source.display(),
                key.files,
                started.elapsed().as_millis()
            ),
        );
        return Ok(Outcome::Hit);
    }

    log(
        opts,
        &format!(
            "MISS  {}  ({} files hashed, key {})",
            source.display(),
            key.files,
            key.short()
        ),
    );

    if let Some(parent) = object.parent() {
        if !parent.as_os_str().is_empty() {
            util::ensure_dir(parent)?;
        }
    }

    let compile_started = Instant::now();
    let status = spec.compile_command(source, object).status().map_err(|e| {
        TurboError::msg(format!(
            "cannot run compiler `{}`: {e}",
            spec.program.display()
        ))
    })?;
    let compile_ms = compile_started.elapsed().as_millis();

    if !status.success() {
        let code = status.code().unwrap_or(1);
        log(
            opts,
            &format!("FAIL  {}  (compiler exited with {code})", source.display()),
        );
        return Ok(Outcome::Failed(code));
    }

    if !object.is_file() {
        return Err(TurboError::msg(format!(
            "compiler reported success but produced no object file at `{}` (check the output flag)",
            object.display()
        )));
    }

    store(&key, object, &cached, &opts.cache_dir)?;
    log(
        opts,
        &format!(
            "STORE {}  (compiled in {} ms, total {} ms)",
            source.display(),
            compile_ms,
            started.elapsed().as_millis()
        ),
    );
    Ok(Outcome::Miss)
}

/// A cached object must exist and be non-empty; a zero-length file indicates a
/// crashed writer (we write atomically, so this should be impossible — belt and
/// braces).
fn is_usable(path: &Path) -> bool {
    match fs::metadata(path) {
        Ok(meta) => meta.is_file() && meta.len() > 0,
        Err(_) => false,
    }
}

fn store(key: &CacheKey, object: &Path, cached: &Path, cache_dir: &Path) -> TurboResult<()> {
    util::ensure_dir(cache_dir)?;
    util::copy_atomic(object, cached)?;
    // Metadata is best-effort: a missing .meta never invalidates the cache.
    let _ = util::atomic_write(&meta_path(cache_dir, &key.hex), key.manifest.as_bytes());
    Ok(())
}

/// `turbo hash` and debugging: compute the key without compiling anything.
pub fn hash_only(spec: &CompilerSpec, source: &Path) -> TurboResult<CacheKey> {
    cachekey::compute(spec, source)
}

#[derive(Debug, Clone)]
pub struct CacheStats {
    pub dir: PathBuf,
    pub entries: usize,
    pub bytes: u64,
}

pub fn stats(cache_dir: &Path) -> TurboResult<CacheStats> {
    let mut entries = 0usize;
    let mut bytes = 0u64;
    match fs::read_dir(cache_dir) {
        Ok(read_dir) => {
            for entry in read_dir {
                let entry = entry?;
                if !entry.file_type()?.is_file() {
                    continue;
                }
                let is_object = entry
                    .path()
                    .extension()
                    .map(|e| e.eq_ignore_ascii_case("obj"))
                    .unwrap_or(false);
                if is_object {
                    entries += 1;
                    bytes += entry.metadata()?.len();
                }
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    Ok(CacheStats {
        dir: cache_dir.to_path_buf(),
        entries,
        bytes,
    })
}

pub fn clear(cache_dir: &Path) -> TurboResult<CacheStats> {
    let before = stats(cache_dir)?;
    if before.entries > 0 {
        util::remove_dir_if_exists(cache_dir)?;
    }
    Ok(before)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cachekey;
    use crate::compiler::CompilerSpec;
    use std::fs;

    fn tmp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("turbo-proxy-{tag}-{}", util::unique_token()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn entry_paths_are_content_addressed() {
        let dir = PathBuf::from("c:/cache");
        assert_eq!(entry_path(&dir, "abc"), PathBuf::from("c:/cache/abc.obj"));
        assert!(meta_path(&dir, "abc").ends_with("abc.meta"));
    }

    #[test]
    fn stats_counts_objects_only() {
        let dir = tmp("stats");
        fs::write(dir.join("a.obj"), b"x").unwrap();
        fs::write(dir.join("a.meta"), b"text").unwrap();
        let s = stats(&dir).unwrap();
        assert_eq!(s.entries, 1);
        assert_eq!(s.bytes, 1);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_cache_dir_is_not_an_error() {
        let dir = tmp("missing").join("does-not-exist");
        let s = stats(&dir).unwrap();
        assert_eq!(s.entries, 0);
    }

    #[test]
    fn hash_only_matches_compute() {
        let dir = tmp("hashonly");
        fs::write(dir.join("a.cpp"), "int main(){return 0;}\n").unwrap();
        let spec = CompilerSpec::new(PathBuf::from("mockcc"), vec![], dir.clone());
        let key = hash_only(&spec, &dir.join("a.cpp")).unwrap();
        let direct = cachekey::compute(&spec, &dir.join("a.cpp")).unwrap();
        assert_eq!(key.hex, direct.hex);
        let _ = fs::remove_dir_all(&dir);
    }
}
