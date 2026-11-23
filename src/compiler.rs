//! Compiler abstraction: MSVC (`cl.exe`) vs GNU-style (`g++`, `clang++`).
//!
//! The tool never parses compiler output; it only needs to build a correct
//! command line and to know whether the toolchain speaks `cl`-style flags or
//! `gcc`-style flags.

use crate::util;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Toolchain {
    Msvc,
    Gnu,
}

impl Toolchain {
    pub fn name(self) -> &'static str {
        match self {
            Toolchain::Msvc => "msvc",
            Toolchain::Gnu => "gnu",
        }
    }
}

/// Everything needed to compile one translation unit.
#[derive(Debug, Clone)]
pub struct CompilerSpec {
    /// Compiler executable as the user spelled it (may be a bare name found in PATH).
    pub program: PathBuf,
    pub toolchain: Toolchain,
    /// Flags that are part of the compile command but are neither source nor output.
    pub user_flags: Vec<String>,
    /// `-I` / `/I` directories, absolute.
    pub include_dirs: Vec<PathBuf>,
    /// `/FI` / `-include` files, absolute.
    pub forced_includes: Vec<PathBuf>,
    /// Working directory for compiler invocations; relative flags resolve here.
    pub cwd: PathBuf,
}

impl CompilerSpec {
    pub fn new(program: PathBuf, user_flags: Vec<String>, cwd: PathBuf) -> Self {
        let toolchain = detect(&program);
        let mut include_dirs = Vec::new();
        let mut forced_includes = Vec::new();
        scan_search_flags(&user_flags, &cwd, &mut include_dirs, &mut forced_includes);
        CompilerSpec {
            program,
            toolchain,
            user_flags,
            include_dirs,
            forced_includes,
            cwd,
        }
    }

    /// `cl <flags> /c <src> /Fo:<obj>` or `g++ <flags> -c <src> -o <obj>`.
    pub fn compile_command(&self, source: &Path, object: &Path) -> Command {
        let mut cmd = Command::new(&self.program);
        cmd.current_dir(&self.cwd);
        match self.toolchain {
            Toolchain::Msvc => {
                cmd.args(&self.user_flags);
                cmd.arg("/nologo");
                cmd.arg("/c");
                cmd.arg(source);
                let mut fo = OsString::from("/Fo:");
                fo.push(object);
                cmd.arg(fo);
            }
            Toolchain::Gnu => {
                cmd.args(&self.user_flags);
                cmd.arg("-c");
                cmd.arg(source);
                cmd.arg("-o");
                cmd.arg(object);
            }
        }
        cmd
    }

    /// Link flags worth forwarding from the compile flags (`-lfoo`, `/LIBPATH:`).
    pub fn link_extra_flags(&self) -> Vec<String> {
        self.user_flags
            .iter()
            .filter(|flag| match self.toolchain {
                Toolchain::Msvc => {
                    let lower = flag.to_ascii_lowercase();
                    lower.starts_with("/libpath:")
                        || lower.ends_with(".lib")
                        || lower == "/debug"
                        || lower.starts_with("/subsystem:")
                        || lower.starts_with("/machine:")
                }
                Toolchain::Gnu => {
                    flag.starts_with("-l")
                        || flag.starts_with("-L")
                        || flag.starts_with("-Wl,")
                        || flag.starts_with("-framework")
                        || matches!(
                            flag.as_str(),
                            "-pthread"
                                | "-static"
                                | "-shared"
                                | "-s"
                                | "-rdynamic"
                                | "-mwindows"
                                | "-mconsole"
                        )
                }
            })
            .cloned()
            .collect()
    }

    /// Default link command: the compiler itself, in link mode.
    pub fn link_command(&self, objects: &[PathBuf], output: &Path) -> Command {
        let mut cmd = Command::new(&self.program);
        cmd.current_dir(&self.cwd);
        match self.toolchain {
            Toolchain::Msvc => {
                cmd.arg("/nologo");
                cmd.args(objects);
                let mut fe = OsString::from("/Fe:");
                fe.push(output);
                cmd.arg(fe);
            }
            Toolchain::Gnu => {
                cmd.args(objects);
                cmd.args(self.link_extra_flags());
                cmd.arg("-o");
                cmd.arg(output);
            }
        }
        cmd
    }

    /// Link command for an explicitly provided linker executable
    /// (`link.exe`, `lld-link.exe` → MSVC syntax; anything else → GCC syntax).
    pub fn link_command_with(&self, linker: &Path, objects: &[PathBuf], output: &Path) -> Command {
        let linker_toolchain = detect(linker);
        let mut cmd = Command::new(linker);
        cmd.current_dir(&self.cwd);
        match linker_toolchain {
            Toolchain::Msvc => {
                cmd.arg("/nologo");
                cmd.args(objects);
                cmd.args(self.link_extra_flags());
                let mut out = OsString::from("/OUT:");
                out.push(output);
                cmd.arg(out);
            }
            Toolchain::Gnu => {
                cmd.args(objects);
                cmd.args(self.link_extra_flags());
                cmd.arg("-o");
                cmd.arg(output);
            }
        }
        cmd
    }
}

/// Guess the flag dialect from the executable name.
pub fn detect(program: &Path) -> Toolchain {
    let stem = program
        .file_stem()
        .map(|s| s.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    match stem.as_str() {
        "cl" | "clang-cl" | "link" | "lld-link" | "cl_cl" => Toolchain::Msvc,
        _ => Toolchain::Gnu,
    }
}

/// Find the real path of a compiler so we can fingerprint it (size + mtime).
pub fn resolve_program(program: &Path, cwd: &Path) -> PathBuf {
    if program.components().count() > 1 || program.is_absolute() {
        let joined = if program.is_absolute() {
            program.to_path_buf()
        } else {
            cwd.join(program)
        };
        return util::absolute(&joined);
    }
    // Bare name: search PATH.
    let exts: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string())
            .split(';')
            .filter(|e| !e.is_empty())
            .map(|e| e.to_ascii_lowercase())
            .collect()
    } else {
        vec![String::new()]
    };
    let name = program.to_string_lossy().to_string();
    let has_ext = Path::new(&name).extension().is_some();
    if let Some(path_var) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let direct = dir.join(&name);
            if has_ext && direct.is_file() {
                return util::absolute(&direct);
            }
            if !has_ext {
                for ext in &exts {
                    let candidate = dir.join(format!("{name}{ext}"));
                    if candidate.is_file() {
                        return util::absolute(&candidate);
                    }
                }
            }
        }
    }
    program.to_path_buf()
}

fn is_source_extension(name: &str) -> bool {
    let ext = Path::new(name)
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    matches!(
        ext.as_str(),
        "c" | "cc" | "cpp" | "cxx" | "c++" | "cp" | "ipp" | "ixx"
    )
}

/// Collect `-I` / `/I` and `/FI` / `-include` values, resolving relative ones
/// against `cwd`.
pub fn scan_search_flags(
    flags: &[String],
    cwd: &Path,
    include_dirs: &mut Vec<PathBuf>,
    forced_includes: &mut Vec<PathBuf>,
) {
    let resolve = |value: &str| util::absolute(&cwd.join(value));
    let mut i = 0usize;
    while i < flags.len() {
        let flag = &flags[i];
        let lower = flag.to_ascii_lowercase();
        let next = flags.get(i + 1);

        // Separated forms: "/I dir", "-I dir", "-isystem dir", "-iquote dir".
        if matches!(lower.as_str(), "/i" | "-i" | "-isystem" | "-iquote") {
            if let Some(value) = next {
                include_dirs.push(resolve(value));
                i += 2;
                continue;
            }
            i += 1;
            continue;
        }
        // Attached forms: "-isystemdir", "-iquotedir" first (longer prefixes),
        // then "/Iinclude" / "-Iinclude".
        if let Some(rest) = strip_ci(flag, "-isystem").or_else(|| strip_ci(flag, "-iquote")) {
            if !rest.is_empty() {
                include_dirs.push(resolve(rest));
                i += 1;
                continue;
            }
        }
        if let Some(rest) = strip_ci(flag, "/i").or_else(|| strip_ci(flag, "-i")) {
            if !rest.is_empty() && !rest.starts_with('-') {
                include_dirs.push(resolve(rest));
                i += 1;
                continue;
            }
        }

        // Forced includes: "/FI file", "/FIfile", "-include file".
        if lower == "/fi" || lower == "-include" {
            if let Some(value) = next {
                forced_includes.push(resolve(value));
                i += 2;
                continue;
            }
            i += 1;
            continue;
        }
        if let Some(rest) = strip_ci(flag, "/fi") {
            if !rest.is_empty() {
                forced_includes.push(resolve(rest));
                i += 1;
                continue;
            }
        }
        i += 1;
    }
}

fn strip_ci<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    if text.len() >= prefix.len() && text[..prefix.len()].eq_ignore_ascii_case(prefix) {
        Some(&text[prefix.len()..])
    } else {
        None
    }
}

/// What `turbo fastcache -- cl /c file.cpp /Fo:out.obj` means.
#[derive(Debug, Clone)]
pub struct ParsedInvocation {
    pub source: PathBuf,
    pub object: PathBuf,
    pub user_flags: Vec<String>,
}

fn is_option_taking_value(lower: &str) -> bool {
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
            | "/fu"
            | "-u"
            | "/link"
            | "-x"
            | "/fpp"
            | "-arch"
    )
}

/// Split a compiler command line into source, object and "other flags".
///
/// Both dialects are understood:
/// - MSVC: `/c src.cpp /Fo:out.obj`, `/Foout.obj`
/// - GCC : `-c src.cpp -o out.o`, `-oout.o`
pub fn parse_invocation(args: &[String]) -> Result<ParsedInvocation, String> {
    let mut source: Option<PathBuf> = None;
    let mut object: Option<PathBuf> = None;
    let mut user_flags: Vec<String> = Vec::new();
    let mut i = 0usize;

    while i < args.len() {
        let arg = &args[i];
        let lower = arg.to_ascii_lowercase();
        let next = args.get(i + 1);

        // Output: MSVC "/Fo..." (attached, with or without ':', or separated).
        if let Some(rest) = strip_ci(arg, "/fo") {
            let rest = rest.strip_prefix(':').unwrap_or(rest);
            if rest.is_empty() {
                if let Some(value) = next {
                    object = Some(PathBuf::from(value));
                    i += 2;
                    continue;
                }
            } else {
                object = Some(PathBuf::from(rest));
                i += 1;
                continue;
            }
        }
        // Structural flags are added back by the compiler abstraction.
        if lower == "/c" || lower == "-c" {
            i += 1;
            continue;
        }
        // Output: GCC "-o path" / "-opath".
        if arg == "-o" {
            if let Some(value) = next {
                object = Some(PathBuf::from(value));
                i += 2;
                continue;
            }
            return Err("-o without a path".to_string());
        }
        if let Some(rest) = arg.strip_prefix("-o") {
            if !rest.is_empty() && !rest.starts_with('-') {
                object = Some(PathBuf::from(rest));
                i += 1;
                continue;
            }
        }

        // Options whose value is a separate token: keep both in user_flags.
        if is_option_taking_value(&lower) {
            user_flags.push(arg.clone());
            if let Some(value) = next {
                user_flags.push(value.clone());
                i += 2;
            } else {
                i += 1;
            }
            continue;
        }

        let looks_like_flag = arg.starts_with('-') || arg.starts_with('/');
        if !looks_like_flag && is_source_extension(arg) && source.is_none() {
            source = Some(PathBuf::from(arg));
            i += 1;
            continue;
        }

        user_flags.push(arg.clone());
        i += 1;
    }

    let source = source.ok_or_else(|| {
        "no source file found in the command line (expected something like src.cpp)".to_string()
    })?;
    let object = object.unwrap_or_else(|| source.with_extension("obj"));
    Ok(ParsedInvocation {
        source,
        object,
        user_flags,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn detects_dialects() {
        assert_eq!(detect(Path::new("cl.exe")), Toolchain::Msvc);
        assert_eq!(detect(Path::new("C:/tools/cl")), Toolchain::Msvc);
        assert_eq!(detect(Path::new("clang-cl.exe")), Toolchain::Msvc);
        assert_eq!(detect(Path::new("g++.exe")), Toolchain::Gnu);
        assert_eq!(detect(Path::new("clang++")), Toolchain::Gnu);
    }

    #[test]
    fn parses_msvc_line() {
        let parsed = parse_invocation(&args(&[
            "/nologo",
            "/c",
            "src\\a.cpp",
            "/Fo:out\\a.obj",
            "/O2",
        ]))
        .unwrap();
        assert_eq!(parsed.source, PathBuf::from("src\\a.cpp"));
        assert_eq!(parsed.object, PathBuf::from("out\\a.obj"));
        assert!(parsed.user_flags.contains(&"/O2".to_string()));
        assert!(!parsed.user_flags.contains(&"src\\a.cpp".to_string()));
    }

    #[test]
    fn parses_gcc_line() {
        let parsed =
            parse_invocation(&args(&["-c", "src/a.cpp", "-o", "out/a.o", "-std=c++20"])).unwrap();
        assert_eq!(parsed.source, PathBuf::from("src/a.cpp"));
        assert_eq!(parsed.object, PathBuf::from("out/a.o"));
        assert_eq!(parsed.user_flags, args(&["-std=c++20"]));
    }

    #[test]
    fn derives_object_name_when_missing() {
        let parsed = parse_invocation(&args(&["/c", "src/a.cpp"])).unwrap();
        assert_eq!(parsed.object, PathBuf::from("src/a.obj"));
    }

    #[test]
    fn keeps_include_flags_with_their_values() {
        let parsed = parse_invocation(&args(&[
            "/I",
            "include",
            "/DX=1",
            "/c",
            "a.cpp",
            "/Fo:a.obj",
        ]))
        .unwrap();
        assert_eq!(parsed.user_flags, args(&["/I", "include", "/DX=1"]));
    }

    #[test]
    fn scan_finds_include_dirs_both_styles() {
        let cwd = PathBuf::from("C:\\proj");
        let flags = args(&[
            "/Iinclude",
            "-I",
            "third_party",
            "-isystem",
            "sys",
            "-I/abs/path",
        ]);
        let mut dirs = Vec::new();
        let mut forced = Vec::new();
        scan_search_flags(&flags, &cwd, &mut dirs, &mut forced);
        let joined: Vec<String> = dirs
            .iter()
            .map(|d| d.to_string_lossy().to_string())
            .collect();
        assert!(joined
            .iter()
            .any(|d| d.ends_with("proj\\include") || d.ends_with("proj/include")));
        assert!(joined.iter().any(|d| d.ends_with("third_party")));
        assert!(joined.iter().any(|d| d.ends_with("sys")));
        assert!(joined
            .iter()
            .any(|d| d.ends_with("abs\\path") || d.ends_with("abs/path")));
    }
}
