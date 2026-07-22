# tmonitor

Terminal-native uptime dashboard. Monitors hosts and services via ICMP ping and TCP port checks,
rendering live green/red status in an auto-columned TUI.

Runs on a Raspberry Pi (or any Linux/macOS machine) connected to an external display.
Single Rust binary — no database, no web server, no configuration beyond a TOML file.

## Quick Start

```bash
cargo run --release
```

This monitors `127.0.0.1` with the default config. Press Ctrl+C to exit.

To install the binary globally:

```bash
cargo install --path .
# Ensure ~/.cargo/bin is on your PATH, or use:
# ./target/release/tmonitor
```

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
| `--log-file <path>` | Write diagnostic logs to file (off by default) |
| `--log-level <level>` | Log level: `error`, `warn`, `info`, `debug` (default: `info`, requires `--log-file`) |
| `--help`, `-h` | Show usage information |

## Diagnostic Logging

When debugging issues (e.g., hosts always showing "Up" when they shouldn't), enable file logging:

```bash
tmonitor --log-file /tmp/tmonitor.log
```

This writes structured entries to the file without interfering with the TUI display:

```
[2026-07-22T10:30:00Z] [INFO] localhost transitioned: Up → Down
[2026-07-22T10:30:05Z] [INFO] Cycle complete: 3 hosts, 5 services, 2 down, 4.21s
[2026-07-22T10:31:00Z] [INFO] localhost transitioned: Down → Up
```

For more verbose output including per-probe timings and HTTP response details:

```bash
tmonitor --log-file /tmp/tmonitor.log --log-level debug
```

Logging is entirely opt-in — no log file is created and no disk writes occur when `--log-file` is not specified. This preserves the zero-disk-write guarantee for normal operation.

## Building from Source

### Prerequisites

- Rust stable (MSRV 1.71)
- Cargo

### Linux (x86_64 and ARM)

```bash
git clone https://github.com/jlundholm/tmonitor
cd tmonitor
cargo build --release
./target/release/tmonitor
```

### macOS

```bash
git clone https://github.com/jlundholm/tmonitor
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

For cross-compilation from an x86 machine, install the appropriate target and linker:

```bash
# Pi 3B+/4/5 (64-bit)
rustup target add aarch64-unknown-linux-gnu

# Pi 2/3 (32-bit)
rustup target add armv7-unknown-linux-gnueabihf

# Pi Zero/1 (ARMv6)
rustup target add arm-unknown-linux-gnueabihf

# Install linker and configure cargo
sudo apt install gcc-aarch64-linux-gnu
export CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc

# Build
cargo build --release --target aarch64-unknown-linux-gnu
```

## Troubleshooting

### Distro Rust is too old

```
error: lock file version 4 found but this version of cargo does not understand this lock file
```

The `apt install rustc cargo` packages are too old (Cargo 1.65). Install Rust via [rustup](https://rustup.rs) instead:

```bash
sudo apt remove rustc cargo
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source ~/.cargo/env
```

Then verify:
```bash
rustc --version   # should be 1.71+
cargo build --release
```

## ICMP Requirements (Linux)

ICMP ping requires permission to create raw datagram sockets.
On some Linux systems, configure the ping group range for your user's group
try it first, if all hosts go down you might need this change:

```bash
# Replace 1000 with your user's GID (run: id -g)
sudo sysctl -w net.ipv4.ping_group_range="1000 1000"
```

To make this persistent across reboots, create `/etc/sysctl.d/60-tmonitor.conf`:

```
net.ipv4.ping_group_range = 1000 1000
```

For a single-user system, you can also allow all groups:
```
sudo sysctl -w net.ipv4.ping_group_range="0 2147483647"
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

- **SIGINT / SIGTERM** — restores terminal, cancels pending checks, exits cleanly
- **Ctrl+C** — same as SIGINT
- **Terminal resize** — layout recomputes automatically

## License

MIT
