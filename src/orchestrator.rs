//! The orchestrator: find every `.cpp`, glue them into Unity Build chunks,
//! compile the chunks in parallel through the cache proxy, then link.
//!
//! Why Unity Build: C++ compilation is dominated by repeated parsing of the
//! same headers. Gluing 8–10 sources into one translation unit makes each
//! header parse once per chunk instead of once per source.

use crate::compiler::CompilerSpec;
use crate::proxy::{self, Outcome, ProxyOptions};
use crate::util;
use crate::{TurboError, TurboResult};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub const BUILD_DIR_NAME: &str = ".build";
pub const DEFAULT_CHUNK_SIZE: usize = 8;

const SOURCE_EXTENSIONS: &[&str] = &["cpp", "cc", "cxx", "c++", "cp", "c"];

const SKIP_DIRS: &[&str] = &[
    ".git",
    ".build",
    ".vs",
    ".idea",
    ".cache",
    ".deps",
    "target",
    "build",
    "out",
    "obj",
    "bin",
    "node_modules",
    "third_party",
    "vendor",
];

#[derive(Debug, Clone)]
pub struct BuildConfig {
    pub root: PathBuf,
    pub jobs: usize,
    pub chunk_size: usize,
    pub output: PathBuf,
    pub linker: Option<PathBuf>,
    pub proxy: ProxyOptions,
    pub quiet: bool,
    /// Extra directory names to skip while discovering sources (`turbo.json`).
    pub exclude_dirs: Vec<String>,
    /// Flags appended to the link command only (`turbo.json: linkFlags`).
    pub link_flags: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct BuildSummary {
    pub units: usize,
    pub chunks: usize,
    pub hits: usize,
    pub misses: usize,
    pub elapsed: Duration,
    pub output: PathBuf,
    pub build_dir: PathBuf,
}

#[derive(Debug)]
pub struct CompileFailure {
    pub unit: usize,
    pub source: PathBuf,
    pub exit_code: i32,
}

#[derive(Debug)]
pub enum BuildResult {
    Success(BuildSummary),
    CompileFailed {
        summary: BuildSummary,
        failure: CompileFailure,
    },
    LinkFailed {
        summary: BuildSummary,
        exit_code: i32,
    },
}

impl BuildResult {
    pub fn summary(&self) -> &BuildSummary {
        match self {
            BuildResult::Success(s)
            | BuildResult::CompileFailed { summary: s, .. }
            | BuildResult::LinkFailed { summary: s, .. } => s,
        }
    }
}

fn has_source_extension(path: &Path) -> bool {
    path.extension()
        .map(|e| {
            let e = e.to_string_lossy().to_ascii_lowercase();
            SOURCE_EXTENSIONS.contains(&e.as_str())
        })
        .unwrap_or(false)
}

fn should_skip_dir(name: &str, extra_excludes: &[String]) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.starts_with('.')
        || SKIP_DIRS.contains(&lower.as_str())
        || extra_excludes
            .iter()
            .any(|excluded| excluded.eq_ignore_ascii_case(&lower))
}

/// Recursively collect `.cpp`/`.cc`/... files under `root`, sorted, absolute.
///
/// `extra_excludes` are directory names (case-insensitive) from `turbo.json`.
pub fn discover_sources(root: &Path, extra_excludes: &[String]) -> TurboResult<Vec<PathBuf>> {
    if !root.is_dir() {
        return Err(TurboError::msg(format!(
            "project root `{}` is not a directory",
            root.display()
        )));
    }
    let mut sources = Vec::new();
    let mut stack = vec![util::absolute(root)];
    while let Some(dir) = stack.pop() {
        let entries = match fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };
            let file_type = match entry.file_type() {
                Ok(t) => t,
                Err(_) => continue,
            };
            // Do not follow symlinks: avoids cycles and weird duplicate paths.
            if file_type.is_symlink() {
                continue;
            }
            let path = entry.path();
            if file_type.is_dir() {
                let name = entry.file_name().to_string_lossy().to_string();
                if !should_skip_dir(&name, extra_excludes) {
                    stack.push(path);
                }
            } else if file_type.is_file() && has_source_extension(&path) {
                sources.push(util::absolute(&path));
            }
        }
    }
    sources.sort();
    Ok(sources)
}

/// `[start, end)` ranges for chunking a list of `len` items.
pub fn chunk_ranges(len: usize, chunk_size: usize) -> Vec<(usize, usize)> {
    let size = chunk_size.max(1);
    let mut ranges = Vec::new();
    let mut start = 0usize;
    while start < len {
        let end = (start + size).min(len);
        ranges.push((start, end));
        start = end;
    }
    ranges
}

/// Relative path from `base` to `target`, or `None` when they live on
/// different roots (e.g. different Windows drives).
pub fn relative_from(base: &Path, target: &Path) -> Option<PathBuf> {
    let base = util::normalize(base);
    let target = util::normalize(target);
    let base_components: Vec<Component> = base.components().collect();
    let target_components: Vec<Component> = target.components().collect();
    if base_components.is_empty() || target_components.is_empty() {
        return None;
    }
    if base_components[0] != target_components[0] {
        return None;
    }
    let mut common = 0usize;
    while common < base_components.len()
        && common < target_components.len()
        && base_components[common] == target_components[common]
    {
        common += 1;
    }
    let mut out = PathBuf::new();
    for _ in common..base_components.len() {
        out.push("..");
    }
    for component in &target_components[common..] {
        out.push(component.as_os_str());
    }
    Some(out)
}

fn unity_include_line(build_dir: &Path, source: &Path) -> String {
    let target = match relative_from(build_dir, source) {
        Some(rel) => rel,
        None => source.to_path_buf(),
    };
    format!("#include \"{}\"\n", util::include_literal(&target))
}

fn write_unity_files(
    build_dir: &Path,
    sources: &[PathBuf],
    chunk_size: usize,
) -> TurboResult<Vec<(PathBuf, PathBuf)>> {
    util::remove_dir_if_exists(build_dir)?;
    util::ensure_dir(build_dir)?;

    let ranges = chunk_ranges(sources.len(), chunk_size);
    let mut units = Vec::with_capacity(ranges.len());
    for (index, (start, end)) in ranges.iter().enumerate() {
        let mut text = String::new();
        text.push_str("// Generated by cpp-turbo-builder. Do not edit.\n");
        text.push_str("// Unity Build chunk: compiling this file is equivalent to compiling\n");
        text.push_str("// each included .cpp separately, but shared headers are parsed once.\n");
        for source in &sources[*start..*end] {
            text.push_str(&unity_include_line(build_dir, source));
        }
        let source_path = build_dir.join(format!("unity_{index}.cpp"));
        let object_path = build_dir.join(format!("unity_{index}.obj"));
        util::atomic_write(&source_path, text.as_bytes())?;
        units.push((source_path, object_path));
    }
    Ok(units)
}

/// Run the whole pipeline. Infrastructure problems come back as `Err`;
/// compiler/linker failures come back as a `BuildResult` variant carrying the
/// exit code to propagate.
pub fn build(spec: &CompilerSpec, cfg: &BuildConfig) -> TurboResult<BuildResult> {
    let started = Instant::now();
    let sources = discover_sources(&cfg.root, &cfg.exclude_dirs)?;
    if sources.is_empty() {
        return Err(TurboError::msg(format!(
            "no C/C++ sources found under `{}`",
            cfg.root.display()
        )));
    }

    let build_dir = util::absolute(&cfg.root.join(BUILD_DIR_NAME));
    let units = write_unity_files(&build_dir, &sources, cfg.chunk_size)?;
    let chunks = units.len();

    if !cfg.quiet {
        eprintln!(
            "[turbo] {} sources -> {} unity chunk(s) of up to {}, {} job(s), compiler {} ({})",
            sources.len(),
            chunks,
            cfg.chunk_size.max(1),
            cfg.jobs.max(1),
            spec.program.display(),
            spec.toolchain.name()
        );
    }

    let jobs = cfg.jobs.max(1).min(chunks.max(1));
    let cursor = AtomicUsize::new(0);
    let slots: Mutex<Vec<Option<(Outcome, u128)>>> = Mutex::new(vec![None; chunks]);
    let errors: Mutex<Vec<(usize, String)>> = Mutex::new(Vec::new());

    std::thread::scope(|scope| {
        for _ in 0..jobs {
            scope.spawn(|| loop {
                let index = cursor.fetch_add(1, Ordering::Relaxed);
                if index >= units.len() {
                    break;
                }
                let (source, object) = &units[index];
                let unit_started = Instant::now();
                let result = proxy::compile(spec, source, object, &cfg.proxy);
                let millis = unit_started.elapsed().as_millis();
                match result {
                    Ok(outcome) => {
                        let tag = match outcome {
                            Outcome::Hit => "HIT ",
                            Outcome::Miss => "MISS",
                            Outcome::Failed(_) => "FAIL",
                        };
                        if !cfg.quiet {
                            eprintln!(
                                "[turbo] [{:>3}/{:>3}] {} {} ({} ms)",
                                index + 1,
                                units.len(),
                                tag,
                                source
                                    .file_name()
                                    .map(|n| n.to_string_lossy().to_string())
                                    .unwrap_or_default(),
                                millis
                            );
                        }
                        slots.lock().unwrap()[index] = Some((outcome, millis));
                    }
                    Err(error) => {
                        errors.lock().unwrap().push((index, error.to_string()));
                    }
                }
            });
        }
    });

    let collected = slots.into_inner().unwrap_or_default();
    let infra_errors = errors.into_inner().unwrap_or_default();
    if let Some((index, message)) = infra_errors.into_iter().min_by_key(|(i, _)| *i) {
        let (source, _) = &units[index];
        return Err(TurboError::msg(format!(
            "cache proxy failed for unit {} ({}): {message}",
            index,
            source.display()
        )));
    }

    let mut hits = 0usize;
    let mut misses = 0usize;
    let mut failure: Option<CompileFailure> = None;
    for (index, slot) in collected.iter().enumerate() {
        match slot {
            Some((Outcome::Hit, _)) => hits += 1,
            Some((Outcome::Miss, _)) => misses += 1,
            Some((Outcome::Failed(code), _)) if failure.is_none() => {
                let (source, _) = &units[index];
                failure = Some(CompileFailure {
                    unit: index,
                    source: source.clone(),
                    exit_code: *code,
                });
            }
            _ => {}
        }
    }

    let summary = BuildSummary {
        units: units.len(),
        chunks,
        hits,
        misses,
        elapsed: started.elapsed(),
        output: util::absolute(&cfg.output),
        build_dir,
    };

    if let Some(failure) = failure {
        return Ok(BuildResult::CompileFailed { summary, failure });
    }

    // All units compiled: link.
    let objects: Vec<PathBuf> = units.iter().map(|(_, object)| object.clone()).collect();
    util::ensure_parent(&summary.output)?;
    let mut command = match &cfg.linker {
        Some(linker) => spec.link_command_with(linker, &objects, &summary.output),
        None => spec.link_command(&objects, &summary.output),
    };
    // Linker-only flags from `turbo.json` (libraries, /LIBPATH, ...).
    command.args(&cfg.link_flags);
    if !cfg.quiet {
        eprintln!(
            "[turbo] linking {} object(s) -> {}",
            objects.len(),
            summary.output.display()
        );
    }
    let link_started = Instant::now();
    let status = command.status().map_err(|e| {
        TurboError::msg(format!(
            "cannot run linker `{}`: {e}",
            command.get_program().to_string_lossy()
        ))
    })?;
    if !status.success() {
        return Ok(BuildResult::LinkFailed {
            summary,
            exit_code: status.code().unwrap_or(1),
        });
    }
    if !summary.output.is_file() {
        return Err(TurboError::msg(format!(
            "linker reported success but `{}` does not exist",
            summary.output.display()
        )));
    }
    if !cfg.quiet {
        eprintln!(
            "[turbo] linked in {} ms",
            link_started.elapsed().as_millis()
        );
    }

    Ok(BuildResult::Success(summary))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_cover_everything() {
        assert_eq!(chunk_ranges(0, 8), Vec::<(usize, usize)>::new());
        assert_eq!(chunk_ranges(1, 8), vec![(0, 1)]);
        assert_eq!(chunk_ranges(8, 8), vec![(0, 8)]);
        assert_eq!(chunk_ranges(9, 8), vec![(0, 8), (8, 9)]);
        assert_eq!(chunk_ranges(20, 8), vec![(0, 8), (8, 16), (16, 20)]);
        // total coverage
        let ranges = chunk_ranges(1000, 7);
        let total: usize = ranges.iter().map(|(s, e)| e - s).sum();
        assert_eq!(total, 1000);
    }

    #[test]
    fn relative_paths_for_unity_includes() {
        let base = PathBuf::from("C:/proj/.build");
        let target = PathBuf::from("C:/proj/src/a.cpp");
        assert_eq!(
            relative_from(&base, &target).unwrap(),
            Path::new("..").join("src").join("a.cpp"),
            "expected ../src/a.cpp (with the platform separator)"
        );
        // Different drives: fall back to the absolute path.
        let other = PathBuf::from("D:/proj/src/a.cpp");
        assert!(relative_from(&base, &other).is_none());
    }

    #[test]
    fn discovers_only_sources_and_skips_noise_dirs() {
        let dir = std::env::temp_dir().join(format!("turbo-disc-{}", util::unique_token()));
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::create_dir_all(dir.join("src/nested")).unwrap();
        fs::create_dir_all(dir.join("build")).unwrap();
        fs::create_dir_all(dir.join(".git")).unwrap();
        fs::create_dir_all(dir.join("target")).unwrap();
        fs::write(dir.join("src/a.cpp"), "int a();\n").unwrap();
        fs::write(dir.join("src/b.cc"), "int b();\n").unwrap();
        fs::write(dir.join("src/nested/c.cxx"), "int c();\n").unwrap();
        fs::write(dir.join("src/header.h"), "#pragma once\n").unwrap();
        fs::write(dir.join("build/generated.cpp"), "int g();\n").unwrap();
        fs::write(dir.join(".git/hook.cpp"), "int h();\n").unwrap();
        fs::write(dir.join("target/t.cpp"), "int t();\n").unwrap();

        let sources = discover_sources(&dir, &[]).unwrap();
        let names: Vec<String> = sources
            .iter()
            .map(|p| {
                p.file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default()
            })
            .collect();
        assert_eq!(names, vec!["a.cpp", "b.cc", "c.cxx"]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn extra_excludes_from_config_are_honoured() {
        let dir = std::env::temp_dir().join(format!("turbo-excl-{}", util::unique_token()));
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::create_dir_all(dir.join("tools")).unwrap();
        fs::create_dir_all(dir.join("tests")).unwrap();
        fs::write(dir.join("src/a.cpp"), "int a();\n").unwrap();
        fs::write(dir.join("tools/gen.cpp"), "int g();\n").unwrap();
        fs::write(dir.join("tests/t.cpp"), "int t();\n").unwrap();

        // Default scan sees everything.
        let all = discover_sources(&dir, &[]).unwrap();
        assert_eq!(all.len(), 3);

        // Config exclusions are case-insensitive directory names.
        let excluded = discover_sources(&dir, &["tests".to_string(), "TOOLS".to_string()]).unwrap();
        let names: Vec<String> = excluded
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert_eq!(names, vec!["a.cpp"]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn unity_file_uses_relative_includes() {
        let dir = std::env::temp_dir().join(format!("turbo-unity-{}", util::unique_token()));
        let build = dir.join(".build");
        let sources = vec![
            util::absolute(&dir.join("src/a.cpp")),
            util::absolute(&dir.join("src/b.cpp")),
        ];
        let units = write_unity_files(&build, &sources, 8).unwrap();
        assert_eq!(units.len(), 1);
        let text = fs::read_to_string(&units[0].0).unwrap();
        assert!(text.contains("#include \"../src/a.cpp\""));
        assert!(text.contains("#include \"../src/b.cpp\""));
        let _ = fs::remove_dir_all(&dir);
    }
}
