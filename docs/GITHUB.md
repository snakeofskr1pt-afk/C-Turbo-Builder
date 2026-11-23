# Готові тексти для GitHub

Файл-шпаргалка: що вставити в налаштування репозиторію, щоб сторінка виглядала професійно
й чесно. Нічого з цього не є кодом — просто тексти для копіювання.

---

## 1. Repository description («About»)

> GitHub обмежує поле About **350 символами** (усі варіанти нижче влазять). Бери один і вставляй у
> Settings → General → Description (або шестірня біля About).

### Максимальна — 332 з 350 (найбільше інформації, що влазить)

```
Content-addressed compile cache + Unity Build orchestrator for C++: hashes source, every included header, flags and compiler identity; reuses cached object files; compiles Unity chunks in parallel; links the app. Drop-in launcher for CMake/make, or standalone build. One static binary, no dependencies, MIT, 62 tests, Windows/Linux.
```

### Рекомендований (187 символів)

```
Content-addressed compile cache + Unity Build orchestrator for C++: hashes sources, headers, flags and compiler; reuses cached objects; compiles chunks in parallel. One binary, zero deps.
```

### Короткий (107 символів)

```
C++ build accelerator: content-addressed object cache + parallel Unity Build in one zero-dependency binary.
```

### Технічний — для тих, хто шукає ccache-альтернативу (152 символи)

```
ccache-style object cache keyed on source + all headers + flags + compiler identity, plus parallel Unity Build orchestration for g++ / clang++ / cl.exe.
```

### Українською — для постів у Telegram/Reddit/DOU (не для GitHub)

```
turbo — кеш об'єктів + Unity Build для C++ в одному бінарнику без залежностей: хешує джерело разом з усіма заголовками й флагами, не компілює незмінене, паралельно збирає чанки. Працює як обгортка над g++/clang++/cl і як самостійний оркестратор.
```

### Анонс англійською (для release notes, Reddit r/cpp, Hacker News)

```markdown
**turbo** — a small C++ build accelerator in a single, dependency-free binary.

It hashes each translation unit together with every header it includes, so unchanged files are
never compiled twice; sources are also glued into Unity Build chunks that compile in parallel.
Use it as a drop-in launcher for an existing CMake/make build (`turbo fastcache -- g++ ...`) or let
it drive the build and link the executable (`turbo build -- g++ -Iinclude -O2`).

Measured on the proxy: ~22 ms per cache hit for a typical unit. Cold-build speed-up depends on the
project (chunks are fewer compiler invocations, but big TUs cost more than small ones).
62 tests, offline `cargo build`, MIT licensed. Windows and Linux; prebuilt binaries in Releases.

Honest v0.1.0 caveats: not yet validated against a real MSVC/GCC toolchain by the author, system
headers are not hashed, no PCH support yet.

Repo: https://github.com/snakeofskr1pt-afk/C-Turbo-Builder
```

### Social preview (картинка 1280×640, текст на ній)

```
turbo
C++ builds: cache what didn't change
```

> ⚠️ Не пиши «5× faster C++ builds» чи «blazing fast». Реальне прискорення залежить від проєкту,
> а холодна збірка на реальному C++ зазвичай 1.5–3×. Чесна цифра краще, ніж хайп, який
> перевірять у першому ж issue.

## 2. Topics

```
cpp build-tool compile-cache unity-build rust cli msvc gcc clang windows incremental-build ccache-alternative zero-dependencies
```

(GitHub: Settings → Topics або шестірня біля About.)

## 3. Опис релізу v0.1.0 (English, готовий до вставки)

```markdown
Prebuilt binaries for cpp-turbo-builder v0.1.0.

`turbo` is a content-addressed compile cache + Unity Build orchestrator for C++.
One static binary, no Rust required to run it.

## Download
- `turbo-v0.1.0-windows-x64.zip` — Windows 10/11 x64: `turbo.exe` + `drag-build.cmd` + docs.
- `turbo-v0.1.0-linux-x64.tar.gz` — Linux x64.

## Quick start
```powershell
turbo.exe build --root C:\work\mygame --jobs 16 --chunk-size 10 -- g++ -Iinclude -O2
```
Or drop a project folder onto `drag-build.cmd`.

You still need a C++ compiler (`g++`, `clang++` or `cl.exe`) — `turbo` caches and orchestrates,
it does not compile by itself.

## What's inside v0.1.0
- Content-addressed object cache: SHA-256 over source + all transitively included headers +
  flags + compiler identity.
- Unity Build: chunk sources, compile chunks in parallel, link once.
- Drop-in compiler wrapper (`turbo fastcache`) for CMake/make; linking and preprocessing are
  forwarded untouched.
- `turbo.json` project config; drag & drop project folder.
- 62 tests, no C++ toolchain required (bundled fake compiler).

## Known limitations
Not yet validated against a real MSVC/GCC toolchain by the author; system headers are not hashed;
no PCH support yet. See README for details.
```

## 4. README-и

- `README.md` — українською (основний).
- `README.en.md` — англійською, для міжнародної аудиторії.

GitHub показує `README.md`; на початку нього є посилання на англійську версію.

## 5. Чеклист перед першим пушем

1. `cargo fmt --all -- --check` і `cargo clippy --all-targets -- -D warnings` — чисті.
2. `cargo test --offline` — 62 зелені.
3. `powershell -ExecutionPolicy Bypass -File .\scripts\publish.ps1 -RepoUrl https://github.com/<акаунт>/cpp-turbo-builder.git -DryRun`
   — переконатись, що план правильний (мережа не потрібна).
4. Те саме без `-DryRun` — скрипт додасть remote, запушить `main` і тег `v0.1.0`.
   (Вручну: `git remote add origin ...` → `git push -u origin main` → `git tag v0.1.0` → `git push origin v0.1.0`.)
5. У Settings → General: Description (з п.1), Topics (п.2), Website — можна порожнім.
6. Settings → Actions → General: дозволити workflows (для CI та Release).
7. Перевірити, що GitHub визначив ліцензію MIT (має бути видно «MIT license» біля About).
8. Після пушу тега — дочекатись зеленого Release-workflow, перевірити, що в релізі є
   `turbo-v0.1.0-windows-x64.zip`.
9. За бажанням: Social preview (Settings → General → Social preview) — картинка 1280×640.
10. У `Cargo.toml` можна додати `repository = "https://github.com/<акаунт>/cpp-turbo-builder"`,
    коли буде відомий акаунт.

**Авторизація:** Git Credential Manager відкриє браузер; якщо пароль питають у терміналі — це
Personal Access Token зі scope `repo`, а не пароль від акаунта.

**Підпис комітів:** зараз автор — `cpp-turbo-builder <cpp-turbo-builder@users.noreply.github.com>`.
Щоб прив'язати до свого акаунта:
```powershell
git config user.name "Your Name"
git config user.email "you@example.com"
git rebase -r --root --exec "git commit --amend --no-edit --reset-author"
```

## 6. Що варто сказати в першому коментарі/README, щоб не було розчарувань

- Це **v0.1.0**: інструмент працює й покритий тестами, але на реальному великому C++-проєкті
  його ще ніхто не міряв — тому в README є розділ «Скільки це економить» з моделлю, а не
  вигаданими цифрами.
- Кеш допомагає на **повторних** збірках; перша збірка проєкту буде такою ж довгою, як і без
  `turbo` (мінус ефект Unity Build).
- Зміна спільного заголовка інвалідує всі чанки, що його включають — це очікувана поведінка.
- Issue/PR вітаються; найкорисніше — реальні заміри `scripts/bench.ps1` на ваших проєктах.
