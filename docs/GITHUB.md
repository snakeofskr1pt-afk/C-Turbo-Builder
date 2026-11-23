# Готові тексти для GitHub

Файл-шпаргалка: що вставити в налаштування репозиторію, щоб сторінка виглядала професійно
й чесно. Нічого з цього не є кодом — просто тексти для копіювання.

---

## 1. Repository description («About»)

Варіант A — короткий (рекомендую, ~90 символів):

```
Content-addressed compile cache + Unity Build orchestrator for C++ — one zero-dependency binary.
```

Варіант B — трохи детальніший (~150 символів):

```
Speed up C++ builds: hash every translation unit, reuse cached objects, compile Unity Build chunks in parallel. Single binary, no deps.
```

Варіант C — технічний, для тих, хто шукає ccache-альтернативу:

```
ccache-style object cache (SHA-256 over source + all headers + flags) plus parallel Unity Build orchestration for g++/clang++/cl.exe.
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
