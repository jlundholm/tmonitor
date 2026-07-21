# tmonitor

Terminal-native uptime dashboard. Monitors hosts and services via ICMP ping and TCP port checks,
rendering live green/red status in an auto-columned TUI.

Runs on a Raspberry Pi (or any Linux/macOS machine) connected to an external display.
Single Rust binary — no database, no web server, no configuration beyond a TOML file.

## Quick Start

```bash
cargo install --path .
tmonitor
```

This monitors `127.0.0.1` with the default config. Press Ctrl+C to exit.

## Configuration

By default tmonitor looks for `tmonitor.toml` in the current directory.
Use `--config <path>` to specify a different file.

```toml
# tmonitor.toml
interval_secs = 60
concurrency = 10

[[hosts]]
name = "router"
address = "192.168.1.1"

[[hosts.services]]
name = "ssh"
port = 22
```

### Options

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `interval_secs` | integer | 60 | Seconds between check cycles |
| `concurrency` | integer | 10 | Max parallel probes per cycle |
| `[[hosts]]` | array | — | List of hosts to monitor |
| `[[hosts]].name` | string | — | Display label (truncated to 22 chars) |
| `[[hosts]].address` | string | — | Hostname or IP address |
| `[[hosts.services]]` | array | — | TCP port services on this host |
| `[[hosts.services]].name` | string | — | Service label |
| `[[hosts.services]].port` | integer | — | TCP port number |

### CLI Flags

| Flag | Description |
|------|-------------|
| `--config <path>` | Path to TOML config file |
| `--help`, `-h` | Show usage information |

## Building from Source

### Prerequisites

- Rust stable (MSRV 1.71+, latest recommended)
- Cargo

### Linux (x86_64 and ARM)

```bash
git clone <repo-url> tmonitor
cd tmonitor
cargo build --release
./target/release/tmonitor
```

### macOS

```bash
git clone <repo-url> tmonitor
cd tmonitor
cargo build --release
./target/release/tmonitor
```

### Raspberry Pi (cross-compile from x86)

The recommended approach is to compile on the Pi directly (ARM target):

```bash
# On the Pi
cargo build --release
```

For cross-compilation from an x86 machine, install the appropriate target:

```bash
rustup target add aarch64-unknown-linux-gnu   # Pi 3B+/4/5 (64-bit)
# or
rustup target add armv7-unknown-linux-gnueabihf  # Pi 2/3 (32-bit)

# Install linker
sudo apt install gcc-aarch64-linux-gnu

# Build
cargo build --release --target aarch64-unknown-linux-gnu
```

## ICMP Requirements (Linux)

ICMP ping requires permission to create raw datagram sockets.
On most Linux systems, configure the ping group range:

```bash
sudo sysctl -w net.ipv4.ping_group_range="0 2147483647"
```

To make this persistent across reboots, add to `/etc/sysctl.conf`
or create `/etc/sysctl.d/60-tmonitor.conf`:

```
net.ipv4.ping_group_range = 0 2147483647
```

## Resource Usage

On a Raspberry Pi 3 or later:
- Idle CPU: < 5%
- Memory: < 50 MB RSS

## How It Works

1. **Config** — reads `tmonitor.toml` (or default monitoring `127.0.0.1`)
2. **Engine** — runs health checks on configurable interval (default 60s)
   - ICMP ping via `surge-ping` (non-privileged DGRAM sockets)
   - TCP port checks via `tokio::net::TcpStream` with 5s timeout
3. **Display** — renders live status in auto-columned TUI at 250ms refresh
   - Green: host is up (with uptime counter)
   - Red: host is down (with downtime counter)
   - Top bar shows `tmonitor` and Pi system uptime

### Signal Handling

- **SIGINT / SIGTERM** — restores terminal, cancels in-flight checks, exits cleanly
- **Ctrl+C** — same as SIGINT
- **Terminal resize** — layout recomputes automatically

## License

MIT
