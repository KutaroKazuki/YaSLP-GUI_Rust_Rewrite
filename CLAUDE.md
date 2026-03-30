# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

**YaSLP-GUI** (Yet another Switch LAN Play GUI) is a cross-platform tool for browsing and connecting to Nintendo Switch LAN Play relay servers. It ships as two binaries:
- `yaslp-gui` — native desktop GUI (egui/eframe)
- `yaslp-web` — headless web server with a browser-based UI (Axum + `index.html`)

## Workspace Layout

```
/                   yaslp-gui (root crate, desktop GUI)
shared/             yaslp-shared (models, parsing, settings I/O)
web/                yaslp-web (Axum web server)
assets/             Embedded font (Inter-Regular.ttf)
build.sh            Cross-platform release build script
.cargo/config.toml  Cross-compilation linker config
```

## Prerequisites (cross-compilation)

```bash
# Rust targets
rustup target add x86_64-unknown-linux-gnu armv7-unknown-linux-gnueabihf aarch64-unknown-linux-gnu x86_64-pc-windows-gnu

# System linkers (Debian/Ubuntu)
apt install gcc-arm-linux-gnueabihf gcc-aarch64-linux-gnu mingw-w64
```

## Build Commands

```bash
# Build all workspace members (debug)
cargo build

# Build release binaries
cargo build --release

# Build a specific package
cargo build --release --package yaslp-gui
cargo build --release --package yaslp-web

# Check all packages without linking
cargo check --workspace

# Cross-compile for all targets (Linux x86_64, ARM32, ARM64, Windows)
./build.sh
```

Cross-compilation targets defined in `.cargo/config.toml`:
- `x86_64-unknown-linux-gnu`
- `armv7-unknown-linux-gnueabihf`
- `aarch64-unknown-linux-gnu`
- `x86_64-pc-windows-gnu`

## Running

```bash
# Desktop GUI (requires display server)
./target/release/yaslp-gui

# Web server (default port 8080)
./target/release/yaslp-web
./target/release/yaslp-web 9000   # custom port
```

## Architecture

### Shared crate (`shared/`)
The foundation. All core types live here:
- `models.rs` — `AppSettings`, `ServerEntry`, `ServerStatus`
- `parse.rs` — parses 3 different server-list JSON formats; resolves platform-specific lan-play download URLs
- `settings.rs` — reads/writes `config.json` in the user's home directory

### GUI crate (root `src/`)
- `app.rs` — the entire GUI state machine and rendering (~60KB). Background work runs on a `rayon` thread pool and communicates back via `Arc<Mutex<>>`.
- `fetch.rs` — blocking `reqwest` HTTP calls for fetching server lists and checking server status concurrently
- `models.rs` — `ServerWrapper` adds display/selection logic on top of `ServerEntry`
- `settings.rs` — thin wrapper around `yaslp-shared` settings

Platform-specific process management lives in `app.rs`:
- **Linux**: spawns lan-play under `sudo`, tracks the PGID, kills the entire process group on disconnect; reads NICs from `/sys/class/net`
- **Windows**: launches lan-play as Administrator via `ShellExecuteExW` (`runas`), captures output through a named pipe, manages the process tree with a Job Object; reads NICs from the Windows registry (friendly name + `\Device\NPF_{GUID}`)

### Web crate (`web/`)
- `main.rs` — Axum app setup, shared `Arc<AsyncMutex<AppState>>`, all route handlers
- `fetch.rs` — async `reqwest` calls (server list refresh, status checks, binary download)
- `index.html` — self-contained single-file frontend (~1145 lines); served directly by the binary

Web API routes: `GET /`, `POST/GET /api/settings`, `POST /api/refresh`, `GET /api/servers`, `POST /api/connect`, `POST /api/disconnect`, `GET /api/state`, `POST /api/download`, `GET /api/info`, `GET /api/nics`, `GET /api/sudo-check`, `POST /api/detect`

The web server binds to `0.0.0.0` but logs the local machine IP for the user.

## Edition Note

The workspace uses **Rust edition 2024**. Keep this in mind when referencing language features or extern crate behavior.

## Versioning & Release

Version is set in the root `Cargo.toml` `[package]` block and in `web/Cargo.toml`. Both must be kept in sync. After bumping the version, run `./build.sh` to produce release artifacts for all 4 targets.
