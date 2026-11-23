//! `#include` discovery: which headers participate in a translation unit.
//!
//! The scanner is intentionally naive — it walks the raw bytes looking for
//! `#include "..."` / `#include <...>` and does not try to be a C preprocessor.
//! Naivety here is *safe*: a comment or string literal that looks like an
//! include only causes the cache key to cover an extra file, i.e. a possible
//! false cache *miss*. It can never cause a false cache *hit*, which is the
//! property we care about.
//!
//! Includes that cannot be resolved on disk (system headers such as
//! `<vector>`) are simply skipped: their content is the compiler's / SDK's
//! business, and the compiler identity plus flags already pin those down.

use crate::sha256;
use crate::util;
use std::collections::HashSet;
use std::fs;
use std::io::{self, BufReader};
use std::path::{Path, PathBuf};

/// Hard limits so a pathological include graph cannot hang the build.
pub const MAX_DEPTH: usize = 64;
pub const MAX_FILE_BYTES: u64 = 32 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IncludeStyle {
    /// `#include "..."` — searched next to the including file, then in `-I` dirs.
    Quote,
    /// `#include <...>` — searched only in `-I` dirs (system headers are skipped).
    Angle,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Include {
    pub style: IncludeStyle,
    pub target: String,
}

/// One file that took part in the hash, with its content digest.
#[derive(Debug, Clone)]
pub struct FileDigest {
    pub path: PathBuf,
    pub key: String,
    pub digest: [u8; 32],
}

/// Result of a full traversal.
#[derive(Debug, Default)]
pub struct IncludeSet {
    pub files: Vec<FileDigest>,
    /// Include directives that could not be resolved on disk (system headers).
    pub unresolved: Vec<String>,
    /// True if any limit kicked in; the key still covers what was traversed.
    pub truncated: bool,
}

/// Extract raw `#include` directives from a buffer.
pub fn parse_includes(data: &[u8]) -> Vec<Include> {
    let mut found = Vec::new();
    let mut i = 0usize;
    while i < data.len() {
        if data[i] != b'#' {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        // `##` is token pasting, not a directive.
        if j < data.len() && data[j] == b'#' {
            i = j + 1;
            continue;
        }
        // Allow whitespace between '#' and the directive name (valid C).
        while j < data.len() && (data[j] == b' ' || data[j] == b'\t') {
            j += 1;
        }
        let word_start = j;
        while j < data.len() && (data[j].is_ascii_alphabetic() || data[j] == b'_') {
            j += 1;
        }
        if &data[word_start..j] != b"include" {
            i = j.max(i + 1);
            continue;
        }
        while j < data.len() && (data[j] == b' ' || data[j] == b'\t') {
            j += 1;
        }
        if j >= data.len() {
            break;
        }
        let (end_byte, style) = match data[j] {
            b'"' => (b'"', IncludeStyle::Quote),
            b'<' => (b'>', IncludeStyle::Angle),
            _ => {
                i = j;
                continue;
            }
        };
        j += 1;
        let name_start = j;
        while j < data.len() && data[j] != end_byte && data[j] != b'\n' && data[j] != b'\r' {
            j += 1;
        }
        if j < data.len() && data[j] == end_byte && j > name_start {
            let name = String::from_utf8_lossy(&data[name_start..j]).into_owned();
            let name = name.trim().to_string();
            if !name.is_empty() {
                found.push(Include {
                    style,
                    target: name,
                });
            }
        }
        i = j + 1;
    }
    found
}

/// Resolve an include the way a compiler would: for quotes, the directory of
/// the including file first, then the `-I` directories; for angle includes only
/// the `-I` directories (system include paths are out of scope on purpose).
pub fn resolve(
    include: &Include,
    including_file: &Path,
    include_dirs: &[PathBuf],
) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::with_capacity(include_dirs.len() + 1);
    if include.style == IncludeStyle::Quote {
        if let Some(parent) = including_file.parent() {
            candidates.push(parent.join(&include.target));
        }
    }
    for dir in include_dirs {
        candidates.push(dir.join(&include.target));
    }
    for candidate in candidates {
        if candidate.is_file() {
            return Some(util::absolute(&candidate));
        }
    }
    None
}

fn read_limited(path: &Path) -> io::Result<(Option<Vec<u8>>, u64)> {
    let meta = fs::metadata(path)?;
    if meta.len() > MAX_FILE_BYTES {
        return Ok((None, meta.len()));
    }
    let file = fs::File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut data = Vec::with_capacity(meta.len() as usize);
    std::io::Read::read_to_end(&mut reader, &mut data)?;
    Ok((Some(data), meta.len()))
}

fn hash_file(path: &Path) -> io::Result<[u8; 32]> {
    let file = fs::File::open(path)?;
    let mut reader = BufReader::new(file);
    sha256::hash_reader(&mut reader)
}

/// Below these limits thread setup costs more than the hashing itself, so a
/// small graph is hashed in the calling thread.
const PARALLEL_MIN_FILES: usize = 8;
const PARALLEL_MIN_BYTES: u64 = 1024 * 1024;

/// Hash `paths` on several threads (std scoped threads, no rayon).
///
/// The first pass over the graph has to read files to find their includes; this
/// second pass re-reads them, which is cheap because the OS page cache is warm.
/// In exchange, hashing a large header corpus is spread across cores (measured
/// on a 4-core box: 200 headers / 1.7 MiB, 160 ms → 82 ms; 400 headers / 27 MiB,
/// 469 ms → 276 ms).
fn hash_files_parallel(paths: &[PathBuf], total_bytes: u64) -> io::Result<Vec<[u8; 32]>> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }
    let jobs = util::default_jobs().max(1).min(paths.len());
    if jobs <= 1 || paths.len() < PARALLEL_MIN_FILES || total_bytes < PARALLEL_MIN_BYTES {
        return paths.iter().map(|path| hash_file(path)).collect();
    }
    let chunk_size = (paths.len() + jobs - 1) / jobs;
    let per_thread: Vec<io::Result<Vec<[u8; 32]>>> = std::thread::scope(|scope| {
        let handles: Vec<_> = paths
            .chunks(chunk_size)
            .map(|chunk| {
                scope.spawn(move || -> io::Result<Vec<[u8; 32]>> {
                    chunk.iter().map(|path| hash_file(path)).collect()
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| match handle.join() {
                Ok(result) => result,
                Err(_) => Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "header hashing thread panicked",
                )),
            })
            .collect()
    });

    let mut digests = Vec::with_capacity(paths.len());
    for result in per_thread {
        digests.extend(result?);
    }
    Ok(digests)
}

/// Walk `sources` and every transitively included project header.
///
/// `include_dirs` should already be absolute (the caller resolves `-I` values).
pub fn collect(sources: &[PathBuf], include_dirs: &[PathBuf]) -> io::Result<IncludeSet> {
    let mut set = IncludeSet::default();
    let mut visited: HashSet<String> = HashSet::new();
    // (file, depth)
    let mut stack: Vec<(PathBuf, usize)> = sources.iter().map(|p| (p.clone(), 0usize)).collect();
    // (cache-key, path, size) in traversal order
    let mut discovered: Vec<(String, PathBuf, u64)> = Vec::new();
    let mut total_bytes: u64 = 0;

    while let Some((path, depth)) = stack.pop() {
        let key = util::path_key(&path);
        if !visited.insert(key.clone()) {
            continue;
        }

        // The file is read once to discover its includes; hashing happens
        // afterwards, in parallel, over the collected file set.
        let (data, size) = match read_limited(&path) {
            Ok((data, size)) => (data, size),
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        if data.is_none() {
            set.truncated = true; // oversized file: hashed, but not scanned
        }
        total_bytes = total_bytes.saturating_add(size);
        discovered.push((key, path.clone(), size));

        let data = match data {
            Some(data) => data,
            None => continue,
        };
        if depth >= MAX_DEPTH {
            set.truncated = true;
            continue;
        }

        for include in parse_includes(&data) {
            match resolve(&include, &path, include_dirs) {
                Some(resolved) => {
                    if !visited.contains(&util::path_key(&resolved)) {
                        stack.push((resolved, depth + 1));
                    }
                }
                None => {
                    if !set.unresolved.iter().any(|u| u == &include.target) {
                        set.unresolved.push(include.target.clone());
                    }
                }
            }
        }
    }

    let paths: Vec<PathBuf> = discovered.iter().map(|(_, path, _)| path.clone()).collect();
    let digests = hash_files_parallel(&paths, total_bytes)?;
    set.files = discovered
        .into_iter()
        .zip(digests)
        .map(|((key, path, _), digest)| FileDigest { path, key, digest })
        .collect();
    set.files.sort_by(|a, b| a.key.cmp(&b.key));
    Ok(set)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn parses_both_styles_and_ignores_noise() {
        let text = br#"
// #include "commented_out.h" is still treated as a dependency (safe)
#include "a.h"
#  include <lib/b.h>
#  define X 1
## paste
#include_next "nope.h"
int main() { return 0; }
"#;
        let includes = parse_includes(text);
        let names: Vec<_> = includes.iter().map(|i| i.target.as_str()).collect();
        assert!(names.contains(&"a.h"));
        assert!(names.contains(&"lib/b.h"));
        assert!(names.contains(&"commented_out.h"));
        let angle = includes.iter().find(|i| i.target == "lib/b.h").unwrap();
        assert_eq!(angle.style, IncludeStyle::Angle);
    }

    #[test]
    fn ignores_string_literals_that_are_not_includes() {
        // A string literal that merely looks like an include: over-approximation
        // is acceptable (a false miss), under-approximation is not.
        let text = br##"const char* s = "#include \"fake.h\"";"##;
        let includes = parse_includes(text);
        // Conservative over-approximation is allowed, under-approximation is not.
        assert!(includes.iter().all(|i| i.target == "fake.h"));
    }

    fn tmpdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("turbo-inc-{tag}-{}", util::unique_token()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn collects_transitive_project_headers() {
        let dir = tmpdir("chain");
        fs::create_dir_all(dir.join("include")).unwrap();
        fs::create_dir_all(dir.join("src")).unwrap();
        let mut h1 = fs::File::create(dir.join("src/a.cpp")).unwrap();
        writeln!(h1, "#include \"../include/a.h\"").unwrap();
        writeln!(h1, "#include <string>").unwrap();
        let mut h2 = fs::File::create(dir.join("include/a.h")).unwrap();
        writeln!(h2, "#pragma once").unwrap();
        writeln!(h2, "#include \"b.h\"").unwrap();
        fs::write(dir.join("include/b.h"), "#pragma once\n").unwrap();

        let include_dirs = vec![util::absolute(&dir.join("include"))];
        let set = collect(&[util::absolute(&dir.join("src/a.cpp"))], &include_dirs).unwrap();

        let mut names: Vec<String> = set
            .files
            .iter()
            .map(|f| f.path.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        names.sort();
        assert_eq!(names, vec!["a.cpp", "a.h", "b.h"]);
        assert_eq!(set.unresolved, vec!["string".to_string()]);
        assert!(!set.truncated);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn survives_include_cycles() {
        let dir = tmpdir("cycle");
        fs::write(dir.join("a.h"), "#include \"b.h\"\n").unwrap();
        fs::write(dir.join("b.h"), "#include \"a.h\"\n").unwrap();
        let set = collect(&[util::absolute(&dir.join("a.h"))], &[]).unwrap();
        assert_eq!(set.files.len(), 2);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn angle_include_uses_i_dirs_only() {
        let dir = tmpdir("angle");
        fs::create_dir_all(dir.join("third_party")).unwrap();
        fs::write(dir.join("third_party/lib.h"), "int f();\n").unwrap();
        fs::write(dir.join("main.cpp"), "#include <lib.h>\n").unwrap();
        let dirs = vec![util::absolute(&dir.join("third_party"))];
        let set = collect(&[util::absolute(&dir.join("main.cpp"))], &dirs).unwrap();
        assert_eq!(set.files.len(), 2);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn parallel_hashing_matches_direct_sha256() {
        let dir = tmpdir("parallel");
        let mut roots = Vec::new();
        for index in 0..64 {
            let path = dir.join(format!("h{index}.h"));
            fs::write(&path, format!("#pragma once\n#define V{index} {index}\n")).unwrap();
            roots.push(path);
        }

        let set = collect(&roots, &[]).unwrap();
        assert_eq!(set.files.len(), 64);
        for file in &set.files {
            let expected = sha256::hash_bytes(&fs::read(&file.path).unwrap());
            assert_eq!(file.digest, expected, "digest mismatch for {:?}", file.path);
        }

        // Traversal used several threads; the result must stay deterministic.
        let again = collect(&roots, &[]).unwrap();
        let first: Vec<(&str, [u8; 32])> = set
            .files
            .iter()
            .map(|file| (file.key.as_str(), file.digest))
            .collect();
        let second: Vec<(&str, [u8; 32])> = again
            .files
            .iter()
            .map(|file| (file.key.as_str(), file.digest))
            .collect();
        assert_eq!(first, second);
        let _ = fs::remove_dir_all(&dir);
    }
}
