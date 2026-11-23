//! `turbo` — command line entry point.
//!
//! ```text
//! turbo fastcache -- <compiler> <normal compile flags...>   # drop-in cache proxy
//! turbo build [options] -- <compiler> <compile flags...>    # Unity Build orchestrator
//! turbo hash -- <compiler> <normal compile flags...>        # print the cache key
//! turbo cache dir|stats|clear [--cache-dir DIR]
//! ```

use std::path::{Path, PathBuf};
use std::process::exit;
use turbo::compiler::{parse_invocation, CompilerSpec};
use turbo::orchestrator::{self, BuildConfig, BuildResult, DEFAULT_CHUNK_SIZE};
use turbo::proxy::{self, Outcome, ProxyOptions};
use turbo::util;
use turbo::TurboError;

const HELP: &str = "\
turbo — cpp-turbo-builder: compile cache + Unity Build orchestrator

USAGE
  turbo fastcache [--cache-dir DIR] [-v] -- <compiler> <compile flags...>
      Cache proxy. Compute the key for one translation unit; on a hit copy the
      cached object file, on a miss run the real compiler and remember it.
      Example: turbo fastcache -- cl /c src/a.cpp /Fo:build/a.obj
               turbo fastcache -- g++ -Iinclude -c src/a.cpp -o build/a.o

  turbo build [--root DIR] [--jobs N] [--chunk-size N] [--out FILE]
              [--linker PATH] [--cache-dir DIR] [--config FILE] [--no-config]
              [--quiet] [-v] [--pause]
              -- <compiler> <compile flags...>
      Find all sources, glue them into unity chunks, compile in parallel through
      the cache, then link the executable. Without `--`, the compiler and flags
      are taken from turbo.json.
      Example: turbo build -- g++ -Iinclude -O2
               turbo build --root . --out app.exe -- cl /O2 /Iinclude
               turbo build --root examples/cpp_demo          (turbo.json driven)

  turbo <project-dir> [build options...] [-- <compiler> ...]
      Shorthand for `turbo build --root <project-dir>`. You can drop a project
      folder onto turbo.exe in Explorer and it will build it.
      Example: turbo C:\\work\\mygame -- g++ -Iinclude -O2

  turbo hash [--manifest] -- <compiler> <compile flags...>
      Print the cache key and which files it covers (debugging tool).

  turbo cache dir|stats|clear [--cache-dir DIR]
      Inspect or wipe the object cache.

CONFIG (turbo.json)
  Place it in the project root (or pass --config FILE; --no-config ignores it).
  Keys: \"compiler\", \"flags\", \"include\", \"defines\", \"exclude\", \"chunkSize\",
  \"jobs\", \"out\", \"linker\", \"linkFlags\". Relative \"include\"/\"out\"/\"linker\"
  paths resolve against the config file's directory. Command-line values win.

ENVIRONMENT
  TURBO_CACHE_DIR   cache location (default: %LOCALAPPDATA%\\cpp-turbo-cache)
  TURBO_JOBS        default worker count for `build`
  TURBO_VERBOSE=1   verbose logging
  TURBO_EXTRA_KEY   extra salt mixed into every cache key

EXIT CODES
  0 success · 2 tool/usage error · other: the compiler's own exit code
";

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let code = match run(argv) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("turbo: {error}");
            2
        }
    };
    exit(code);
}

fn run(mut argv: Vec<String>) -> turbo::TurboResult<i32> {
    if argv.is_empty() {
        print!("{HELP}");
        return Ok(0);
    }

    // Drag & drop: `turbo.exe C:\path\to\project` behaves like
    // `turbo build --root C:\path\to\project`, so a folder can be dropped onto
    // the executable in Explorer.
    if Path::new(&argv[0]).is_dir() {
        let directory = argv.remove(0);
        argv.insert(0, "--root".to_string());
        argv.insert(1, directory);
        return cmd_build(argv);
    }

    let command = argv.remove(0);
    match command.as_str() {
        "-h" | "--help" | "help" => {
            print!("{HELP}");
            Ok(0)
        }
        "-V" | "--version" | "version" => {
            println!(
                "turbo {} (cpp-turbo-builder, zero dependencies)",
                env!("CARGO_PKG_VERSION")
            );
            Ok(0)
        }
        "fastcache" | "proxy" | "fc" => cmd_fastcache(argv),
        "build" | "b" => cmd_build(argv),
        "hash" => cmd_hash(argv),
        "cache" => cmd_cache(argv),
        other => Err(TurboError::msg(format!(
            "unknown command `{other}` (see `turbo --help`)"
        ))),
    }
}

fn split_at_double_dash(args: Vec<String>) -> (Vec<String>, Vec<String>) {
    match args.iter().position(|a| a == "--") {
        Some(position) => {
            let mut iter = args.into_iter();
            let head: Vec<String> = iter.by_ref().take(position).collect();
            iter.next(); // drop the "--" itself
            (head, iter.collect())
        }
        None => (args, Vec::new()),
    }
}

fn take_value(args: &[String], index: &mut usize, flag: &str) -> turbo::TurboResult<String> {
    match args.get(*index + 1) {
        Some(value) => {
            *index += 2;
            Ok(value.clone())
        }
        None => Err(TurboError::msg(format!("{flag} requires a value"))),
    }
}

fn parse_usize(value: &str, flag: &str) -> turbo::TurboResult<usize> {
    value
        .trim()
        .parse::<usize>()
        .map_err(|_| TurboError::msg(format!("{flag} expects a positive integer, got `{value}`")))
}

fn cmd_fastcache(argv: Vec<String>) -> turbo::TurboResult<i32> {
    let (options, command_line) = split_at_double_dash(argv);
    let mut cache_dir = util::cache_dir();
    let mut verbose = util::env_verbose();
    let mut index = 0usize;
    while index < options.len() {
        match options[index].as_str() {
            "--cache-dir" => {
                cache_dir = PathBuf::from(take_value(&options, &mut index, "--cache-dir")?)
            }
            "--verbose" | "-v" => {
                verbose = true;
                index += 1;
            }
            other => {
                return Err(TurboError::msg(format!(
                    "unknown option `{other}` for `fastcache`"
                )))
            }
        }
    }
    if command_line.is_empty() {
        return Err(TurboError::msg(
            "fastcache needs a compiler command after `--`, e.g. `turbo fastcache -- g++ -c a.cpp -o a.o`",
        ));
    }
    let program = PathBuf::from(&command_line[0]);
    let compiler_args = &command_line[1..];

    // Only "compile this translation unit to an object" invocations are cached.
    // Everything else (linking, preprocess-only, dependency-only, CMake compiler
    // probes) is handed to the real compiler untouched — that is what makes the
    // proxy safe to drop in front of an existing build system.
    if !is_compile_invocation(compiler_args) {
        if verbose {
            eprintln!(
                "[turbo] PASS (not a compile invocation): {} {}",
                program.display(),
                compiler_args.join(" ")
            );
        }
        return passthrough(&program, compiler_args);
    }

    let parsed = parse_invocation(compiler_args).map_err(TurboError::msg)?;
    let cwd = std::env::current_dir()?;
    let spec = CompilerSpec::new(program, parsed.user_flags, cwd);
    let proxy_options = ProxyOptions::new(cache_dir, verbose);
    let outcome = proxy::compile(&spec, &parsed.source, &parsed.object, &proxy_options)?;
    Ok(match outcome {
        Outcome::Hit | Outcome::Miss => 0,
        Outcome::Failed(code) => code,
    })
}

/// `/c` or `-c` present and no "produce something else" mode (`-E`, `-M`, `/P`…).
fn is_compile_invocation(args: &[String]) -> bool {
    let mut has_compile_flag = false;
    for arg in args {
        let lower = arg.to_ascii_lowercase();
        if lower == "/c" || lower == "-c" {
            has_compile_flag = true;
        }
        // Preprocess-only / dependency-only / analyze-only invocations never
        // produce an object file and must not be cached or rewritten.
        if matches!(
            lower.as_str(),
            "/p" | "/ep"
                | "/e"
                | "-e"
                | "-m"
                | "-mm"
                | "-mg"
                | "-md"
                | "-mmd"
                | "/analyze"
                | "-fsyntax-only"
                | "-cxx-abi-check"
        ) {
            return false;
        }
    }
    has_compile_flag
}

fn passthrough(program: &Path, args: &[String]) -> turbo::TurboResult<i32> {
    let status = std::process::Command::new(program)
        .args(args)
        .status()
        .map_err(|e| {
            TurboError::msg(format!("cannot run compiler `{}`: {e}", program.display()))
        })?;
    Ok(status.code().unwrap_or(1))
}

fn cmd_hash(argv: Vec<String>) -> turbo::TurboResult<i32> {
    let (options, command_line) = split_at_double_dash(argv);
    let mut show_manifest = false;
    for option in &options {
        match option.as_str() {
            "--manifest" => show_manifest = true,
            other => {
                return Err(TurboError::msg(format!(
                    "unknown option `{other}` for `hash`"
                )))
            }
        }
    }
    if command_line.is_empty() {
        return Err(TurboError::msg(
            "hash needs a compiler command after `--` (same syntax as `fastcache`)",
        ));
    }
    let program = PathBuf::from(&command_line[0]);
    let parsed = parse_invocation(&command_line[1..]).map_err(TurboError::msg)?;
    let cwd = std::env::current_dir()?;
    let spec = CompilerSpec::new(program, parsed.user_flags, cwd);
    let key = proxy::hash_only(&spec, &parsed.source)?;
    println!("cache key: {}", key.hex);
    println!("files hashed: {}", key.files);
    if !key.unresolved.is_empty() {
        println!(
            "unresolved includes ({}): {}",
            key.unresolved.len(),
            key.unresolved.join(", ")
        );
    }
    if key.truncated {
        println!("warning: include traversal was truncated by a limit");
    }
    if show_manifest {
        println!("--- manifest ---");
        print!("{}", key.manifest);
    }
    Ok(0)
}

fn cmd_build(argv: Vec<String>) -> turbo::TurboResult<i32> {
    let (options, command_line) = split_at_double_dash(argv);

    // CLI values stay optional: turbo.json fills the gaps, built-in defaults the rest.
    let mut root_arg: Option<PathBuf> = None;
    let mut jobs_arg: Option<usize> = None;
    let mut chunk_size_arg: Option<usize> = None;
    let mut output_arg: Option<PathBuf> = None;
    let mut linker_arg: Option<PathBuf> = None;
    let mut cache_dir = util::cache_dir();
    let mut config_arg: Option<PathBuf> = None;
    let mut no_config = false;
    let mut pause = false;
    let mut verbose = util::env_verbose();
    let mut quiet = false;

    let mut index = 0usize;
    while index < options.len() {
        match options[index].as_str() {
            "--root" => root_arg = Some(PathBuf::from(take_value(&options, &mut index, "--root")?)),
            "--jobs" | "-j" => {
                let raw = take_value(&options, &mut index, "--jobs")?;
                let value = parse_usize(&raw, "--jobs")?;
                if value == 0 {
                    return Err(TurboError::msg("--jobs must be at least 1"));
                }
                jobs_arg = Some(value);
            }
            "--chunk-size" => {
                let raw = take_value(&options, &mut index, "--chunk-size")?;
                let value = parse_usize(&raw, "--chunk-size")?;
                if value == 0 {
                    return Err(TurboError::msg("--chunk-size must be at least 1"));
                }
                chunk_size_arg = Some(value);
            }
            "--out" | "-o" => {
                output_arg = Some(PathBuf::from(take_value(&options, &mut index, "--out")?))
            }
            "--linker" => {
                linker_arg = Some(PathBuf::from(take_value(&options, &mut index, "--linker")?))
            }
            "--config" => {
                config_arg = Some(PathBuf::from(take_value(&options, &mut index, "--config")?))
            }
            "--no-config" => {
                no_config = true;
                index += 1;
            }
            "--cache-dir" => {
                cache_dir = PathBuf::from(take_value(&options, &mut index, "--cache-dir")?)
            }
            "--quiet" | "-q" => {
                quiet = true;
                index += 1;
            }
            "--verbose" | "-v" => {
                verbose = true;
                index += 1;
            }
            "--pause" => {
                pause = true;
                index += 1;
            }
            // Positional project directory: `turbo build C:\proj` and the
            // drag-and-drop form `turbo.exe C:\proj`.
            other if !other.starts_with('-') && root_arg.is_none() => {
                root_arg = Some(PathBuf::from(other));
                index += 1;
            }
            other => {
                return Err(TurboError::msg(format!(
                    "unknown option `{other}` for `build`"
                )))
            }
        }
    }

    // --- locate the project root and turbo.json ---------------------------
    let invocation_dir = std::env::current_dir()?;
    let root_hint = root_arg.clone().unwrap_or_else(|| invocation_dir.clone());
    let config_path: Option<PathBuf> = if no_config {
        None
    } else {
        Some(util::absolute(
            &config_arg
                .clone()
                .unwrap_or_else(|| root_hint.join("turbo.json")),
        ))
    };
    let config = match &config_path {
        Some(path) => {
            let loaded = turbo::config::Config::load(path).map_err(TurboError::msg)?;
            if verbose && loaded.is_some() {
                eprintln!("[turbo] config: {}", path.display());
            }
            loaded.unwrap_or_default()
        }
        None => turbo::config::Config::default(),
    };
    let config_dir = config_path
        .as_ref()
        .and_then(|path| path.parent().map(|parent| parent.to_path_buf()))
        .unwrap_or_else(|| root_hint.clone());

    let root = match &root_arg {
        Some(root) => util::absolute(root),
        // `--config elsewhere/turbo.json` without `--root`: the config's
        // directory is the project root.
        None => match &config_arg {
            Some(config_file) => util::absolute(
                config_file
                    .parent()
                    .filter(|parent| !parent.as_os_str().is_empty())
                    .unwrap_or_else(|| Path::new(".")),
            ),
            None => invocation_dir.clone(),
        },
    };

    // --- compiler and flags: CLI `-- ...` beats turbo.json ----------------
    let (program, cli_flags): (PathBuf, Vec<String>) = if !command_line.is_empty() {
        (PathBuf::from(&command_line[0]), command_line[1..].to_vec())
    } else if let Some(compiler) = &config.compiler {
        (PathBuf::from(compiler), Vec::new())
    } else {
        return Err(TurboError::msg(
            "build needs a compiler: pass `-- <compiler> [flags]` or set \"compiler\" in turbo.json",
        ));
    };

    let toolchain = turbo::compiler::detect(&program);
    let mut user_flags = config.compile_flags(toolchain, &config_dir);
    user_flags.extend(cli_flags);
    let spec = CompilerSpec::new(program, user_flags, invocation_dir);

    // --- effective settings: CLI > turbo.json > defaults ------------------
    let jobs = jobs_arg.or(config.jobs).unwrap_or_else(util::default_jobs);
    let chunk_size = chunk_size_arg
        .or(config.chunk_size)
        .unwrap_or(DEFAULT_CHUNK_SIZE);
    let output = output_arg
        .or_else(|| config.out.as_ref().map(|out| config_dir.join(out)))
        .unwrap_or_else(|| root.join(default_executable_name()));
    let linker =
        linker_arg.or_else(|| config.linker.as_ref().map(|linker| config_dir.join(linker)));

    let build_config = BuildConfig {
        root,
        jobs,
        chunk_size,
        output,
        linker,
        proxy: ProxyOptions::new(cache_dir, verbose),
        quiet,
        exclude_dirs: config.exclude.clone(),
        link_flags: config.link_flags.clone(),
    };

    let code = match orchestrator::build(&spec, &build_config)? {
        BuildResult::Success(summary) => {
            print_summary(&summary);
            eprintln!("[turbo] executable -> {}", summary.output.display());
            0
        }
        BuildResult::CompileFailed { summary, failure } => {
            print_summary(&summary);
            eprintln!(
                "[turbo] compile failed in unit {} ({}), exit code {}",
                failure.unit,
                failure.source.display(),
                failure.exit_code
            );
            if failure.exit_code == 0 {
                1
            } else {
                failure.exit_code
            }
        }
        BuildResult::LinkFailed { summary, exit_code } => {
            print_summary(&summary);
            eprintln!("[turbo] link failed, exit code {exit_code}");
            if exit_code == 0 {
                1
            } else {
                exit_code
            }
        }
    };

    if pause {
        eprintln!("[turbo] press Enter to close...");
        let mut line = String::new();
        let _ = std::io::stdin().read_line(&mut line);
    }
    Ok(code)
}

fn print_summary(summary: &orchestrator::BuildSummary) {
    let seconds = summary.elapsed.as_secs_f64();
    eprintln!(
        "[turbo] {} chunk(s): {} cache hit(s), {} miss(es) in {:.2} s",
        summary.chunks, summary.hits, summary.misses, seconds
    );
}

fn cmd_cache(argv: Vec<String>) -> turbo::TurboResult<i32> {
    let mut cache_dir = util::cache_dir();
    let mut subcommand: Option<String> = None;
    let mut index = 0usize;
    while index < argv.len() {
        match argv[index].as_str() {
            "--cache-dir" => {
                cache_dir = PathBuf::from(take_value(&argv, &mut index, "--cache-dir")?)
            }
            "-v" | "--verbose" => index += 1,
            other if subcommand.is_none() && !other.starts_with('-') => {
                subcommand = Some(other.to_string());
                index += 1;
            }
            other => {
                return Err(TurboError::msg(format!(
                    "unknown argument `{other}` for `cache`"
                )))
            }
        }
    }
    match subcommand.as_deref() {
        Some("dir") => {
            println!("{}", cache_dir.display());
            Ok(0)
        }
        Some("stats") => {
            let stats = proxy::stats(&cache_dir)?;
            println!("cache:   {}", stats.dir.display());
            println!("entries: {}", stats.entries);
            println!("size:    {}", human_bytes(stats.bytes));
            Ok(0)
        }
        Some("clear") => {
            let stats = proxy::clear(&cache_dir)?;
            println!(
                "removed {} entr{} ({})",
                stats.entries,
                if stats.entries == 1 { "y" } else { "ies" },
                human_bytes(stats.bytes)
            );
            Ok(0)
        }
        Some(other) => Err(TurboError::msg(format!(
            "unknown cache subcommand `{other}` (expected dir, stats or clear)"
        ))),
        None => Err(TurboError::msg(
            "cache needs a subcommand: dir, stats or clear",
        )),
    }
}

fn default_executable_name() -> &'static str {
    if cfg!(windows) {
        "app.exe"
    } else {
        "app"
    }
}

fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0usize;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}
