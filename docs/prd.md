---
title: tmonitor
status: final
created: 2026-07-18
updated: 2026-07-18
---

# PRD: tmonitor

## 1. Vision

tmonitor is a terminal-native uptime dashboard that runs on a Raspberry Pi connected to an external display. It continuously monitors hosts and services across a local network using ICMP ping and TCP port checks, rendering live green/red status in an auto-columned TUI that fills the available screen width. No database, no web server, no historical data — a single Rust binary that turns any terminal into a network operations display.

This is for IT generalists and homelab operators who want to spot a service failure before users file a ticket, without maintaining a full monitoring stack.

## 2. Target User

### 2.1 Jobs To Be Done

- Quickly assess whether everything on my network is up from across the room
- Know immediately when a service goes down so I can respond before users notice
- Add or remove a monitored host without touching any code
- Deploy the dashboard on cheap, low-power hardware and forget about it

### 2.2 Key User Journeys

- **UJ-1. Jared walks into his home office and glances at the Pi display.**
  - **Persona + context:** Jared, IT generalist, walking past the monitor on his desk.
  - **Entry state:** tmonitor has been running unattended for days on the Pi.
  - **Path:** Glances at the display → sees all green → continues walking.
  - **Climax:** The all-green display tells him everything is fine in under one second.
  - **Resolution:** No action needed. Monitor keeps running.
  - **Edge case:** A row is red with a non-zero downtime counter → Jared sits down to investigate.

- **UJ-2. Jared adds a new server to monitoring.**
  - **Persona + context:** Jared, setting up a new web server on the network.
  - **Entry state:** Server is provisioned and on the network.
  - **Path:** SSHes into the Pi → edits `tmonitor.toml` → adds hostname and optional port → saves file → restarts tmonitor.
  - **Climax:** The new server appears on the display after restart.
  - **Resolution:** Monitor back to showing live status with the new host included.

## 3. Glossary

- **Host** — A network-connected machine being monitored, identified by hostname or IP address.
- **Service** — A TCP port-based application running on a host (e.g., SSH on port 22, HTTP on port 80). A host may have zero or more services monitored.
- **Health Check** — A probe (ICMP echo or TCP connect) that determines whether a host or service is responsive.
- **Uptime** — Continuous duration a host or service has been passing health checks.
- **Downtime** — Continuous duration a host or service has been failing health checks.
- **Auto-column** — Layout algorithm that scans terminal width and arranges entries into the maximum number of balanced columns.

## 4. Features

### 4.1 Host and Service Monitoring

**Description:** Core monitoring loop that reads a TOML config file, runs health checks against configured hosts and services at a regular interval, and renders results in a live auto-columned TUI. Runs indefinitely until the process is terminated.

Realizes UJ-1, UJ-2.

**Functional Requirements:**

#### FR-1: ICMP Ping Health Check

The system can probe a host via ICMP echo request and classify it as up (echo reply received within timeout) or down.

**Consequences (testable):**
- An online host with ICMP enabled is shown as up within one check interval
- An unreachable host is shown as down within one check interval
- A host that does not respond to ICMP (firewall rules) is consistently shown as down

#### FR-2: TCP Port Health Check

The system can probe a TCP port on a host via TCP connect and classify the service as up (connection accepted) or down (connection refused or timeout).

**Consequences (testable):**
- An open port on a reachable host is shown as up
- A closed port or unreachable host is shown as down
- Config specifies port with optional protocol hint

#### FR-3: TOML Configuration

The system reads a TOML config file on startup that defines hosts and their monitored services.

**Consequences (testable):**
- A valid TOML file with one host and no services produces a ping-only entry
- A valid TOML file with hosts and port-based services produces entries with both ping and port status
- An invalid TOML file causes a clear error message on startup and the program exits
- If no config file exists, tmonitor falls back to a default config monitoring 127.0.0.1 (ping only)

#### FR-4: Auto-Columned TUI Display

The system renders each monitored host/service as a cell in a terminal layout that automatically computes the number of columns based on terminal width. Cells fill left-to-right, then top-to-bottom. Services appear as their own flat cells (not grouped under their parent host), with the hostname as a prefix (e.g. `web-server:ssh`).

A persistent top bar shows the program name `tmonitor` on the left and the Pi system uptime on the right (e.g. `up 12d 3h 42m`). No column headers are shown.

Cells are sorted in the order they appear in the config file. Down hosts are not reordered to the top — position is stable so the user learns where each host lives.

**Consequences (testable):**
- A narrow terminal (80 cols) shows fewer columns than a wide terminal (200 cols)
- Each cell shows: `hostname  Up  0d 23h 14m` (green) or `hostname  Down  0d 0h 12m` (red)
- Duration format always includes days: `Xd Yh Zm` (e.g. `0d 23h 14m`, `12d 3h 5m`)
- Display redraws on terminal resize
- Top bar is always visible at the top of the display

#### FR-5: Live Status with Counters

Each entry displays a green indicator with uptime counter when the host/service is up, and a red indicator with downtime counter when down.

**Consequences (testable):**
- A healthy host shows green text with an uptime counter that counts up from zero
- When a host fails a check, the display flips to red and a downtime counter starts counting up from zero
- When a failed host recovers, the display flips to green and an uptime counter starts counting up from zero
- Transitions are immediate with no animation, flash, or transition effect

#### FR-6: Continuous Monitoring Loop

The system runs health checks on all configured hosts and services on a configurable interval (default 60s), updating the display on each cycle. Checks are dispatched in parallel batches with a configurable concurrency cap (default 10) to avoid saturating the Pi's network stack.

**Consequences (testable):**
- All hosts are checked at least once per interval
- Display updates atomically after each check cycle completes
- At most N concurrent checks in flight at any time (default 10)
- A cycle completes within the check interval even when one host is slow

**Cross-Cutting NFRs:**

- **NFR-1 (Resource Usage):** Idle CPU usage below 5% on a Raspberry Pi 3 or later. Memory usage below 50 MB RSS.
- **NFR-2 (Cross-Platform):** Single binary compiles and runs on Linux x86_64, Linux ARM (Raspberry Pi), and macOS.
- **NFR-3 (No Persistence):** Zero writes to disk. No database, no log files.

**Notes:**
- Concurrency model should use async or threads so slow/unreachable hosts don't block the check cycle
- Signal handling (SIGINT/SIGTERM) should restore terminal to normal state on exit

## 5. Non-Goals

- tmonitor is not a full monitoring platform (no Nagios/Zabbix replacement)
- tmonitor does not send alerts or notifications
- tmonitor does not store or graph historical data
- tmonitor does not expose a web UI or API
- tmonitor does not support authentication or multi-user access

## 6. MVP Scope

### 6.1 In Scope

- ICMP ping health checks
- TCP port health checks
- TOML configuration file
- Default config with 127.0.0.1 when no config file exists
- Auto-columned terminal display with top bar (program name + Pi uptime)
- Green/red live status with uptime/downtime counters (always showing days)
- Cross-platform binary (Linux x86, Linux ARM, macOS)

### 6.2 Out of Scope for MVP

- HTTP/HTTPS probes (deferred)
- DNS resolution checks (deferred)
- Certificate expiry monitoring (deferred)
- Config reload without restart (config changes require a binary restart)
- Any form of notification (deferred to post-MVP)
- Network discovery / nmap integration (deferred)

## 7. Success Metrics

**Primary:**
- **SM-1:** I learn about a service outage from the tmonitor display before a user reports it. Validates FR-1, FR-2, FR-5, FR-6.

**Secondary:**
- **SM-2:** Monitor runs unattended on a Raspberry Pi for 30+ days without crash or noticeable performance degradation. Validates NFR-1.
- **SM-3:** Adding a new host requires editing exactly one TOML file and takes under 30 seconds. Validates FR-3.

## 8. Open Questions

1. Should the binary be named `tmonitor` or something else?
   - `[ASSUMPTION: tmonitor, matching the project name]`

## 9. Assumptions Index

- From §4.1 FR-6: 60s default check interval, configurable
- From §4.1 FR-6: 10 concurrent check cap, configurable
- From §8: Binary named `tmonitor`
