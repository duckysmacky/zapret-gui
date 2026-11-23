set shell := ["bash", "-euo", "pipefail", "-c"]
set windows-shell := ["powershell.exe", "-NoLogo", "-NoProfile", "-Command"]

win_target := "x86_64-pc-windows-msvc"
win_exe    := if os() == "windows" { "target/release/zapret-gui.exe" } else { "target" / win_target / "release/zapret-gui.exe" }
iscc       := env("ISCC", if os() == "windows" { "C:/Program Files/Inno Setup 7/ISCC.exe" } else { home_directory() / ".wine/drive_c/Program Files/Inno Setup 7/ISCC.exe" })
version    := replace_regex(read("Cargo.toml"), '(?s).*?\nversion\s*=\s*"([^"]+)".*', "$1")

default:
    @just --list

run:
    cargo run -p zapret-ui

check:
    cargo fmt --all
    cargo clippy --workspace --all-targets -- -D warnings
    cargo test --workspace

# Compile-check the Windows target
[unix]
check-win:
    cargo xwin clippy --workspace --all-targets --target {{win_target}} -- -D warnings

[windows]
check-win:
    cargo clippy --workspace --all-targets -- -D warnings

# Build the Windows release exe
[unix]
build-win:
    cargo xwin build --release -p zapret-ui --target {{win_target}}

[windows]
build-win:
    cargo build --release -p zapret-ui

# Build the exe, then the installer via Inno Setup
[unix]
installer ver=(version + "-dev"): build-win
    test -f "{{iscc}}" || { echo "ISCC not found: {{iscc}} (set ISCC=...)"; exit 1; }
    WINEDEBUG=-all wine "{{iscc}}" "/DAppVersion={{ver}}" '/DExePath=..\{{replace(win_exe, '/', '\')}}' installer/zapret-gui.iss
    @echo "-> installer/Output/zapret-gui-setup-{{ver}}.exe"

# ExePath is not passed on Windows: the .iss default (..\target\release\zapret-gui.exe) is the native build output.
[windows]
installer ver=(version + "-dev"): build-win
    if (-not (Test-Path '{{iscc}}')) { Write-Error 'ISCC not found: {{iscc}} (set $env:ISCC=...)'; exit 1 }
    & '{{iscc}}' '/DAppVersion={{ver}}' installer/zapret-gui.iss
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    Write-Host '-> installer/Output/zapret-gui-setup-{{ver}}.exe'

[unix]
clean-installer:
    rm -rf installer/Output

[windows]
clean-installer:
    Remove-Item -Recurse -Force -ErrorAction SilentlyContinue installer/Output
