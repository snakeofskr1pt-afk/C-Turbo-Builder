# cpp-turbo-builder

**A content-addressed compile cache and Unity Build orchestrator for C++ — one binary, zero dependencies.**

`turbo` sits between your build and the C++ compiler. It hashes each translation unit
(source + every included header + compiler flags + compiler identity), reuses the resulting
object file when nothing changed, and glues sources into parallel Unity Build chunks so shared
headers are parsed once per chunk instead of once per file.

```
                turbo build -- g++ -Iinclude -O2
                             │
           ┌─────────────────┴──────────────────┐
           │ 1. find every .cpp                 │
           │ 2. glue into .build/unity_N files  │  ← shared headers parsed once per chunk
           │ 3. N threads → cache proxy         │
           │       ├─ HIT  → copy cached .obj   │
           │       └─ MISS → run the compiler   │
           │ 4. link                           │
           └───────────────────────────────────┘
```

## Features

- **Content-addressed object cache.** SHA-256 over the full dependency set: source, every
  transitively included project header, compiler flags (in order), and the compiler binary
  identity (name + size + mtime). Cache lives in `%LOCALAPPDATA%\cpp-turbo-cache` on Windows.
- **Unity Build.** Sources are grouped into chunks (`--chunk-size`, default 8) that are compiled
  in parallel (`--jobs`, default = CPU count), then linked.
- **Drop-in compiler wrapper.** `turbo fastcache -- g++ -c a.cpp -o a.o` behaves like the compiler,
  so it can be used as `CMAKE_CXX_COMPILER_LAUNCHER` or a `CXX` wrapper in an existing build
  system. Linking, preprocessing (`-E`) and dependency generation (`-M`) are forwarded untouched.
- **Project config.** `turbo.json` holds compiler, flags, include dirs, defines, excludes, chunk
  size, jobs, output and linker. Command line always wins over the config.
- **Drag & drop.** `turbo <project-dir>` is shorthand for `turbo build --root <project-dir>`;
  drop a folder onto `turbo.exe` (or onto `drag-build.cmd`) and it builds.
- **Zero dependencies.** No crates, no serde, no rayon. Hand-written SHA-256, JSON reader,
  include parser and worker pool — the project builds offline with nothing but `rustc`.
- **Honest failure modes.** Failed compilations are never cached; an unwritable cache disables
  caching with a warning instead of failing the build; cache writes are atomic.

## Quick start

Prebuilt binaries (no Rust needed): download the archive from **Releases**, unpack, then:

```powershell
turbo.exe build --root C:\work\mygame --jobs 16 --chunk-size 10 -- g++ -Iinclude -O2
turbo.exe C:\work\mygame            # same thing, folder can be dropped onto the exe
turbo.exe cache stats
```

From source (requires Rust 1.70+, builds offline):

```powershell
cargo build --release
# binaries: target\release\turbo.exe
.\target\release\turbo.exe build --root examples\cpp_demo        # g++ + turbo.json
```

You still need a C++ compiler (`g++`, `clang++`, `cl.exe`) — `turbo` is a cache/orchestrator on
top of it, not a compiler itself.

## Commands

| Command | What it does |
|---|---|
| `turbo build [DIR] [--jobs N] [--chunk-size N] [--out FILE] [--linker PATH] [--config FILE] [--no-config] [--pause] -- <compiler> <flags>` | Discover sources, Unity Build, parallel compile through the cache, link |
| `turbo <project-dir> [...]` | Shorthand for `turbo build --root <project-dir>` (drag & drop) |
| `turbo fastcache [--cache-dir DIR] [-v] -- <compiler> <flags>` | Cache proxy for a single translation unit |
| `turbo hash [--manifest] -- <compiler> <flags>` | Print the cache key and the files it covers |
| `turbo cache dir\|stats\|clear` | Inspect or wipe the object cache |

Environment: `TURBO_CACHE_DIR`, `TURBO_JOBS`, `TURBO_VERBOSE=1`, `TURBO_EXTRA_KEY`.
Exit codes: `0` success, `2` tool/usage error, anything else is the compiler's own exit code.

## `turbo.json`

```json
{
  "compiler": "g++",
  "flags": ["-O2", "-std=c++20"],
  "include": ["include", "third_party/include"],
  "defines": { "NDEBUG": null, "ENGINE_VERSION": "42" },
  "exclude": ["third_party", "tests"],
  "chunkSize": 10,
  "jobs": 8,
  "out": "app.exe",
  "linker": "lld-link.exe",
  "linkFlags": ["-lm"]
}
```

Relative `include`/`out`/`linker` paths resolve against the config file's directory.
Precedence: **command line → `turbo.json` → built-in defaults**.

## How the cache key works

```
cpp-turbo-builder cache-key v1
compiler   cl.exe <size> <mtime>        ← compiler upgrade busts the cache
toolchain  msvc
flag       /O2
flag       /Iinclude
file       c:/proj/src/a.cpp   <sha256>
file       c:/proj/include/a.h <sha256> ← and recursively for every #include
```

A hit copies the cached object and never starts the compiler. A miss stores the fresh object
atomically. The manifest is kept next to the object as `<key>.meta`, so a hit/miss can be
explained months later (`turbo hash --manifest`). The include scanner is deliberately naive
(byte scan): a false *miss* is acceptable, a false *hit* never is.

## Measured cost / expected speed-up

The proxy overhead is small: **~22 ms per hit** for a typical unit (3 headers) and **~88 ms** for a
heavy one (200 headers / 1.7 MiB), measured on the development machine. Header hashing is
parallel (~2× on 4 cores; 200 headers 160 → 82 ms, 400 headers / 27 MiB 469 → 276 ms).

To model the end-to-end effect, `mockcc` (the bundled fake compiler used by the test suite) can
simulate compile/link time via `TURBO_MOCK_COMPILE_MS` / `TURBO_MOCK_LINK_MS`. On a 24-source
project with 1 s per unit and 4 jobs:

| Scenario | No Unity, cached | Unity + cache | Difference |
|---|---|---|---|
| Cold build | 6842 ms | **1384 ms** | 4.9× |
| Nothing changed | 835 ms | **330 ms** | 2.5× |
| One `.cpp` changed | 1706 ms | 1330 ms | ≈1.3× |
| Shared header changed | 7035 ms | **1383 ms** | 5.1× |

⚠️ These numbers model the *mechanism*, not real C++ compilation. Real-world cold-build gains are
typically **1.5–3×** (a big translation unit costs more than a small one), while a no-op rebuild
trades compilation for hashing + linking. On any real project, measure with
`scripts/bench.ps1`.

## Tests

```
cargo test --offline
```

62 tests: 45 unit (SHA-256 vectors, include parser, parallel hashing, JSON/turbo.json, cache keys,
chunking, paths) and 17 end-to-end CLI tests. They need **no C++ compiler**: the suite uses
`mockcc`, a bundled fake compiler/linker that really expands quoted includes, so header
invalidation and parallel compilation are verified honestly. A real `g++` job also runs in the
CI workflow.

## Limitations

- Not yet verified against a real MSVC/GCC toolchain in the development environment (no compiler
  was installed there); command-line construction is unit-tested and end-to-end paths use `mockcc`.
- System headers (`<...>` not found in `-I`) are not hashed — the compiler identity + flags pin
  them down instead.
- The include scanner does not understand macros that generate `#include`, nor PCH.
- Two parallel misses on the same key may compile twice (correct, just redundant).
- `turbo.json` is strict JSON (no comments).

## Roadmap

1. PCH support and `/showIncludes`-based exact dependencies.
2. Per-key lock file to deduplicate parallel misses.
3. Real-compiler CI matrix (MSVC on Windows, GCC/Clang on Linux) — workflow is already in the repo.
4. Optional BLAKE3 hashing backend behind a feature flag.
5. `turbo.json` profiles (`build`, `release`) and config inheritance.

## Why this shape?

- **One binary** (`turbo fastcache` + `turbo build` subcommands): the original plan had a Go
  orchestrator, but Go was unavailable and the machine had no network, so the orchestrator lives in
  the same Rust binary. The `fastcache` CLI contract is stable, so any external build system can
  still drive it.
- **No crates / SHA-256 instead of BLAKE3**: crates.io was unreachable, and the project must build
  offline. SHA-256 is a hand-written ~120 lines with official test vectors; hashing is orders of
  magnitude cheaper than compiling.
- **`mockcc` in the repo**: makes cache/invalidation/parallel logic testable on machines without a
  C++ toolchain, including CI.

## License

[MIT](LICENSE) — use it, fork it, ship it.
