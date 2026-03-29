# YaSLP-GUI

**Yet another Switch LAN Play GUI** — a cross-platform tool for browsing, connecting to, and monitoring Nintendo Switch LAN Play relay servers.

Ships as two binaries with identical features:

| Binary | Description |
|--------|-------------|
| `yaslp-gui` | Native desktop GUI (egui) |
| `yaslp-web` | Headless web server with browser-based UI |

---

## Features

- **Server browser** — fetches the public relay list and displays address, type, region, version, online/idle/active player counts, and ping
- **Live updates** — pings all servers every second and updates player counts and latency in real-time
- **Connect / Disconnect** — launches `lan-play` with the selected server and manages the process lifecycle
- **Quick Connect** — connect directly to any relay by address; detects the server type (rust/node/dotnet) first and shows an error if it is not a valid lan-play server
- **Side panel** — shows live server details and console output from `lan-play` while connected
- **Sudo / UAC support** — on Linux uses the sudo credential cache (prompts only when expired); on Windows triggers a UAC elevation dialog
- **Network interface selection** — optionally pass `--netif` to `lan-play` with a specific pcap device
- **Binary downloader** — downloads the correct `lan-play` binary for your platform directly from the app
- **Auto-refresh** — loads the server list automatically on startup
- **Settings persistence** — all configuration saved to `config.json` next to the executable

---

## Downloads

Pre-built binaries are available on the [Releases](../../releases) page.

| File | Platform |
|------|----------|
| `yaslp-gui-linux-x86_64` | Linux x86\_64 (desktop GUI) |
| `yaslp-gui-linux-armv7` | Linux ARMv7 (desktop GUI) |
| `yaslp-gui-linux-aarch64` | Linux AArch64 (desktop GUI) |
| `yaslp-gui-windows-x86_64.exe` | Windows x86\_64 (desktop GUI) |
| `yaslp-web-linux-x86_64` | Linux x86\_64 (web server) |
| `yaslp-web-linux-armv7` | Linux ARMv7 (web server) |
| `yaslp-web-linux-aarch64` | Linux AArch64 (web server) |
| `yaslp-web-windows-x86_64.exe` | Windows x86\_64 (web server) |

On Linux, make the binary executable after downloading:
```bash
chmod +x yaslp-gui-linux-x86_64
./yaslp-gui-linux-x86_64
```

---

## Usage

### Desktop GUI

```bash
./yaslp-gui
```

Requires a display server (X11 or Wayland on Linux, native on Windows).

### Web Server

```bash
./yaslp-web           # serves on port 8080
./yaslp-web 9000      # custom port
```

Open `http://<machine-ip>:8080` in any browser. The web server binds to `0.0.0.0` and logs the local IP on startup.

---

## Settings

Open the Settings dialog (⚙ button) to configure:

| Setting | Description |
|---------|-------------|
| **HTTP Timeout** | Request timeout in milliseconds for server status checks (default: 500 ms) |
| **Network Interface** | Enable `--netif` and select a pcap interface (checkbox + dropdown) |
| **Server List URL** | URL of the relay server list JSON |
| **Client Directory** | Path to the directory containing the `lan-play` binary |
| **Launch Mode** | `Default`, `ACNH` (adds `--pmtu 500`), or `Custom` (free-form parameters) |
| **Download Binary** | Downloads the correct `lan-play` binary for your platform into the client directory |

On **Linux**, the **Run as privileged** option controls whether `lan-play` is launched via `sudo`. The sudo credential cache is checked first — a password is only requested when the cache has expired.

On **Windows**, the **Run as Administrator** option triggers a UAC elevation dialog on connect.

---

## Quick Connect

Click **⚡ Quick Connect** and enter a relay address (e.g. `1.2.3.4:11451`).

The app checks whether the address is a valid lan-play server before connecting:
- Tries `GET /info` → rust/node backend
- Falls back to `GET /` → dotnet backend
- Shows an error if neither matches

Once connected, the side panel shows live details (ping, online/idle/active players, version) updated every second.

---

## Server Types

| Type | Detection endpoint | Player count fields |
|------|--------------------|---------------------|
| **rust** | `/info` | `online`, `idle` |
| **node** | `/info` | `online`, `idle` |
| **dotnet** | `/` | `clientCount` |

---

## Web API

`yaslp-web` exposes a REST API consumed by its embedded frontend. Third-party clients can use it too.

| Method | Route | Description |
|--------|-------|-------------|
| `GET` | `/` | Serves the web UI |
| `GET` | `/api/settings` | Read current settings |
| `POST` | `/api/settings` | Update settings |
| `POST` | `/api/refresh` | Start server list refresh (202 Accepted) |
| `GET` | `/api/servers` | Server list + refresh progress |
| `POST` | `/api/connect` | Connect `{"addr":"…","sudo_password":"…"}` |
| `POST` | `/api/disconnect` | Disconnect |
| `GET` | `/api/state` | Poll connection state, console output, QC server |
| `POST` | `/api/download` | Download lan-play binary (202 Accepted) |
| `GET` | `/api/info` | Platform, privileged mode, version |
| `GET` | `/api/nics` | List network interfaces |
| `GET` | `/api/sudo-check` | Check sudo cache validity (Linux) |
| `POST` | `/api/detect` | Detect server type `{"addr":"…"}` |

---

## Building from Source

### Prerequisites

- Rust toolchain (stable, edition 2024)
- For cross-compilation: `gcc-arm-linux-gnueabihf`, `gcc-aarch64-linux-gnu`, `mingw-w64`

```bash
# Add Rust cross-compilation targets
rustup target add x86_64-unknown-linux-gnu armv7-unknown-linux-gnueabihf aarch64-unknown-linux-gnu x86_64-pc-windows-gnu
```

### Build

```bash
# All packages, debug
cargo build

# Single package, release
cargo build --release --package yaslp-gui
cargo build --release --package yaslp-web

# All targets (Linux x64, ARM32, ARM64, Windows) — requires cross-compile toolchains
./build.sh
```

---

## Platform Notes

### Linux
- `lan-play` is spawned via `sudo` when privileged mode is enabled
- The sudo credential cache is reused when valid; password prompt only appears when expired
- Process group is tracked and killed cleanly on disconnect
- Network interfaces read from `/sys/class/net`

### Windows
- `lan-play` is launched as Administrator via `ShellExecuteExW` with the `runas` verb
- Output is captured through a named pipe
- The entire process tree (cmd.exe + lan-play) is managed via a Job Object and terminated together on disconnect
- Network interfaces read from the Windows registry (friendly name + `\Device\NPF_{GUID}`)
- WinPcap or Npcap must be installed for `lan-play` to function

---

## License

GPL-3.0 — see [LICENSE](LICENSE)
