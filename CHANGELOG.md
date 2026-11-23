# Changelog

All notable changes to this project are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0] - 2026-11-23

First public release. Everything is implemented on `std` only — the project builds offline
with nothing but `rustc`.

### Added

- **Cache proxy** (`turbo fastcache`): SHA-256 cache key over compiler identity, ordered
  compile flags, the source file and every transitively included project header; hit copies the
  cached object, miss runs the real compiler and stores the result atomically. Failed
  compilations are never cached. Non-compile invocations (linking, `-E`, `-M`, compiler probes)
  are forwarded to the compiler untouched.
- **Unity Build orchestrator** (`turbo build`): recursive source discovery with configurable
  excludes, chunking into `.build/unity_N.cpp`, parallel compilation through the proxy
  (`std::thread::scope`, no external crates), final link, deterministic summary with cache
  hit/miss counters.
- **`turbo.json`** project config: compiler, flags, include dirs, defines, excludes, chunk size,
  jobs, output, linker, link flags. Hand-written RFC 8259 JSON reader (no serde). Precedence:
  CLI → config → defaults.
- **Parallel header hashing**: two-pass include discovery + SHA-256 across worker threads; small
  graphs stay sequential to avoid thread overhead.
- **Drag & drop**: `turbo <project-dir>` shorthand, `--pause` for double-click launchers, and a
  `drag-build.cmd` launcher shipped in the Windows release.
- **Prebuilt releases**: GitHub Actions workflow builds a static Windows binary
  (`+crt-static`, no VC++ runtime needed) and a Linux tarball on every `v*` tag.
- **Test suite**: 62 tests (45 unit + 17 integration CLI) with a bundled fake compiler (`mockcc`)
  that really expands quoted includes, so no C++ toolchain is required to test.
- **Docs**: Ukrainian README, English README, design notes, GitHub publishing cheat-sheet and a
  `bench.ps1` cold/hot benchmark script.

### Known limitations

- Not yet validated against a real MSVC/GCC toolchain by the author (no compiler available in the
  development environment); end-to-end paths are covered via `mockcc`.
- System headers (`<...>` not reachable through `-I`) are not hashed.
- No PCH support; the include scanner is intentionally naive (safe over-approximation).
- Two simultaneous misses on the same key may compile twice (correct, just redundant).

[0.1.0]: https://github.com/snakeofskr1pt-afk/C-Turbo-Builder/releases/tag/v0.1.0
