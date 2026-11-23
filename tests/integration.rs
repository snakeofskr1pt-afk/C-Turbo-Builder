//! End-to-end tests for the two stages:
//!
//! * Stage 1 — `turbo fastcache`: hit copies the object, miss runs the compiler,
//!   headers and flags bust the cache, failures are not cached.
//! * Stage 2 — `turbo build`: sources are discovered, glued into Unity Build
//!   chunks, compiled in parallel and linked into an executable.
//!
//! The real compiler is replaced by `mockcc` (a binary built from this same
//! package), so the suite runs on a machine with no C++ toolchain installed.
//! `mockcc` appends one line per invocation to `$TURBO_MOCK_LOG`, which is how
//! these tests prove that a cache hit actually skipped compilation.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

const TURBO: &str = env!("CARGO_BIN_EXE_turbo");
const MOCKCC: &str = env!("CARGO_BIN_EXE_mockcc");

struct Sandbox {
    dir: PathBuf,
    cache: PathBuf,
    log: PathBuf,
}

impl Sandbox {
    fn new(tag: &str) -> Sandbox {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir =
            std::env::temp_dir().join(format!("turbo-it-{tag}-{}-{nanos}", std::process::id()));
        fs::create_dir_all(&dir).expect("create sandbox");
        let cache = dir.join(".test-cache");
        let log = dir.join("mockcc.log");
        Sandbox { dir, cache, log }
    }

    fn write(&self, relative: &str, content: &str) -> PathBuf {
        let path = self.dir.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent dirs");
        }
        fs::write(&path, content).expect("write fixture file");
        path
    }

    /// A `turbo` command that is isolated from the developer's real cache and
    /// deterministic regardless of environment.
    fn command(&self) -> Command {
        let mut command = Command::new(TURBO);
        command
            .current_dir(&self.dir)
            .env("TURBO_CACHE_DIR", &self.cache)
            .env("TURBO_MOCK_LOG", &self.log)
            .env_remove("TURBO_VERBOSE")
            .env_remove("TURBO_JOBS")
            .env_remove("TURBO_EXTRA_KEY");
        command
    }

    /// `compiler_flags` are appended *after* the compiler, i.e. they belong to
    /// the compiler command line, not to `turbo` itself.
    fn build(&self, compiler_flags: &[&str]) -> Output {
        let mut command = self.command();
        command
            .args([
                "build",
                "--root",
                ".",
                "--jobs",
                "2",
                "--chunk-size",
                "2",
                "--out",
                "app.exe",
                "--",
            ])
            .arg(MOCKCC)
            .args(compiler_flags);
        command.output().expect("run turbo build")
    }

    fn log_lines(&self) -> Vec<String> {
        fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .map(|line| line.to_string())
            .collect()
    }

    fn compile_count(&self) -> usize {
        self.log_lines()
            .iter()
            .filter(|line| line.starts_with("C\t"))
            .count()
    }

    fn link_count(&self) -> usize {
        self.log_lines()
            .iter()
            .filter(|line| line.starts_with("L\t"))
            .count()
    }

    fn executable(&self) -> PathBuf {
        self.dir.join("app.exe")
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).to_string()
}

/// How many cached objects are in the cache directory.
fn obj_count(cache: &Path) -> usize {
    fs::read_dir(cache)
        .map(|entries| {
            entries
                .filter_map(|entry| entry.ok())
                .filter(|entry| {
                    entry
                        .path()
                        .extension()
                        .map(|ext| ext.eq_ignore_ascii_case("obj"))
                        .unwrap_or(false)
                })
                .count()
        })
        .unwrap_or(0)
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "command failed with {:?}\nstderr:\n{}",
        output.status.code(),
        stderr_of(output)
    );
}

/// 3 sources, a shared header, chunk size 2 → 2 Unity Build chunks.
fn seed_project(sandbox: &Sandbox) {
    sandbox.write("include/common.h", "#pragma once\n#define VALUE 1\n");
    for name in ["a", "b", "c"] {
        sandbox.write(
            &format!("src/{name}.cpp"),
            "#include \"../include/common.h\"\nint value(){return VALUE;}\n",
        );
    }
}

#[test]
fn build_then_reuse_then_invalidate() {
    let sandbox = Sandbox::new("flow");
    seed_project(&sandbox);
    let flags = ["-I", "include"];

    // Cold build: 3 sources in 2 chunks → exactly 2 compiler runs, 1 link.
    let output = sandbox.build(&flags);
    assert_success(&output);
    assert!(
        sandbox.executable().is_file(),
        "executable was not produced"
    );
    assert_eq!(
        sandbox.compile_count(),
        2,
        "cold build must compile each chunk"
    );
    assert_eq!(sandbox.link_count(), 1);

    // Hot build: every chunk must come from the cache.
    let output = sandbox.build(&flags);
    assert_success(&output);
    assert_eq!(
        sandbox.compile_count(),
        2,
        "hot build must not invoke the compiler at all"
    );
    let stderr = stderr_of(&output);
    assert!(
        stderr.contains("2 cache hit(s)"),
        "expected a fully cached summary, got:\n{stderr}"
    );

    // Touching a header that both chunks include → both chunks recompile.
    sandbox.write("include/common.h", "#pragma once\n#define VALUE 2\n");
    let output = sandbox.build(&flags);
    assert_success(&output);
    assert_eq!(
        sandbox.compile_count(),
        4,
        "header change must invalidate every chunk that includes it"
    );

    // Changing a compiler flag → new keys, everything recompiles.
    let mut flags_with_define = flags.to_vec();
    flags_with_define.extend(["-DEXTRA_SALT=1"]);
    let output = sandbox.build(&flags_with_define);
    assert_success(&output);
    assert_eq!(
        sandbox.compile_count(),
        6,
        "flag change must invalidate the whole project"
    );

    // And a hot run with the new flags is again free.
    let output = sandbox.build(&flags_with_define);
    assert_success(&output);
    assert_eq!(sandbox.compile_count(), 6);
}

#[test]
fn unreferenced_header_does_not_invalidate() {
    let sandbox = Sandbox::new("nodep");
    seed_project(&sandbox);
    sandbox.write("include/unused.h", "#pragma once\n#define NEVER_USED 1\n");
    let flags = ["-I", "include"];

    assert_success(&sandbox.build(&flags));
    assert_eq!(sandbox.compile_count(), 2);

    sandbox.write("include/unused.h", "#pragma once\n#define NEVER_USED 2\n");
    assert_success(&sandbox.build(&flags));
    assert_eq!(
        sandbox.compile_count(),
        2,
        "editing an unreferenced header must not bust the cache"
    );
}

#[test]
fn fastcache_proxy_hits_and_copies_object() {
    let sandbox = Sandbox::new("proxy");
    sandbox.write("src/inc.h", "#pragma once\n#define K 1\n");
    sandbox.write("src/unit.cpp", "#include \"inc.h\"\nint f(){return K;}\n");

    let run = || {
        let mut command = sandbox.command();
        command
            .args(["fastcache", "--cache-dir"])
            .arg(&sandbox.cache)
            .arg("--")
            .arg(MOCKCC)
            .args(["-I", "src", "-c", "src/unit.cpp", "-o", "unit.obj"]);
        command.output().expect("run fastcache")
    };

    let first = run();
    assert_success(&first);
    assert_eq!(sandbox.compile_count(), 1);
    let first_object = fs::read(sandbox.dir.join("unit.obj")).expect("object exists");

    // Corrupt the output: a cache hit must restore it verbatim.
    sandbox.write("unit.obj", "tampered");
    let second = run();
    assert_success(&second);
    assert_eq!(sandbox.compile_count(), 1, "second run must be a cache hit");
    assert_eq!(
        fs::read(sandbox.dir.join("unit.obj")).expect("object exists"),
        first_object,
        "cache hit must copy the cached object bytes"
    );

    // A header edit flips the key → compiler runs again, and the object bytes
    // change because mockcc hashes the whole translation unit it sees.
    sandbox.write("src/inc.h", "#pragma once\n#define K 2\n");
    let third = run();
    assert_success(&third);
    assert_eq!(
        sandbox.compile_count(),
        2,
        "header edit must miss the cache"
    );
    assert_ne!(
        fs::read(sandbox.dir.join("unit.obj")).expect("object exists"),
        first_object,
        "a recompiled unit reflects the new header content"
    );
}

#[test]
fn fastcache_passes_through_non_compile_invocations() {
    let sandbox = Sandbox::new("passthrough");
    sandbox.write("src/a.cpp", "int main(){return 0;}\n");

    // 1) A real compile goes through the cache.
    let mut command = sandbox.command();
    command
        .args(["fastcache", "--cache-dir"])
        .arg(&sandbox.cache)
        .arg("--")
        .arg(MOCKCC)
        .args(["-c", "src/a.cpp", "-o", "a.obj"]);
    assert_success(&command.output().expect("run fastcache"));
    assert_eq!(sandbox.compile_count(), 1);
    let cached_before = obj_count(&sandbox.cache);
    assert_eq!(cached_before, 1);

    // 2) Linking has no `-c`/`/c`: it must be forwarded verbatim, never cached.
    let mut command = sandbox.command();
    command
        .args(["fastcache", "--cache-dir"])
        .arg(&sandbox.cache)
        .arg("--")
        .arg(MOCKCC)
        .args(["a.obj", "-o", "linked.exe"]);
    assert_success(&command.output().expect("run fastcache"));
    assert!(sandbox.dir.join("linked.exe").is_file());
    assert_eq!(sandbox.link_count(), 1, "the linker must have run");
    assert_eq!(sandbox.compile_count(), 1, "linking must not compile");
    assert_eq!(
        obj_count(&sandbox.cache),
        cached_before,
        "linking must not touch the cache"
    );

    // 3) Preprocess-only (-E) is forwarded too: output produced, nothing cached.
    let mut command = sandbox.command();
    command
        .args(["fastcache", "--cache-dir"])
        .arg(&sandbox.cache)
        .arg("--")
        .arg(MOCKCC)
        .args(["-E", "src/a.cpp", "-o", "a.i"]);
    assert_success(&command.output().expect("run fastcache"));
    assert!(
        sandbox.dir.join("a.i").is_file(),
        "preprocessed output expected"
    );
    assert_eq!(sandbox.compile_count(), 1);
    assert_eq!(obj_count(&sandbox.cache), cached_before);
}

#[test]
fn compile_failure_is_propagated_and_not_cached() {
    let sandbox = Sandbox::new("fail");
    sandbox.write("src/bad.cpp", "#error MOCK cannot compile this\n");

    let output = sandbox.build(&[]);
    assert!(!output.status.success(), "build must fail");
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(sandbox.compile_count(), 1);
    assert!(!sandbox.executable().is_file());

    // Failures must not be cached: the compiler is asked again.
    let output = sandbox.build(&[]);
    assert!(!output.status.success());
    assert_eq!(
        sandbox.compile_count(),
        2,
        "failed compilations must never be cached"
    );

    // Fix the source: now it builds and caches.
    sandbox.write("src/bad.cpp", "int ok(){return 0;}\n");
    let output = sandbox.build(&[]);
    assert_success(&output);
    assert!(sandbox.executable().is_file());
}

#[test]
fn hash_command_is_deterministic() {
    let sandbox = Sandbox::new("hash");
    sandbox.write("src/one.cpp", "int x = 1;\n");

    let run = || {
        let mut command = sandbox.command();
        command
            .arg("hash")
            .arg("--")
            .arg(MOCKCC)
            .args(["-c", "src/one.cpp", "-o", "one.obj"]);
        command.output().expect("run turbo hash")
    };

    let first = run();
    assert_success(&first);
    let second = run();
    assert_success(&second);
    assert_eq!(first.stdout, second.stdout, "the key must be stable");

    let text = String::from_utf8_lossy(&first.stdout);
    let line = text
        .lines()
        .find(|line| line.starts_with("cache key: "))
        .expect("hash prints a cache key");
    let key = line.trim_start_matches("cache key: ");
    assert_eq!(
        key.len(),
        64,
        "key must be a full SHA-256 hex string: {key}"
    );
    assert!(key.chars().all(|c| c.is_ascii_hexdigit()));
}

#[test]
fn cache_stats_and_clear() {
    let sandbox = Sandbox::new("cache");
    seed_project(&sandbox);
    assert_success(&sandbox.build(&["-I", "include"]));

    let stats = |sandbox: &Sandbox| {
        let mut command = sandbox.command();
        command
            .args(["cache", "stats", "--cache-dir"])
            .arg(&sandbox.cache);
        command.output().expect("run cache stats")
    };

    let output = stats(&sandbox);
    assert_success(&output);
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        text.contains("entries: 2"),
        "expected 2 cached objects, got:\n{text}"
    );

    let mut clear = sandbox.command();
    clear
        .args(["cache", "clear", "--cache-dir"])
        .arg(&sandbox.cache);
    assert_success(&clear.output().expect("run cache clear"));

    let output = stats(&sandbox);
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        text.contains("entries: 0"),
        "cache must be empty, got:\n{text}"
    );
}

#[test]
fn parallel_build_uses_multiple_workers() {
    // 6 sources, chunk size 1, 4 jobs: all chunks are independent, so the build
    // must succeed regardless of scheduling and produce a single executable.
    let sandbox = Sandbox::new("parallel");
    for name in ["p0", "p1", "p2", "p3", "p4", "p5"] {
        sandbox.write(&format!("src/{name}.cpp"), "int f(){return 0;}\n");
    }
    let mut command = sandbox.command();
    command
        .args([
            "build",
            "--root",
            ".",
            "--jobs",
            "4",
            "--chunk-size",
            "1",
            "--out",
            "app.exe",
        ])
        .arg("--")
        .arg(MOCKCC);
    let output = command.output().expect("run turbo build");
    assert_success(&output);
    assert_eq!(sandbox.compile_count(), 6);
    assert_eq!(sandbox.link_count(), 1);
    assert!(sandbox.executable().is_file());
    let exe = fs::read_to_string(sandbox.executable()).expect("read executable");
    assert!(
        exe.contains("objects=6"),
        "linker must receive all 6 objects:\n{exe}"
    );
}

#[test]
fn turbo_json_drives_build_and_cli_wins() {
    let sandbox = Sandbox::new("config");
    sandbox.write("include/cfg.h", "#pragma once\n#define CFG 1\n");
    sandbox.write("src/a.cpp", "#include <cfg.h>\nint a(){return CFG;}\n");
    sandbox.write("src/b.cpp", "#include <cfg.h>\nint b(){return CFG;}\n");
    sandbox.write("src/c.cpp", "int c(){return 0;}\n");
    // Excluded through turbo.json: must not appear in the build.
    sandbox.write("tools/gen.cpp", "int gen(){return 1;}\n");

    let mockcc_json = MOCKCC.replace('\\', "\\\\");
    let config = format!(
        r#"{{
            "compiler": "{mockcc_json}",
            "include": ["include"],
            "defines": {{ "NDEBUG": null }},
            "exclude": ["tools"],
            "chunkSize": 1,
            "jobs": 2,
            "out": "cfg.exe"
        }}"#
    );
    sandbox.write("turbo.json", &config);

    // No `--` at all: compiler, include dir, chunk size and output come from
    // turbo.json; three sources (tools/ is excluded) at chunk size 1.
    let mut command = sandbox.command();
    command.args(["build", "--root", "."]);
    let output = command.output().expect("run turbo build");
    assert_success(&output);
    assert_eq!(sandbox.compile_count(), 3, "chunk size 1 → 3 compilations");
    assert!(
        sandbox.dir.join("cfg.exe").is_file(),
        "output name must come from turbo.json"
    );

    // Second run: fully cached.
    let mut command = sandbox.command();
    command.args(["build", "--root", "."]);
    assert_success(&command.output().expect("run turbo build"));
    assert_eq!(sandbox.compile_count(), 3, "config build must be cacheable");

    // The config's include dir really is used: editing <cfg.h> invalidates the
    // two chunks that include it (a and b), not the one that does not (c).
    sandbox.write("include/cfg.h", "#pragma once\n#define CFG 2\n");
    let mut command = sandbox.command();
    command.args(["build", "--root", "."]);
    assert_success(&command.output().expect("run turbo build"));
    assert_eq!(
        sandbox.compile_count(),
        5,
        "header reached through the config's -I must invalidate its dependents"
    );

    // Command line overrides the config (chunk size 2, different output).
    let mut command = sandbox.command();
    command
        .args([
            "build",
            "--root",
            ".",
            "--chunk-size",
            "2",
            "--out",
            "cli.exe",
            "--",
        ])
        .arg(MOCKCC);
    assert_success(&command.output().expect("run turbo build"));
    assert!(
        sandbox.dir.join("cli.exe").is_file(),
        "CLI --out must beat turbo.json out"
    );
    assert_eq!(
        sandbox.compile_count(),
        7,
        "chunk size 2 → two new chunks on top of the five compilations"
    );

    // --no-config drops turbo.json: no compiler anywhere → usage error.
    let mut command = sandbox.command();
    command.args(["build", "--root", ".", "--no-config"]);
    let output = command.output().expect("run turbo build");
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn broken_turbo_json_is_reported() {
    let sandbox = Sandbox::new("badconfig");
    sandbox.write("src/a.cpp", "int a();\n");
    sandbox.write("turbo.json", "{ this is not json");

    let mut command = sandbox.command();
    command.args(["build", "--root", "."]);
    let output = command.output().expect("run turbo build");
    assert_eq!(output.status.code(), Some(2));
    let stderr = stderr_of(&output);
    assert!(
        stderr.contains("turbo.json"),
        "error must name the config file, got:\n{stderr}"
    );
}

#[test]
fn directory_argument_builds_like_drag_and_drop() {
    // `turbo C:\project` (or dropping a folder onto turbo.exe) is shorthand for
    // `turbo build --root C:\project`.
    let sandbox = Sandbox::new("drop");
    sandbox.write("src/a.cpp", "int main(){return 0;}\n");

    let mut command = sandbox.command();
    command.arg(&sandbox.dir).arg("--").arg(MOCKCC);
    let output = command.output().expect("run turbo <dir>");
    assert_success(&output);
    assert!(sandbox.executable().is_file());
    assert_eq!(sandbox.compile_count(), 1);
}

#[test]
fn positional_root_after_build_subcommand() {
    let sandbox = Sandbox::new("posroot");
    sandbox.write("src/a.cpp", "int main(){return 0;}\n");

    let mut command = sandbox.command();
    command.arg("build").arg(&sandbox.dir).arg("--").arg(MOCKCC);
    let output = command.output().expect("run turbo build <dir>");
    assert_success(&output);
    assert!(sandbox.executable().is_file());
}

#[test]
fn pause_flag_does_not_hang_without_stdin() {
    // `--pause` is for launchers (double-click, drag & drop): it waits for
    // Enter. With a closed stdin it must return immediately.
    let sandbox = Sandbox::new("pause");
    sandbox.write("src/a.cpp", "int main(){return 0;}\n");

    let mut command = sandbox.command();
    command.stdin(Stdio::null());
    command
        .args(["build", "--root", ".", "--pause", "--"])
        .arg(MOCKCC);
    let output = command.output().expect("run turbo build --pause");
    assert_success(&output);
}

#[test]
fn unwritable_cache_dir_does_not_break_the_build() {
    // The cache is an optimisation: if it cannot be written, the build must
    // still succeed — just without caching.
    let sandbox = Sandbox::new("readonlycache");
    sandbox.write("src/a.cpp", "int main(){return 0;}\n");
    // A regular file used as a parent directory guarantees create_dir_all fails.
    sandbox.write("blocked", "not a directory");

    let mut command = sandbox.command();
    command
        .env("TURBO_CACHE_DIR", sandbox.dir.join("blocked").join("cache"))
        .args(["build", "--root", ".", "--", MOCKCC]);
    let output = command.output().expect("run turbo build");
    assert_success(&output);
    assert!(
        sandbox.executable().is_file(),
        "the executable must be produced even when the cache is unusable"
    );
    let stderr = stderr_of(&output);
    assert!(
        stderr.contains("cannot write to cache"),
        "a warning about the unusable cache is expected, got:\n{stderr}"
    );
}

#[test]
fn tool_errors_exit_with_code_two() {
    let sandbox = Sandbox::new("usage");

    // Unknown command.
    let mut command = sandbox.command();
    command.arg("definitely-not-a-command");
    let output = command.output().expect("run turbo");
    assert_eq!(output.status.code(), Some(2));

    // build without a compiler after `--`.
    let mut command = sandbox.command();
    command.args(["build", "--root", "."]);
    let output = command.output().expect("run turbo build");
    assert_eq!(output.status.code(), Some(2));

    // Unknown option.
    let mut command = sandbox.command();
    command.args(["build", "--nope", "--"]).arg(MOCKCC);
    let output = command.output().expect("run turbo build");
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn help_and_version_are_available() {
    let sandbox = Sandbox::new("help");
    for args in [vec!["--help"], vec!["--version"]] {
        let mut command = sandbox.command();
        command.args(&args);
        let output = command.output().expect("run turbo");
        assert_success(&output);
        assert!(!output.stdout.is_empty(), "{args:?} printed nothing");
    }
}

/// The `examples/cpp_demo` fixture is committed, so make sure it still looks
/// like a project the orchestrator can discover.
#[test]
fn example_project_is_discoverable() {
    let example = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/cpp_demo");
    if !example.is_dir() {
        return;
    }
    let mut found = Vec::new();
    for entry in fs::read_dir(example.join("src")).expect("examples/cpp_demo/src") {
        let entry = entry.expect("dir entry");
        let name = entry.file_name().to_string_lossy().to_string();
        if name.ends_with(".cpp") {
            found.push(name);
        }
    }
    found.sort();
    assert!(
        found.len() >= 3,
        "example project should have a few sources, found {found:?}"
    );
    assert!(
        example.join("include").is_dir(),
        "example project should ship an include directory"
    );
}
