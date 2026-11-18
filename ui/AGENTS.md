# ui crate (Slint) - Agent Guide

The desktop shell: `.slint` UI files compiled at build time, Rust glue between Slint and the core API, the tray and the
window lifecycle. Read the root `AGENTS.md` first; this file only adds UI-specific rules.

## Stack

| Concern | Choice |
|---|---|
| UI | Slint 1.x, `.slint` files compiled via `slint-build` in `build.rs` |
| Widget style | `fluent` (Windows 11-like), set via `slint_build::CompilerConfiguration::with_style` |
| Tray | `tray-icon` crate (with its `muda` menus) |
| Single instance | named mutex / lock + local socket via `interprocess` to ask the running instance to show its window (Proposed default) |
| Notifications | a Windows toast crate (Proposed default: `notify-rust`. *Unverified: toast support for unpackaged apps / AppUserModelID requirements*) |

## Rules

### Separation
- `.slint` files are the view: properties, models and callbacks only. No business logic in Slint or in the glue.
- Expose state through Slint **globals** (e.g. an `AppState` global and a `Commands` global with callbacks), so the main
  window and the Uliana window bind to the same things.
- Strings: plain Russian literals, no i18n layer (D29).

### Threads
- The Slint event loop owns the main thread. Core runs on its tokio runtime on other threads.
- Never block the event loop. Callbacks spawn a task that calls `CoreClient::request`.
- Core events reach the UI through `slint::invoke_from_event_loop` / `Weak::upgrade_in_event_loop`. Map `AppState` to
  globals/models in one place.

### Window lifecycle and tray
- Close → hide to tray: `on_close_requested` returns `CloseRequestResponse::HideWindow`, and the app runs with
  `slint::run_event_loop_until_quit()` so it stays alive without visible windows. Quit: tell core to shut down (kills
  winws), then `slint::quit_event_loop()`. Verified in Slint 1.19 (`ui/src/main.rs`).
- Create the tray on the main thread after the Slint backend is initialized. Forward menu events into the event loop.
  *Unverified at runtime: tray-icon needs a Win32 message loop on its thread; it is created on the main thread where
  winit pumps messages (compiles for Windows; check in the VM).* On Linux the tray is stubbed (D30).
- Tray icon/color and status line come from `AppState`. Menu items call the same core commands as the window.

### Theme and Uliana skin
- Theme: system/light/dark. Switch light/dark at runtime through `Palette.color-scheme` (verified in Slint 1.19;
  `ColorScheme.unknown` = follow the system).
- Uliana: a separate root window component bound to the same globals, chosen at startup from the mode core reports
  (`ULIANA_MODE`, D24). The pettable cat uses Slint animations + `TouchArea` with original pixel-art images.
- No feature exists only in Uliana mode.

### Rendering and licensing
- Make sure the app renders in a VM without GPU acceleration. Fall back to the software renderer if needed
  (`SLINT_BACKEND` env var for testing).
- Slint is used under its royalty-free desktop license, which requires attribution (e.g. Slint's `AboutSlint` widget in
  About). *Unverified: re-read the current license terms before the first public release.*

## Testing
- Keep state → globals mapping in plain functions that can be unit-tested without a window.
- Optional: Slint's testing backend for a few interaction tests (Proposed default: only if needed).

## Commands
```sh
cargo run -p ui             # adjust to the ui crate's package name (Linux: mock platform)
cargo build -p ui --release
```
Live preview of `.slint` files: the Slint LSP (`slint-lsp`, works with nvim) or the VS Code extension.
