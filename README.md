# NetLite

<div align="center">

**轻巧 · 简单 · 实用 — 跨平台网络工具箱**

[![Rust](https://img.shields.io/badge/Rust-2026-orange.svg)](https://www.rust-lang.org)
[![License: Apache-2.0](https://img.shields.io/badge/License-Apache--2.0-blue.svg)](https://opensource.org/licenses/Apache-2.0)

[English](README-en.md) | 中文

</div>

## 简介

NetLite 是一个基于 Rust 和 GPUI 框架构建的**跨平台网络工具箱**，支持 Windows、Linux 和 macOS。它集成了网络通信调试、IP 网络计算等常用功能，界面简洁、启动快速，是开发者和网络工程师的得力助手。

## 功能特性

- **TCP/UDP 双协议** — 完整支持 TCP 和 UDP 协议的客户端与服务端模式
- **IPv4 / IPv6 双栈** — 同时支持两种 IP 版本
- **多种 TCP 解码器** — 原始字节、行分隔、长度前缀、JSON 解码，灵活适配不同协议格式
- **聊天式消息展示** — 直观呈现收发报文交互过程
- **自动回复 / 周期发送** — 收到消息后按预设内容自动响应，支持定时重复发送
- **配置持久化** — 自动保存连接配置，重启后直接复用
- **IP 地址计算器** — 输入 CIDR 格式（如 `192.168.1.0/24`）或纯 IP（如 `192.168.1.1`），即时计算网络地址、子网掩码、广播地址、可用主机数及地址范围
- **IP 地址扫描器** — 输入 IP 网段（如 `192.168.1.0/24`）、地址段（如 `192.168.1.1-100`）或单个 IP（如 `192.168.1.1`），快速探测指定范围内主机的可达性

### 界面体验
- **暗黑 / 亮色模式** — 自动跟随系统主题
- **多标签页** — 同时管理多个连接，轻松切换
- **收藏标记** — 支持消息收藏和自定义备注
- **日志导出** — 消息支持导出为 TXT / JSON / CSV 格式

## 快速开始

### 系统要求

| 平台 | 要求 |
|------|------|
| Windows | Windows 10 或更高版本 |
| Linux | 需要 GTK3 库 (`libgtk-3-dev`) |
| macOS | macOS 10.15 或更高版本 |

### 安装

从 [GitHub Release](https://github.com/kanebyqian/NetLite/releases) 下载对应平台的最新版本：

#### Windows

1. 下载 `NetLite-windows-x86_64.zip`
2. 解压后双击运行 `NetLite.exe`

#### Linux

```bash
tar -xzf NetLite-linux-x86_64.tar.gz
chmod +x NetLite
./NetLite
```

首次运行可能需要安装 GTK3 依赖：

```bash
sudo apt-get install -y libgtk-3-dev libwebkit2gtk-4.1-dev libappindicator3-dev librsvg2-dev patchelf libxkbcommon-x11-dev
```

#### macOS

1. 下载 `NetLite-macos-universal.tar.gz`
2. 解压后拖入 Applications 文件夹
3. 右键点击 → "打开"（macOS 首次运行需要此步骤）

### 使用方法

1. **创建连接** — 点击左侧面板 `+` 按钮，选择连接类型（客户端/服务端）、协议（TCP/UDP）、填写地址和端口
2. **建立连接** — 客户端点击 `[连接]`，服务端点击 `[启动]`
3. **切换编码模式** — 在输入框上方选择文本模式或十六进制模式
4. **发送消息** — 在底部输入框输入内容，点击 `[发送]` 或按 Enter
5. **管理连接** — 使用标签页切换不同连接；点击标签页上的 `×` 关闭；右键点击连接可删除保存的配置

### 内置工具

点击工具首页的卡片即可进入对应工具：

- **IP 地址计算器**：输入 `IP/掩码` 或 `IP/子网名`，如 `192.168.1.0/24`、`10.0.0.0/255.0.0.0`
- **IP 地址扫描器**：输入网段、地址段或单个 IP，如 `192.168.1.0/24`、`192.168.1.1-100`、`192.168.1.1`

## 项目结构

```
NetLite/
├── src/
│   ├── main.rs              # 应用入口
│   ├── app.rs               # 应用状态与事件处理
│   ├── config/              # 配置管理
│   ├── network/             # 网络层（TCP/UDP 协议实现）
│   ├── tools/               # 内置工具（计算器、扫描器等）
│   ├── ui/                  # UI 组件
│   └── utils/               # 工具函数
├── assets/                  # 嵌入资源（字体、图标）
├── .github/workflows/       # CI/CD 构建流程
├── Cargo.toml
└── README.md
```

## 从源码编译

```bash
git clone https://github.com/kanebyqian/NetLite.git
cd NetLite
cargo build --release
```

编译产物位于 `target/release/` 目录。

## 技术栈

| 组件 | 说明 |
|------|------|
| [GPUI](https://github.com/zed-industries/zed) | GPU 加速 UI 框架 |
| [gpui-component](https://github.com/longbridge/gpui-component) | 现代化 UI 组件库 |
| [Tokio](https://tokio.rs/) | 异步运行时 |
| [ipnet](https://crates.io/crates/ipnet) | IPv4/IPv6 地址处理 |
| [Serde](https://crates.io/crates/serde) | 数据序列化 |
| [NetAssistant](https://github.com/SunJary/NetAssistant) | 网络调试工具 |

## 许可证

本项目采用 [Apache License 2.0](LICENSE) 许可证。

---

<div align="center">

**如果这个项目对你有帮助，欢迎 Star ⭐️**

Made with ❤️ in Rust

</div>
