//! Cache key = compiler identity + compile flags + content of the source and of
//! every project header it (transitively) includes.
//!
//! The key is the SHA-256 of a small canonical manifest. The manifest is stored
//! next to the cached object (`.meta`) so a cache hit/miss can be explained
//! months later without re-running the tool.

use crate::compiler::{resolve_program, CompilerSpec};
use crate::includes;
use crate::sha256;
use crate::util;
use crate::TurboResult;
use std::path::Path;

pub const KEY_VERSION: &str = "cpp-turbo-builder cache-key v1";

#[derive(Debug, Clone)]
pub struct CacheKey {
    /// 64 lowercase hex characters.
    pub hex: String,
    /// The canonical text that was hashed (stored as the `.meta` sidecar).
    pub manifest: String,
    /// How many files (source + headers) took part.
    pub files: usize,
    /// Include targets that were not found on disk (system headers, typically).
    pub unresolved: Vec<String>,
    /// True when a depth/size limit truncated the traversal.
    pub truncated: bool,
}

impl CacheKey {
    pub fn short(&self) -> &str {
        &self.hex[..self.hex.len().min(8)]
    }
}

/// `stem\tsize\tmtime` — deliberately *not* the full compiler path, so caches
/// stay usable if the toolchain moves, while any reinstall/upgrade still busts
/// the cache because size or mtime changes.
fn compiler_identity(spec: &CompilerSpec) -> String {
    let resolved = resolve_program(&spec.program, &spec.cwd);
    let stem = resolved
        .file_name()
        .map(|n| n.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_else(|| spec.program.to_string_lossy().to_ascii_lowercase());
    let (size, mtime) = util::file_identity(&resolved).unwrap_or((0, 0));
    format!("{stem}\t{size}\t{mtime}")
}

pub fn compute(spec: &CompilerSpec, source: &Path) -> TurboResult<CacheKey> {
    let mut roots: Vec<std::path::PathBuf> = vec![util::absolute(source)];
    roots.extend(spec.forced_includes.iter().cloned());

    let set = includes::collect(&roots, &spec.include_dirs)?;

    let mut manifest = String::with_capacity(4096);
    manifest.push_str(KEY_VERSION);
    manifest.push('\n');
    manifest.push_str("compiler\t");
    manifest.push_str(&compiler_identity(spec));
    manifest.push('\n');
    manifest.push_str("toolchain\t");
    manifest.push_str(spec.toolchain.name());
    manifest.push('\n');
    for flag in &spec.user_flags {
        manifest.push_str("flag\t");
        manifest.push_str(flag);
        manifest.push('\n');
    }
    // Escape hatch: let users add an arbitrary salt (e.g. the value of an env
    // var the build depends on) without changing the tool.
    if let Ok(salt) = std::env::var("TURBO_EXTRA_KEY") {
        if !salt.is_empty() {
            manifest.push_str("salt\t");
            manifest.push_str(&salt);
            manifest.push('\n');
        }
    }
    for file in &set.files {
        manifest.push_str("file\t");
        manifest.push_str(&file.key);
        manifest.push('\t');
        manifest.push_str(&sha256::hex(&file.digest));
        manifest.push('\n');
    }

    let digest = sha256::hash_bytes(manifest.as_bytes());
    Ok(CacheKey {
        hex: sha256::hex(&digest),
        manifest,
        files: set.files.len(),
        unresolved: set.unresolved,
        truncated: set.truncated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compiler::CompilerSpec;
    use std::fs;
    use std::path::PathBuf;

    fn fixture(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("turbo-key-{tag}-{}", util::unique_token()));
        fs::create_dir_all(dir.join("include")).unwrap();
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("include/a.h"), "#pragma once\nint a();\n").unwrap();
        fs::write(
            dir.join("src/main.cpp"),
            "#include \"../include/a.h\"\nint main(){return a();}\n",
        )
        .unwrap();
        dir
    }

    fn spec_for(dir: &Path, extra: &[&str]) -> CompilerSpec {
        let mut flags: Vec<String> = vec!["-I".into(), "include".into(), "-O2".into()];
        flags.extend(extra.iter().map(|s| s.to_string()));
        CompilerSpec::new(PathBuf::from("mockcc"), flags, dir.to_path_buf())
    }

    #[test]
    fn key_is_stable_and_covers_headers() {
        let dir = fixture("stable");
        let source = dir.join("src/main.cpp");
        let spec = spec_for(&dir, &[]);

        let first = compute(&spec, &source).unwrap();
        let second = compute(&spec, &source).unwrap();
        assert_eq!(first.hex, second.hex);
        assert_eq!(first.files, 2);

        // Touch a header: the key must change.
        fs::write(dir.join("include/a.h"), "#pragma once\nint a2();\n").unwrap();
        let third = compute(&spec, &source).unwrap();
        assert_ne!(first.hex, third.hex);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn key_changes_with_flags() {
        let dir = fixture("flags");
        let source = dir.join("src/main.cpp");
        let a = compute(&spec_for(&dir, &["-DVALUE=1"]), &source).unwrap();
        let b = compute(&spec_for(&dir, &["-DVALUE=2"]), &source).unwrap();
        assert_ne!(a.hex, b.hex);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn key_changes_with_source_content() {
        let dir = fixture("source");
        let source = dir.join("src/main.cpp");
        let spec = spec_for(&dir, &[]);
        let a = compute(&spec, &source).unwrap();
        fs::write(dir.join("src/main.cpp"), "int main(){return 42;}\n").unwrap();
        let b = compute(&spec, &source).unwrap();
        assert_ne!(a.hex, b.hex);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn manifest_is_human_readable() {
        let dir = fixture("manifest");
        let source = dir.join("src/main.cpp");
        let key = compute(&spec_for(&dir, &[]), &source).unwrap();
        assert!(key.manifest.starts_with(KEY_VERSION));
        assert!(key.manifest.contains("compiler\t"));
        assert!(key.manifest.contains("main.cpp"));
        assert!(key.manifest.contains("a.h"));
        assert_eq!(key.hex.len(), 64);
        let _ = fs::remove_dir_all(&dir);
    }
}
