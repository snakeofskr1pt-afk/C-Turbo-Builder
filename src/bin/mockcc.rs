//! `mockcc` — a fake C/C++ compiler + linker used by the test suite.
//!
//! The point: prove the caching, invalidation and parallel-build logic without
//! requiring MSVC or GCC on the machine. It understands the subset of `cl`/`gcc`
//! command lines that `turbo` generates, writes deterministic fake object files
//! and appends every invocation to `$TURBO_MOCK_LOG` (tab separated) so tests
//! can count how often the "compiler" really ran.

use std::collections::HashSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use turbo::sha256;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Err(error) = run(&args) {
        eprintln!("mockcc: error: {error}");
        std::process::exit(1);
    }
}

fn strip_ci<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    if text.len() >= prefix.len() && text[..prefix.len()].eq_ignore_ascii_case(prefix) {
        Some(&text[prefix.len()..])
    } else {
        None
    }
}

fn is_source(path: &str) -> bool {
    Path::new(path)
        .extension()
        .map(|e| {
            let e = e.to_string_lossy().to_ascii_lowercase();
            matches!(
                e.as_str(),
                "c" | "cc" | "cpp" | "cxx" | "c++" | "cp" | "ipp" | "ixx"
            )
        })
        .unwrap_or(false)
}

fn is_object(path: &str) -> bool {
    Path::new(path)
        .extension()
        .map(|e| {
            let e = e.to_string_lossy().to_ascii_lowercase();
            matches!(e.as_str(), "obj" | "o")
        })
        .unwrap_or(false)
}

/// Options whose value is the following token and must not be mistaken for a
/// source/object path.
fn takes_separate_value(lower: &str) -> bool {
    matches!(
        lower,
        "/i" | "-i"
            | "/d"
            | "-d"
            | "/fi"
            | "-include"
            | "-isystem"
            | "-iquote"
            | "/fd"
            | "-mf"
            | "-mt"
            | "-l"
            | "-lpath"
            | "-u"
            | "-x"
    )
}

#[derive(Default)]
struct ParsedArgs {
    compile_mode: bool,
    positional: Vec<String>,
    object: Option<PathBuf>,
    executable: Option<PathBuf>,
}

fn parse_args(args: &[String]) -> ParsedArgs {
    let mut parsed = ParsedArgs::default();
    let mut index = 0usize;
    while index < args.len() {
        let arg = &args[index];
        let lower = arg.to_ascii_lowercase();

        if lower == "/c" || lower == "-c" {
            parsed.compile_mode = true;
            index += 1;
            continue;
        }
        // MSVC /Fo (object) — attached or separated.
        if lower == "/fo" {
            if let Some(value) = args.get(index + 1) {
                parsed.object = Some(PathBuf::from(value));
                index += 2;
                continue;
            }
        }
        if let Some(rest) = strip_ci(arg, "/fo") {
            if !rest.is_empty() {
                parsed.object = Some(PathBuf::from(rest));
                index += 1;
                continue;
            }
        }
        // MSVC /Fe and link.exe /OUT: (executable).
        if let Some(rest) = strip_ci(arg, "/fe") {
            if !rest.is_empty() {
                parsed.executable = Some(PathBuf::from(rest));
                index += 1;
                continue;
            }
        }
        if let Some(rest) = strip_ci(arg, "/out:") {
            if !rest.is_empty() {
                parsed.executable = Some(PathBuf::from(rest));
                index += 1;
                continue;
            }
        }
        // GCC -o (object or executable depending on mode).
        if arg == "-o" {
            if let Some(value) = args.get(index + 1) {
                parsed.object = Some(PathBuf::from(value));
                index += 2;
                continue;
            }
        }
        if let Some(rest) = arg.strip_prefix("-o") {
            if !rest.is_empty() && !rest.starts_with('-') {
                parsed.object = Some(PathBuf::from(rest));
                index += 1;
                continue;
            }
        }
        if takes_separate_value(&lower) {
            index += 2;
            continue;
        }
        if arg.starts_with('-') || arg.starts_with('/') {
            index += 1;
            continue;
        }
        parsed.positional.push(arg.clone());
        index += 1;
    }
    parsed
}

fn run(args: &[String]) -> Result<(), String> {
    let parsed = parse_args(args);
    if is_preprocess_only(args) {
        preprocess(&parsed)
    } else if parsed.compile_mode {
        compile(&parsed)
    } else {
        link(&parsed)
    }
}

/// `-E`, `-M`, `/P`, ... — never produce an object file.
fn is_preprocess_only(args: &[String]) -> bool {
    args.iter().any(|arg| {
        matches!(
            arg.to_ascii_lowercase().as_str(),
            "/p" | "/ep" | "/e" | "-e" | "-m" | "-mm" | "-mg" | "-mmd"
        )
    })
}

fn preprocess(parsed: &ParsedArgs) -> Result<(), String> {
    let source = parsed
        .positional
        .iter()
        .find(|entry| is_source(entry))
        .ok_or_else(|| "preprocess mode but no source file given".to_string())?;
    let source_path = PathBuf::from(source);
    let (combined, inputs) = collect_translation_unit(&source_path)?;
    match parsed.object.clone().or_else(|| parsed.executable.clone()) {
        Some(output) => {
            if let Some(parent) = output.parent() {
                if !parent.as_os_str().is_empty() {
                    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                }
            }
            fs::write(&output, &combined)
                .map_err(|e| format!("cannot write `{}`: {e}", output.display()))?;
            log(
                "P",
                &format!("{}\t{}\t{inputs}", source_path.display(), output.display()),
            );
        }
        None => {
            print!("{}", String::from_utf8_lossy(&combined));
            log("P", &format!("{}\t-\t{inputs}", source_path.display()));
        }
    }
    Ok(())
}

fn contains_error_marker(data: &[u8]) -> bool {
    let needle = b"#error MOCK";
    data.windows(needle.len()).any(|window| window == needle)
}

/// Quote-style includes only — enough to walk Unity Build chunks
/// (`#include "../src/a.cpp"`) like a real preprocessor would.
fn quoted_includes(data: &[u8]) -> Vec<String> {
    let mut found = Vec::new();
    let mut index = 0usize;
    while index < data.len() {
        if data[index] != b'#' {
            index += 1;
            continue;
        }
        let rest = &data[index + 1..];
        let mut cursor = 0usize;
        while cursor < rest.len() && (rest[cursor] == b' ' || rest[cursor] == b'\t') {
            cursor += 1;
        }
        let keyword = b"include";
        if rest.len() >= cursor + keyword.len() && &rest[cursor..cursor + keyword.len()] == keyword
        {
            let mut open = cursor + keyword.len();
            while open < rest.len() && (rest[open] == b' ' || rest[open] == b'\t') {
                open += 1;
            }
            if open < rest.len() && rest[open] == b'"' {
                let start = open + 1;
                let mut end = start;
                while end < rest.len()
                    && rest[end] != b'"'
                    && rest[end] != b'\n'
                    && rest[end] != b'\r'
                {
                    end += 1;
                }
                if end < rest.len() && rest[end] == b'"' && end > start {
                    found.push(String::from_utf8_lossy(&rest[start..end]).into_owned());
                }
            }
        }
        index += 1;
    }
    found
}

/// Flatten a translation unit the way a real compiler would: the file itself
/// plus every transitively `"..."`-included file. Mock objects are hashed over
/// this combined text, so editing an included header changes the object.
fn collect_translation_unit(root: &Path) -> Result<(Vec<u8>, usize), String> {
    let mut combined = Vec::new();
    let mut visited: HashSet<String> = HashSet::new();
    let mut stack: Vec<(PathBuf, usize)> = vec![(root.to_path_buf(), 0usize)];
    let mut inputs = 0usize;

    while let Some((path, depth)) = stack.pop() {
        let key = path.to_string_lossy().to_ascii_lowercase();
        if !visited.insert(key) {
            continue;
        }
        let data = fs::read(&path).map_err(|e| format!("cannot read `{}`: {e}", path.display()))?;
        if contains_error_marker(&data) {
            return Err(format!(
                "compile error in `{}`: simulated #error MOCK",
                path.display()
            ));
        }
        combined.extend_from_slice(&data);
        combined.push(b'\n');
        inputs += 1;
        if depth >= 32 {
            continue;
        }
        let parent = path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        for include in quoted_includes(&data) {
            let candidate = parent.join(&include);
            if candidate.is_file() {
                stack.push((candidate, depth + 1));
            }
        }
    }
    Ok((combined, inputs))
}

/// Simulate the cost of a real compiler/linker so benchmarks can model a heavy
/// C++ build (`TURBO_MOCK_COMPILE_MS`, `TURBO_MOCK_LINK_MS`). Tests never set
/// these, so the suite stays fast.
fn simulated_delay(variable: &str) {
    if let Ok(raw) = std::env::var(variable) {
        if let Ok(milliseconds) = raw.trim().parse::<u64>() {
            if milliseconds > 0 {
                std::thread::sleep(std::time::Duration::from_millis(milliseconds));
            }
        }
    }
}

fn compile(parsed: &ParsedArgs) -> Result<(), String> {
    let source = parsed
        .positional
        .iter()
        .find(|p| is_source(p))
        .ok_or_else(|| "compile mode but no source file given".to_string())?;
    let source_path = PathBuf::from(source);
    // Log the attempt before doing any work: tests count "C" lines to prove
    // whether the cache proxy really invoked the compiler (success or failure).
    log(
        "C",
        &format!(
            "{}\t{}",
            source_path.display(),
            parsed
                .object
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default()
        ),
    );
    simulated_delay("TURBO_MOCK_COMPILE_MS");
    let (combined, inputs) = collect_translation_unit(&source_path)?;
    let object = parsed
        .object
        .clone()
        .ok_or_else(|| "compile mode but no output path (/Fo: or -o)".to_string())?;
    if let Some(parent) = object.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
    }
    let digest = sha256::hex(&sha256::hash_bytes(&combined));
    let body = format!(
        "mockcc object v1\nsource={}\ninputs={inputs}\nsize={}\nsha256={digest}\n",
        source_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default(),
        combined.len()
    );
    fs::write(&object, body).map_err(|e| format!("cannot write `{}`: {e}", object.display()))?;
    Ok(())
}

fn link(parsed: &ParsedArgs) -> Result<(), String> {
    let objects: Vec<PathBuf> = parsed
        .positional
        .iter()
        .filter(|p| is_object(p))
        .map(PathBuf::from)
        .collect();
    if objects.is_empty() {
        return Err("link mode but no object files given".to_string());
    }
    let output = parsed
        .executable
        .clone()
        .or_else(|| parsed.object.clone())
        .ok_or_else(|| "link mode but no output path (/Fe:, /OUT: or -o)".to_string())?;
    if let Some(parent) = output.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
    }
    simulated_delay("TURBO_MOCK_LINK_MS");

    let mut combined = sha256::Sha256::new();
    let mut lines = String::new();
    for object in &objects {
        let data = fs::read(object)
            .map_err(|e| format!("cannot read object `{}`: {e}", object.display()))?;
        combined.update(&data);
        lines.push_str(&format!(
            "object={}\n",
            object
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default()
        ));
    }
    let body = format!(
        "mockcc executable v1\nobjects={}\nlinked_sha256={}\n{lines}",
        objects.len(),
        combined.hex()
    );
    fs::write(&output, body).map_err(|e| format!("cannot write `{}`: {e}", output.display()))?;
    log("L", &format!("{}\t{}", objects.len(), output.display()));
    Ok(())
}

/// Append `kind<TAB>detail` to `$TURBO_MOCK_LOG`, if set. One `write_all` per
/// line keeps parallel invocations from interleaving inside a line.
fn log(kind: &str, detail: &str) {
    if let Some(path) = std::env::var_os("TURBO_MOCK_LOG") {
        if path.is_empty() {
            return;
        }
        let line = format!("{kind}\t{detail}\n");
        if let Ok(mut file) = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(Path::new(&path))
        {
            let _ = file.write_all(line.as_bytes());
        }
    }
}
