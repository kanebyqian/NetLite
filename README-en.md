# NetLite

<div align="center">

**Lightweight · Simple · Practical — Cross-Platform network tool box**

[![Rust](https://img.shields.io/badge/Rust-2026-orange.svg)](https://www.rust-lang.org)
[![License: Apache-2.0](https://img.shields.io/badge/License-Apache--2.0-blue.svg)](https://opensource.org/licenses/Apache-2.0)

English | [中文](README.md)

</div>

## Introduction

NetLite is a **cross-platform network tool box** built with Rust and the GPUI framework, supporting Windows, Linux, and macOS. It integrates common tools for network communication debugging and IP network calculation with a clean interface and fast startup — designed for developers and network engineers.

## Features

- **TCP/UDP Dual Protocol** — Full support for TCP and UDP in both client and server modes
- **IPv4 / IPv6 Dual Stack** — Works with both IP versions
- **Multiple TCP Decoders** — Raw bytes, line-based, length-prefixed, and JSON decoding for flexible protocol adaptation
- **Chat-style Message Display** — Intuitive visualization of send/receive interactions
- **Auto-reply / Periodic Send** — Auto-respond with preset content on received messages; timed repeated sending for stress testing
- **Configuration Persistence** — Connection configs are auto-saved and restored on restart
- **IP Address Calculator** — Input CIDR notation (e.g., `192.168.1.0/24`) or a plain IP (e.g., `192.168.1.1`) to instantly compute network address, subnet mask, broadcast address, usable host count, and address range
- **IP Address Scanner** — Input a CIDR range (e.g., `192.168.1.0/24`), address range (e.g., `192.168.1.1-100`), or single IP (e.g., `192.168.1.1`) to quickly probe host reachability

### Interface
- **Dark / Light Theme** — Automatically follows system preference
- **Multi-tab Management** — Manage multiple connections simultaneously
- **Message Favorites** — Bookmark messages with custom remarks
- **Log Export** — Export messages as TXT, JSON, or CSV

## Quick Start

### System Requirements

| Platform | Requirements |
|----------|-------------|
| Windows | Windows 10 or later |
| Linux | GTK3 libraries (`libgtk-3-dev`) |
| macOS | macOS 10.15 or later |

### Installation

Download from [GitHub Releases](https://github.com/kanebyqian/NetLite/releases):

#### Windows

1. Download `NetLite-windows-x86_64.zip`
2. Extract and run `NetLite.exe`

#### Linux

```bash
tar -xzf NetLite-linux-x86_64.tar.gz
chmod +x NetLite
./NetLite
```

You may need GTK3 dependencies on first run:

```bash
sudo apt-get install -y libgtk-3-dev libwebkit2gtk-4.1-dev libappindicator3-dev librsvg2-dev patchelf libxkbcommon-x11-dev
```

#### macOS

1. Download `NetLite-macos-universal.tar.gz`
2. Extract and drag NetLite to Applications
3. Right-click → "Open" (required for first launch on macOS)

### Usage

1. **Create Connection** — Click `+` in the left panel, select type (client/server), protocol (TCP/UDP), address and port
2. **Connect** — Click `[Connect]` for clients, `[Start]` for servers
3. **Switch Encoding** — Choose text or hex mode above the input field
4. **Send Messages** — Type in the bottom input field, click `[Send]` or press Enter
5. **Manage Connections** — Switch tabs to change connections; click `×` on a tab to close; right-click a connection to delete its saved config

### Built-in Tools

Click a tool card on the home page:

- **IP Address Calculator**: Enter `IP/prefix` or `IP/subnet`, e.g., `192.168.1.0/24`, `10.0.0.0/255.0.0.0`
- **IP Address Scanner**: Enter a CIDR, range, or single IP, e.g., `192.168.1.0/24`, `192.168.1.1-100`, `192.168.1.1`

## Project Structure

```
NetLite/
├── src/
│   ├── main.rs              # Application entry point
│   ├── app.rs               # App state and event handling
│   ├── config/              # Configuration management
│   ├── network/             # Network layer (TCP/UDP implementations)
│   ├── tools/               # Built-in tools (calculator, scanner, etc.)
│   ├── ui/                  # UI components
│   └── utils/               # Utility functions
├── assets/                  # Embedded resources (fonts, icons)
├── .github/workflows/       # CI/CD build pipeline
├── Cargo.toml
└── README.md
```

## Build from Source

```bash
git clone https://github.com/kanebyqian/NetLite.git
cd NetLite
cargo build --release
```

The executable will be in `target/release/`.

## Technology Stack

| Component | Description |
|-----------|-------------|
| [GPUI](https://github.com/zed-industries/zed) | GPU-accelerated UI framework |
| [gpui-component](https://github.com/longbridge/gpui-component) | Modern UI component library |
| [Tokio](https://tokio.rs/) | Async runtime |
| [ipnet](https://crates.io/crates/ipnet) | IPv4/IPv6 address handling |
| [Serde](https://crates.io/crates/serde) | Data serialization |
| [NetAssistant](https://github.com/SunJary/NetAssistant) | network debugging tool |

## License

This project is licensed under the [Apache License 2.0](LICENSE).

---

<div align="center">

**If this project helps you, please give it a Star ⭐️**

Made with ❤️ in Rust

</div>
