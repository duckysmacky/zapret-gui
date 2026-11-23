# zapret-gui - Agent Guide

A Windows desktop app in Rust (a `core` library crate + a `ui` crate built with Slint) that makes Flowseal's
[zapret-discord-youtube](https://github.com/Flowseal/zapret-discord-youtube) usable by non-technical people.
Instead of running upstream's `.bat` scripts, the app downloads upstream releases, parses each strategy `.bat`
into structured data, and drives `winws.exe` directly. On top of that it adds automatic strategy selection,
upstream sync, a tray icon, and a friendly Russian-language UI with full parity to upstream's `service.bat` menu.
Built first for one specific non-technical user, then released publicly (MIT).
**zapret-gui is not an official zapret project.** The README and the app's About section say so (D21, D28).

## Repository layout

A Cargo workspace with two crates:

| Crate | Kind | Holds |
|---|---|---|
| `core/` | library | **all** logic: upstream sync, strategy parsing, runner (winws), prober, scanner, watchdog, diagnostics, config, logging setup, the core API (`CoreClient`), platform seams and their Windows + mock implementations |
| `ui/` | binary | the Slint app: `.slint` UI files compiled at build time, Rust glue mapping core state to Slint properties/models, tray, window lifecycle. See `ui/AGENTS.md` |

Module layout inside the crates is **not fixed by these docs**. The owner decides it as the project grows. Rules:
- `core` never depends on a UI framework. `ui` contains no business logic, only wiring and presentation.
- Windows-specific code lives behind the platform seams in `core` (`.context/ARCHITECTURE.md#platform-seams`).
- Don't add crates, a second process or IPC without asking (D3).

## Read before you work

| Task touches | Read |
|---|---|
| Anything (first time) | `.context/README.md`, `.context/ARCHITECTURE.md`, `.context/FEATURES.md` |
| The `ui` crate | `ui/AGENTS.md` |
| Downloading/parsing upstream, strategies, lists, game filter, ipset, fakes | `.context/UPSTREAM.md` |
| Probing, auto-strategy, tests, DPI checkers, watchdog | `.context/PROBING.md` |
| Diagnostics checks, service.bat parity | `.context/FEATURES.md#servicebat-parity` |
| Elevation, file locations, archive extraction, hosts file, user input | `.context/SECURITY.md` |
| Builds, Linux mock, VM testing, CI, installer | `.context/TESTING-AND-RELEASE.md` |
| Planning what to do next | `.context/ROADMAP.md`, `.context/DECISIONS.md` |

If a `.context/<work-item>/` folder exists for your task (TICKET.md, ATTEMPTS.md, FINDINGS.md, ...), read it first
and keep it updated as you work.

**The docs win.** If code and `.context/` disagree, point out the conflict and ask. Don't silently "fix" either one.
If you change a design (not just an implementation detail), update the relevant doc in the same change and append a
DECISIONS entry. Docs marked `Proposed default` were filled in by the planner, not chosen by the owner: follow them,
but raise it if one looks wrong. Things marked *Unverified* must be checked against the real crate/docs before you
rely on them.

**UI looks are intentionally unspecified.** The docs describe what each screen must *do*, not how it looks or how
it's laid out. Don't invent a visual design. Ask, or build the plainest functional version and say so.

## Non-negotiable invariants

Full reasoning in `.context/SECURITY.md`.

1. **Never execute upstream `.bat` files.** Parse them as text only. Upstream is a data source. (D1)
2. **Archive extraction must reject path traversal** (`..`, absolute paths, drive letters, symlinks) and extract only
   into a fresh staging directory. The app runs elevated, so a zip-slip is a system compromise.
3. **Binaries the elevated app executes must live in an admin-only-writable location**, never in `%APPDATA%` or
   another user-writable folder. Otherwise any unelevated process can swap `winws.exe` and get admin.
4. **User-entered list entries are data, never arguments.** Validate them (domain / IP / CIDR syntax, no newlines,
   no control characters) and only ever write them into list files.
5. **Quit means everything stops.** "Quit" from the tray kills `winws.exe`. Closing the window only hides to tray.
6. **The hosts file is never edited without a backup and explicit user confirmation.**
7. **All Windows-specific calls go through the platform seams**, so the app builds and runs on Linux with mocks.

## Commands

Proposed default, adjust to the real workspace:

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace

# run on Linux (mock platform is selected automatically off Windows)
cargo run -p ui            # adjust to the ui crate's package name
```

Windows release builds and the installer are produced by CI (see `.context/TESTING-AND-RELEASE.md`).
Before calling a task done: fmt, clippy (no warnings) and tests pass for the workspace, and anything Windows-only you
touched still compiles for `x86_64-pc-windows-msvc` (CI's Windows job, or the owner's Windows VM).

## Working conventions

- **Errors:** typed errors with `thiserror` in `core`. `anyhow` is acceptable only at the top of `ui`. No
  `unwrap()`/`expect()` in non-test `core` code unless the invariant is explained in a comment.
- **Async:** tokio. Blocking work (extraction, hashing, file walking) goes through `spawn_blocking`. Never block the
  UI thread.
- **Naming:** descriptive names, snake_case (Rust standard). No single-letter names except trivial loop counters.
- **Strings:** the UI is Russian-only. User-visible strings are plain Russian literals in `.slint` and Rust; no i18n
  layer (`@tr`, translation helpers) (D29).
- **Modules:** `mod_name.rs` + `mod_name/` folder for submodules. Never `mod_name/mod.rs`.
- **Logging:** `tracing` everywhere. Logs go to a rotating file (ARCHITECTURE.md#configuration).
- **Value types:** `Command`, `CoreEvent`, `AppState` and friends derive `serde::Serialize`/`Deserialize`, so the
  core API can later move behind IPC unchanged.
- **Dependencies:** stick to the stack in `.context/TESTING-AND-RELEASE.md#toolchain`. Ask before adding anything else.
- **Commits:** small and focused, one concern each.
- **Ask before:** changing on-disk formats (config, bundle storage), changing anything in SECURITY.md invariants,
  adding a process/service/IPC, renaming or adding crates, touching the hosts file logic, adding dependencies, or
  deciding UI visuals.

## Glossary

| Term | Meaning |
|---|---|
| upstream | `Flowseal/zapret-discord-youtube` on GitHub |
| winws | `winws.exe`, the DPI-bypass engine (from bol-van/zapret), uses the WinDivert driver |
| bundle | one extracted upstream release (binaries + lists + strategy `.bat`s). Several versions are stored side by side; the user switches between them |
| pinned version | an upstream version the user explicitly picked from the version dropdown. Update checks don't switch away from it |
| strategy | one upstream `.bat` (e.g. `general (ALT3).bat`) parsed into a winws argument list |
| active strategy | the strategy winws is currently running with |
| leftover winws | a `winws.exe` still running after the app crashed. Killed on the next enable (D22) |
| runtime lists | the list files actually passed to winws: bundle lists + user lists + ipset mode applied |
| game filter | upstream's port-range switch for game traffic: off / TCP+UDP / TCP / UDP |
| ipset mode | upstream's `none` / `loaded` / `any` switch for `ipset-all.txt` |
| fakes | fake-payload `.bin` files; upstream lets you choose which is active for Discord UDP and game UDP |
| core API | `CoreClient`: the only interface the ui crate uses (commands in, state/events out) |
| prober | the component that checks whether target sites work (HTTP/TLS, DPI-freeze, ping) |
| quick / full scan | auto-strategy modes: stop at first passing strategy / rank all strategies |
| watchdog | periodic light probe that detects a strategy that stopped working |
| platform seam | a trait wrapping Windows-only functionality, with a real (Windows) and mock (Linux) implementation |
| Uliana mode | secret alternative skin enabled by `ULIANA_MODE=1` |
