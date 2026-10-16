//! FTP / TFTP Server：轻量级文件上传与下载服务
//!
//! 定位：一个轻量、临时、可配置监听地址与端口、支持双向文件传输的本地文件服务器。
//! 支持 FTP 与 TFTP 两种协议，后端使用 std::thread + std::net 自研（不加第三方 crate），
//! 与现有 syslog_server 的线程模型保持一致。
//!
//! 核心流程：选择目录 → 配置服务 → 启动 → 上传/下载 → 查看传输记录。
//! V1 不做成 NAS / 复杂管理平台；不做多账户、Anonymous、SFTP/FTPS、文件管理等。

use std::collections::VecDeque;
use std::fs;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use gpui::prelude::*;
use gpui::*;
use gpui_component::Theme;
use gpui_component::input::{Input, InputState};
use gpui_component::scroll::ScrollableElement;
use gpui_component::{ActiveTheme, StyledExt};
use gpui_component::tooltip::Tooltip;

use chrono::Local;

use crate::app::NetLiteApp;

/// 内存中保留的最大传输记录数（超过后丢弃最旧的）
const MAX_RECORDS: usize = 10000;
/// UI 轮询间隔（毫秒）
const POLL_INTERVAL_MS: u64 = 200;
/// 控制 / 数据连接读超时（毫秒）——用于响应停止请求
const CONNECT_TIMEOUT_MS: u64 = 300;
/// TFTP 数据块最大字节数（RFC 1350 建议 512）
const TFTP_BLOCK_SIZE: usize = 512;
/// TFTP 等待 ACK 的超时（毫秒）
const TFTP_ACK_TIMEOUT_MS: u64 = 1000;

/// 传输协议
#[derive(Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum FileProtocol {
    FTP,
    TFTP,
}

impl FileProtocol {
    pub fn label(self) -> &'static str {
        match self {
            FileProtocol::FTP => "FTP",
            FileProtocol::TFTP => "TFTP",
        }
    }
}

/// 服务运行状态
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum FileServerStatus {
    Stopped,
    Starting,
    Running,
    Stopping,
    Failed,
}

/// 操作类型
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TransferOperation {
    Upload,
    Download,
}

impl TransferOperation {
    pub fn label(self) -> &'static str {
        match self {
            TransferOperation::Upload => "上传",
            TransferOperation::Download => "下载",
        }
    }
}

/// 传输状态
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TransferStatus {
    Success,
    Failure,
    InProgress,
}

/// 单条传输记录
#[derive(Clone)]
pub struct TransferRecord {
    pub timestamp: String,
    pub protocol: FileProtocol,
    pub source_addr: String,
    pub operation: TransferOperation,
    pub filename: String,
    pub size: u64,
    pub duration_ms: u64,
    pub status: TransferStatus,
    pub error: Option<String>,
}

impl TransferRecord {
    fn format_size(&self) -> String {
        format_bytes(self.size)
    }
}

/// 共享的运行时统计（由服务端线程写入，轮询任务合并进 UI 状态）
#[derive(Default)]
struct SharedStats {
    active_connections: u32,
    upload_count: u64,
    download_count: u64,
    uploaded_bytes: u64,
    downloaded_bytes: u64,
}

/// FTP 服务端配置（启动时从 UI 读取）
struct FtpServerConfig {
    username: String,
    password: String,
    passive_start: u16,
    passive_end: u16,
    timeout_secs: u64,
    listen_ip: Ipv4Addr,
}

/// 服务端上下文（通过 Arc 共享给各线程）
struct ServerContext {
    root: Arc<PathBuf>,
    protocol: FileProtocol,
    ftp: FtpServerConfig,
    allow_upload: bool,
    allow_download: bool,
    stats: Arc<Mutex<SharedStats>>,
    records: Arc<Mutex<VecDeque<TransferRecord>>>,
    running: Arc<AtomicBool>,
}

/// 共享目录中的条目（用于 UI 展示）
#[derive(Clone)]
struct DirRow {
    name: String,
    size: u64,
    is_dir: bool,
    modified: String,
}

/// 单个 FTP/TFTP 服务器的运行时状态 + 配置 + 统计
///
/// 每个协议独立拥有一份：运行标志、运行状态、错误信息、上传/下载权限，以及由服务端线程写入、
/// 轮询任务合并进 UI 的共享统计与记录。两个服务器互不干扰，可独立启停或同时运行。
struct ServerInstance {
    /// 协议（启动后固定为 FTP 或 TFTP）
    protocol: FileProtocol,
    /// 运行标志（true=运行中；stop 时置 false，服务端线程与轮询任务据此退出）
    running: Arc<AtomicBool>,
    /// 运行状态
    status: FileServerStatus,
    /// 错误信息（如端口被占用、密码为空）
    error_message: Option<String>,
    /// 允许上传
    allow_upload: bool,
    /// 允许下载
    allow_download: bool,
    /// 共享统计（由服务端线程写入，轮询任务合并进 UI）
    stats: Arc<Mutex<SharedStats>>,
    /// 共享记录（由服务端线程写入，轮询任务拖进 UI）
    records: Arc<Mutex<VecDeque<TransferRecord>>>,
    /// 运行时统计（UI 展示）
    active_connections: u32,
    upload_count: u64,
    download_count: u64,
    uploaded_bytes: u64,
    downloaded_bytes: u64,
}

impl ServerInstance {
    fn new(protocol: FileProtocol) -> Self {
        Self {
            protocol,
            running: Arc::new(AtomicBool::new(false)),
            status: FileServerStatus::Stopped,
            error_message: None,
            allow_upload: true,
            allow_download: true,
            stats: Arc::new(Mutex::new(SharedStats::default())),
            records: Arc::new(Mutex::new(VecDeque::new())),
            active_connections: 0,
            upload_count: 0,
            download_count: 0,
            uploaded_bytes: 0,
            downloaded_bytes: 0,
        }
    }

    /// 是否运行中（Running / Starting）
    fn is_running(&self) -> bool {
        matches!(self.status, FileServerStatus::Running | FileServerStatus::Starting)
    }

    /// 合并后台线程写入的统计到 UI 展示字段
    fn merge_stats(&mut self) {
        if let Ok(stats) = self.stats.lock() {
            self.active_connections = stats.active_connections;
            self.upload_count = stats.upload_count;
            self.download_count = stats.download_count;
            self.uploaded_bytes = stats.uploaded_bytes;
            self.downloaded_bytes = stats.downloaded_bytes;
        }
    }

    /// 把后台线程写入的记录拖出（空则返回空 vec）
    fn drain_records(&mut self) -> Vec<TransferRecord> {
        if let Ok(mut shared) = self.records.lock() {
            if shared.is_empty() {
                Vec::new()
            } else {
                shared.drain(..).collect()
            }
        } else {
            Vec::new()
        }
    }
}

/// 按协议取出可变的服务器实例
fn server_mut(s: &mut FileServerState, protocol: FileProtocol) -> Option<&mut ServerInstance> {
    match protocol {
        FileProtocol::FTP => Some(&mut s.ftp_server),
        FileProtocol::TFTP => Some(&mut s.tftp_server),
    }
}

/// FTP / TFTP 服务器的状态
pub struct FileServerState {
    /// FTP 服务器运行时状态
    pub ftp_server: ServerInstance,
    /// TFTP 服务器运行时状态
    pub tftp_server: ServerInstance,
    /// 当前选中的服务器标签（FTP / TFTP），用于切换配置面板
    pub viewed_protocol: FileProtocol,
    /// 根目录（两个服务共用同一共享目录）
    pub root_directory: PathBuf,
    /// 传输记录（受 MAX_RECORDS 限制；含 FTP/TFTP，按 protocol 区分）
    pub records: Vec<TransferRecord>,
    /// 共享目录当前内容（运行时自动刷新）
    pub directory: Vec<DirRow>,
    /// 高级配置是否展开
    pub advanced_open: bool,
    /// 是否展示传输详情
    pub show_detail: bool,
    /// 当前选中的传输记录索引
    pub selected_record: Option<usize>,
}

impl FileServerState {
    pub fn new(_cx: &mut Context<Self>) -> Self {
        Self {
            ftp_server: ServerInstance::new(FileProtocol::FTP),
            tftp_server: ServerInstance::new(FileProtocol::TFTP),
            viewed_protocol: FileProtocol::FTP,
            root_directory: PathBuf::from("."),
            records: Vec::new(),
            directory: Vec::new(),
            advanced_open: false,
            show_detail: false,
            selected_record: None,
        }
    }

    /// 追加一条传输记录，并做上限裁剪
    fn append_record(&mut self, record: TransferRecord) {
        self.records.push(record);
        if self.records.len() > MAX_RECORDS {
            let excess = self.records.len() - MAX_RECORDS;
            self.records.drain(0..excess);
        }
    }

    /// 根据 root_directory 重新列出目录内容
    fn refresh_directory(&mut self) {
        self.directory = list_directory(&self.root_directory);
    }

    /// 置单个服务器为失败态并提示（内部辅助）
    fn set_failed(state: &Entity<FileServerState>, protocol: FileProtocol, message: &str, cx: &mut Context<NetLiteApp>) {
        state.update(cx, |s, _| {
            if let Some(server) = server_mut(s, protocol) {
                server.error_message = Some(message.to_string());
                server.status = FileServerStatus::Failed;
            }
        });
        cx.notify();
    }

    /// 启动服务：读取配置 → 绑定 → 启动服务端线程 → 启动轮询任务（按协议独立启停）
    pub fn start_service(
        protocol: FileProtocol,
        app: &NetLiteApp,
        state: &Entity<FileServerState>,
        cx: &mut Context<NetLiteApp>,
    ) {
        // 读取共享监听 IP
        let ip_str = app
            .file_server_ip_input
            .read_with(cx, |i, _| i.text().to_string())
            .trim()
            .to_string();
        let listen_ip: Ipv4Addr = match ip_str.parse() {
            Ok(ip) => ip,
            Err(_) => {
                Self::set_failed(state, protocol, "监听 IP 格式错误", cx);
                return;
            }
        };

        // 端口按当前协议从对应配置读取（FTP / TFTP 各自独立）
        let port_text = match protocol {
            FileProtocol::FTP => app.file_server_port_input.read_with(cx, |i, _| i.text().to_string()),
            FileProtocol::TFTP => app.file_server_tftp_port_input.read_with(cx, |i, _| i.text().to_string()),
        };
        let port: u16 = match port_text.trim().parse() {
            Ok(p) if (1..=65535).contains(&p) => p,
            _ => {
                Self::set_failed(state, protocol, "端口无效，请输入 1-65535", cx);
                return;
            }
        };

        // 协议专属配置：FTP 带用户名/密码/被动范围；TFTP 带超时
        let (username, password, passive_start, passive_end, timeout_secs) = match protocol {
            FileProtocol::FTP => (
                app.file_server_username_input.read_with(cx, |i, _| i.text().to_string()),
                app.file_server_password_input.read_with(cx, |i, _| i.text().to_string()),
                app
                    .file_server_passive_start_input
                    .read_with(cx, |i, _| i.text().to_string().trim().parse::<u16>().unwrap_or(50000)),
                app
                    .file_server_passive_end_input
                    .read_with(cx, |i, _| i.text().to_string().trim().parse::<u16>().unwrap_or(50100)),
                0,
            ),
            FileProtocol::TFTP => (
                String::new(),
                String::new(),
                50000,
                50100,
                app
                    .file_server_timeout_input
                    .read_with(cx, |i, _| i.text().to_string().trim().parse::<u64>().unwrap_or(30)),
            ),
        };

        // 读取共用根目录与各服务器的权限/统计/记录/运行标志
        let (root_directory, allow_upload, allow_download, stats, records, running) = state.read_with(cx, |s, _| {
            let server = match protocol {
                FileProtocol::FTP => &s.ftp_server,
                FileProtocol::TFTP => &s.tftp_server,
            };
            (
                s.root_directory.clone(),
                server.allow_upload,
                server.allow_download,
                server.stats.clone(),
                server.records.clone(),
                server.running.clone(),
            )
        });

        if passive_start > passive_end {
            Self::set_failed(state, protocol, "被动端口范围无效（起始 > 结束）", cx);
            return;
        }
        if !root_directory.is_dir() {
            Self::set_failed(state, protocol, "文件目录不存在或不是目录", cx);
            return;
        }
        // FTP：设置了用户名或密码时，二者都必须提供
        if protocol == FileProtocol::FTP && (!username.is_empty() || !password.is_empty()) && (username.is_empty() || password.is_empty()) {
            Self::set_failed(state, protocol, "FTP 用户名与密码需同时填写", cx);
            return;
        }

        // 绑定套接字（失败直接返回，无需另设错误通道）
        let bound: std::io::Result<BindSocket> = match protocol {
            FileProtocol::FTP => TcpListener::bind((listen_ip, port)).map(BindSocket::Tcp),
            FileProtocol::TFTP => UdpSocket::bind((listen_ip, port)).map(BindSocket::Udp),
        };
        let socket = match bound {
            Ok(s) => s,
            Err(e) => {
                Self::set_failed(state, protocol, &format!("绑定失败：{e}"), cx);
                return;
            }
        };

        // 组装服务端上下文并启动对应协议
        let ctx = ServerContext {
            root: Arc::new(root_directory.clone()),
            protocol,
            ftp: FtpServerConfig {
                username,
                password,
                passive_start,
                passive_end,
                timeout_secs,
                listen_ip,
            },
            allow_upload,
            allow_download,
            stats,
            records,
            running,
        };

        Self::start_one(cx, state, protocol, ctx, socket);
    }

    /// 启动单个服务端：置状态 → 起阻塞 I/O 线程 → 起轮询合并任务（仅更新本服务器）
    fn start_one(
        cx: &mut Context<NetLiteApp>,
        state: &Entity<FileServerState>,
        protocol: FileProtocol,
        ctx: ServerContext,
        socket: BindSocket,
    ) {
        // 置为启动中并标记运行中
        state.update(cx, |s, _| {
            if let Some(server) = server_mut(s, protocol) {
                server.status = FileServerStatus::Starting;
                server.error_message = None;
                server.running.store(true, Ordering::Relaxed);
            }
        });
        cx.notify();

        // 服务端线程（阻塞 I/O）
        let server_ctx = Arc::new(ctx);
        let protocol = server_ctx.protocol;
        let server_ctx_thread = server_ctx.clone();
        thread::spawn(move || match (&socket, protocol) {
            (BindSocket::Tcp(l), FileProtocol::FTP) => {
                run_ftp_server(server_ctx_thread, l.try_clone().unwrap())
            }
            (BindSocket::Udp(u), FileProtocol::TFTP) => {
                run_tftp_server(server_ctx_thread, u.try_clone().unwrap())
            }
            _ => {}
        });

        // 轮询任务：合并本服务器的统计与记录，运行标志置 false 时退出
        let p_state = state.clone();
        let p_running = server_ctx.running.clone();
        cx.spawn(async move |_, cx| {
            loop {
                smol::Timer::after(Duration::from_millis(POLL_INTERVAL_MS)).await;
                let stopped = !p_running.load(Ordering::Relaxed);
                p_state.update(cx, |s, _| {
                    if let Some(server) = server_mut(s, protocol) {
                        server.merge_stats();
                        for record in server.drain_records() {
                            s.append_record(record);
                        }
                    }
                });
                if stopped {
                    p_state.update(cx, |s, _| {
                        if let Some(server) = server_mut(s, protocol) {
                            server.status = FileServerStatus::Stopped;
                            server.error_message = None;
                        }
                    });
                    break;
                }
                cx.refresh();
            }
        })
        .detach();

        state.update(cx, |s, _| {
            if let Some(server) = server_mut(s, protocol) {
                server.status = FileServerStatus::Running;
            }
        });
        cx.notify();
    }

    /// 停止服务（对外入口，按协议独立停止）
    pub fn stop_service(protocol: FileProtocol, state: &Entity<FileServerState>, cx: &mut Context<NetLiteApp>) {
        Self::stop_one(protocol, state, cx);
    }

    /// 停止单个服务端：停止运行标志并置为停止态
    fn stop_one(protocol: FileProtocol, state: &Entity<FileServerState>, cx: &mut Context<NetLiteApp>) {
        state.update(cx, |s, _| {
            if let Some(server) = server_mut(s, protocol) {
                server.running.store(false, Ordering::Relaxed);
                server.status = FileServerStatus::Stopped;
            }
        });
        cx.notify();
    }
}

/// 绑定后的套接字（FTP 为 TCP，TFTP 为 UDP）
enum BindSocket {
    Tcp(TcpListener),
    Udp(UdpSocket),
}

/// 工具结构
pub struct FileServerTool<'a> {
    app: &'a NetLiteApp,
    state: Entity<FileServerState>,
}

// ---------------------------------------------------------------------------
// 自由函数：格式化 / 路径 / 目录辅助
// ---------------------------------------------------------------------------

/// 格式化字节数为人类可读字符串
fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    if bytes == 0 {
        return "0 B".to_string();
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} {}", bytes, UNITS[unit])
    } else {
        format!("{:.1} {}", value, UNITS[unit])
    }
}

/// 格式化毫秒数为人类可读时长
fn format_duration(ms: u64) -> String {
    if ms < 1000 {
        format!("{} ms", ms)
    } else {
        let secs = ms as f64 / 1000.0;
        format!("{:.2} s", secs)
    }
}

/// 安全地根目录拼接请求路径：拒绝绝对路径与 `../`、`..` 逃逸。
/// 返回位于 root 内的规范化路径；若不安全或无法定位则返回 None。
fn secure_join(root: &Path, requested: &str) -> Option<PathBuf> {
    let root_canon = root.canonicalize().ok()?;
    // 统一分隔符
    let normalized = requested.replace('\\', "/");
    // 拒绝绝对路径（如 C:\... 或 /etc/passwd）
    if Path::new(requested).is_absolute() {
        // 仅取文件名，避免路径逃逸
        let file_name = Path::new(requested).file_name()?;
        return Some(root_canon.join(file_name));
    }
    let mut combined = root_canon.clone();
    for part in normalized.split('/') {
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." {
            return None; // 拒绝目录穿越
        }
        combined = combined.join(part);
    }
    // 最终校验：若路径存在，规范化后必须位于 root 内；若不存在（如待上传文件），
    // 则校验其父目录位于 root 内。
    let inside = match combined.canonicalize() {
        Ok(can) => can.starts_with(&root_canon),
        Err(_) => match combined.parent() {
            Some(p) => p.canonicalize().map(|c| c.starts_with(&root_canon)).unwrap_or(false),
            None => false,
        },
    };
    if inside {
        Some(combined)
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// 工具：UI 渲染
// ---------------------------------------------------------------------------

impl<'a> FileServerTool<'a> {
    pub fn new(app: &'a NetLiteApp) -> Self {
        Self {
            app,
            state: app.file_server_state.clone(),
        }
    }

    pub fn render(self, _window: &mut Window, cx: &mut Context<NetLiteApp>) -> AnyElement {
        let state = self.state.clone();
        let app = self.app;
        let theme = cx.theme().clone();

        let show_detail = state.read(cx).show_detail;

        div()
            .flex()
            .flex_col()
            .flex_1()
            .bg(theme.background)
            .overflow_x_hidden()
            // 整个工具内容区（配置 + 状态 + 目录 + 记录）一起随页面垂直滚动
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .child(Self::render_config_bar(state.clone(), app, cx))
                            .child(Self::render_body(state.clone(), cx)),
                    ),
            )
            .when(show_detail, |div| {
                div.child(Self::render_detail_modal(state.clone(), cx))
            })
            .into_any_element()
    }

    /// 配置分组：标题 + 内容
    fn section(title: &str, content: impl IntoElement, theme: Theme) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_2()
            .w_full()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().text_sm().font_semibold().text_color(theme.foreground).child(title.to_string())),
            )
            .child(content)
    }

    /// 顶部：协议选项卡 + 当前服务配置卡片
    fn render_config_bar(
        state: Entity<FileServerState>,
        app: &NetLiteApp,
        cx: &mut Context<NetLiteApp>,
    ) -> impl IntoElement {
        let s = state.read(cx);
        let theme = cx.theme().clone();
        let viewed = s.viewed_protocol;
        let st = state.clone();
        drop(s);

        div()
            .flex()
            .flex_col()
            .gap_3()
            .w_full()
            .px_4()
            .py_4()
            .bg(theme.background)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .w_full()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .text_base()
                                    .font_semibold()
                                    .text_color(theme.foreground)
                                    .child("服务配置"),
                            ),
                    )
                    .child(Self::protocol_tab_bar(st.clone(), cx)),
            )
            .child(Self::server_card(viewed, st, app, cx))
    }

    /// 协议切换选项卡（FTP / TFTP）：可随时切换，不受运行状态限制
    fn protocol_tab_bar(state: Entity<FileServerState>, cx: &mut Context<NetLiteApp>) -> impl IntoElement {
        let s = state.read(cx);
        let theme = cx.theme().clone();
        let viewed = s.viewed_protocol;
        let st = state.clone();
        drop(s);

        div()
            .flex()
            .items_center()
            .gap_1()
            .rounded_md()
            .bg(theme.secondary)
            .p_1()
            .child(Self::tab_option(FileProtocol::FTP, viewed == FileProtocol::FTP, &theme, st.clone(), cx))
            .child(Self::tab_option(FileProtocol::TFTP, viewed == FileProtocol::TFTP, &theme, st.clone(), cx))
    }

    fn tab_option(protocol: FileProtocol, active: bool, theme: &Theme, state: Entity<FileServerState>, cx: &mut Context<NetLiteApp>) -> Div {
        let label = match protocol {
            FileProtocol::FTP => "FTP",
            FileProtocol::TFTP => "TFTP",
        };
        let st = state.clone();
        let protocol = protocol.clone();
        div()
            .cursor_pointer()
            .px_3()
            .py_1()
            .rounded_sm()
            .text_sm()
            .font_medium()
            .text_color(if active { theme.primary } else { theme.muted_foreground })
            .bg(if active { theme.background } else { theme.secondary })
            .child(label)
            .on_mouse_down(MouseButton::Left, cx.listener(move |_, _, _, cx| {
                st.update(cx, |s, _| s.viewed_protocol = protocol.clone());
                cx.notify();
            }))
    }

    /// 单个服务器的完整配置卡片（配置 + 状态统计 + 启动/停止 + 错误提示）
    fn server_card(
        protocol: FileProtocol,
        state: Entity<FileServerState>,
        app: &NetLiteApp,
        cx: &mut Context<NetLiteApp>,
    ) -> impl IntoElement {
        let s = state.read(cx);
        let theme = cx.theme().clone();
        let server = match protocol {
            FileProtocol::FTP => &s.ftp_server,
            FileProtocol::TFTP => &s.tftp_server,
        };
        let is_running = server.is_running();
        let advanced_open = s.advanced_open;
        let status = server.status;
        let error = server.error_message.clone();
        let st = state.clone();
        drop(s);

        div()
            .rounded_lg()
            .border_1()
            .border_color(theme.border)
            .bg(theme.secondary)
            .px_4()
            .py_4()
            .flex()
            .flex_col()
            .gap_4()
            .child(Self::section("网络设置", Self::config_row_ip_port(protocol.clone(), app, is_running, cx), theme.clone()))
            .child(Self::section("共享目录", Self::config_row_directory(st.clone(), is_running, cx), theme.clone()))
            .when(protocol == FileProtocol::FTP, |div| {
                div.child(Self::section("登录认证", Self::config_row_ftp_auth(st.clone(), app, is_running, cx), theme.clone()))
            })
            .when(protocol == FileProtocol::TFTP, |div| {
                div.child(Self::tftp_auth_warning(cx))
            })
            .child(Self::section("访问权限", Self::config_row_permissions(st.clone(), protocol.clone(), is_running, cx), theme.clone()))
            .when(protocol == FileProtocol::FTP, |div| {
                div.child(Self::section("高级配置", Self::config_row_advanced(st.clone(), app, advanced_open, is_running, cx), theme.clone()))
            })
            .child(Self::server_status_bar(protocol.clone(), st.clone(), cx))
            .child(Self::render_error_section(error, theme.clone(), status))
            .child(Self::bottom_control(st, protocol, is_running, cx))
    }

    /// 监听 IP + 端口行
    fn config_row_ip_port(
        protocol: FileProtocol,
        app: &NetLiteApp,
        is_running: bool,
        cx: &mut Context<NetLiteApp>,
    ) -> impl IntoElement {
        let theme = cx.theme().clone();
        // FTP 与 TFTP 使用各自的端口配置（相互独立）
        let port_input: &Entity<InputState> = match protocol {
            FileProtocol::FTP => &app.file_server_port_input,
            FileProtocol::TFTP => &app.file_server_tftp_port_input,
        };

        div()
            .flex()
            .items_center()
            .gap_4()
            .w_full()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .flex_1()
                    .child(div().text_sm().font_medium().text_color(theme.muted_foreground).child("监听 IP"))
                    .child(Input::new(&app.file_server_ip_input).h_8().appearance(false).bg(theme.background).border_1().border_color(theme.border).rounded_md().disabled(is_running).flex_1()),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .flex_1()
                    .child(div().text_sm().font_medium().text_color(theme.muted_foreground).child("端口"))
                    .child(Input::new(port_input).h_8().appearance(false).bg(theme.background).border_1().border_color(theme.border).rounded_md().disabled(is_running).flex_1()),
            )
            .into_any_element()
    }

    /// 根目录行（带「选择」按钮）
    fn config_row_directory(state: Entity<FileServerState>, is_running: bool, cx: &mut Context<NetLiteApp>) -> impl IntoElement {
        let s = state.read(cx);
        let theme = cx.theme().clone();
        let root = s.root_directory.clone();
        let running = is_running;
        let st = state.clone();
        drop(s);

        div()
            .flex()
            .items_center()
            .gap_2()
            .w_full()
            .child(
                div()
                    .text_sm()
                    .font_medium()
                    .text_color(theme.muted_foreground)
                    .child("根目录"),
            )
            .child(
                div()
                    .flex()
                    .flex_1()
                    .items_center()
                    .min_w_0()
                    .rounded_md()
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.background)
                    .px_3()
                    .py_1p5()
                    .overflow_hidden()
                    .truncate()
                    .child(
                        div()
                            .text_sm()
                            .text_color(if root.as_os_str().is_empty() {
                                theme.muted_foreground
                            } else {
                                theme.foreground
                            })
                            .child(if root.as_os_str().is_empty() {
                                "尚未选择目录".to_string()
                            } else {
                                root.to_string_lossy().to_string()
                            }),
                    ),
            )
            .child(
                div()
                    .cursor_pointer()
                    .px_3()
                    .py_1p5()
                    .rounded_md()
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.background)
                    .when(!running, |d| d.hover(|style| style.bg(theme.border)))
                    .text_sm()
                    .font_medium()
                    .text_color(theme.foreground)
                    .child("选择")
                    .on_mouse_down(MouseButton::Left, cx.listener(move |_, _, _, cx| {
                        if running {
                            return;
                        }
                        let st = st.clone();
                        cx.spawn(async move |_, cx| {
                            if let Some(handle) = rfd::AsyncFileDialog::new()
                                .set_title("选择 FTP/TFTP 根目录")
                                .pick_folder()
                                .await
                            {
                                let path = handle.path().to_path_buf();
                                st.update(cx, |s, _| {
                                    s.root_directory = path;
                                    s.refresh_directory();
                                });
                            }
                        })
                        .detach();
                    })),
            )
            .into_any_element()
    }

    /// FTP 用户名 / 密码行
    fn config_row_ftp_auth(
        state: Entity<FileServerState>,
        app: &NetLiteApp,
        is_running: bool,
        cx: &mut Context<NetLiteApp>,
    ) -> impl IntoElement {
        let theme = cx.theme().clone();
        let running = is_running;
        let username_input = app.file_server_username_input.clone();
        let password_input = app.file_server_password_input.clone();

        div()
            .flex()
            .items_center()
            .gap_4()
            .w_full()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .flex_1()
                    .child(div().text_sm().font_medium().text_color(theme.muted_foreground).child("用户名"))
                    .child(Input::new(&username_input).h_8().appearance(false).bg(theme.background).border_1().border_color(theme.border).rounded_md().disabled(running).flex_1()),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .flex_1()
                    .child(div().text_sm().font_medium().text_color(theme.muted_foreground).child("密码"))
                    .child(
                        Input::new(&password_input)
                            .flex_1()
                            .h_8()
                            .appearance(false)
                            .bg(theme.background)
                            .border_1()
                            .border_color(theme.border)
                            .rounded_md()
                            .disabled(running)
                            .mask_toggle(),
                    ),
            )
            .into_any_element()
    }

    /// 允许上传 / 允许下载开关行（每个服务器独立保存权限）
    fn config_row_permissions(
        state: Entity<FileServerState>,
        protocol: FileProtocol,
        is_running: bool,
        cx: &mut Context<NetLiteApp>,
    ) -> impl IntoElement {
        let s = state.read(cx);
        let server = match protocol {
            FileProtocol::FTP => &s.ftp_server,
            FileProtocol::TFTP => &s.tftp_server,
        };
        let allow_upload = server.allow_upload;
        let allow_download = server.allow_download;
        drop(s);

        div()
            .flex()
            .items_center()
            .gap_4()
            .w_full()
            .child(Self::permission_toggle(state.clone(), protocol, "允许上传", allow_upload, is_running, cx))
            .child(Self::permission_toggle(state.clone(), protocol, "允许下载", allow_download, is_running, cx))
            .into_any_element()
    }

    /// 允许/允许下载单个开关
    fn permission_toggle(
        state: Entity<FileServerState>,
        protocol: FileProtocol,
        label: &str,
        on: bool,
        is_running: bool,
        cx: &mut Context<NetLiteApp>,
    ) -> impl IntoElement {
        let theme = cx.theme().clone();
        let label = label.to_string();
        div()
            .cursor_pointer()
            .px_3()
            .py_1p5()
            .rounded_md()
            .border_1()
            .border_color(if on { theme.primary } else { theme.border })
            .bg(if on { theme.primary.opacity(0.12) } else { theme.background })
            .text_sm()
            .font_medium()
            .text_color(if on { theme.primary } else { theme.muted_foreground })
            .child(label.clone())
            .on_mouse_down(MouseButton::Left, cx.listener(move |_, _, _, cx| {
                if is_running {
                    return;
                }
                let st = state.clone();
                st.update(cx, |s, _| {
                    if let Some(server) = server_mut(s, protocol) {
                        match label.as_str() {
                            "允许上传" => server.allow_upload = !server.allow_upload,
                            "允许下载" => server.allow_download = !server.allow_download,
                            _ => {}
                        }
                    }
                });
                cx.notify();
            }))
    }

    /// 高级配置（仅 FTP）：主动/被动、被动端口范围、连接超时
    fn config_row_advanced(
        state: Entity<FileServerState>,
        app: &NetLiteApp,
        advanced_open: bool,
        is_running: bool,
        cx: &mut Context<NetLiteApp>,
    ) -> impl IntoElement {
        let theme = cx.theme().clone();
        let st = state.clone();
        let running = is_running;

        div()
            .flex()
            .flex_col()
            .gap_3()
            .w_full()
            .child(
                div()
                    .cursor_pointer()
                    .when(!running, |d| d.hover(|style| style.bg(theme.border)))
                    .border_1()
                    .border_color(theme.border)
                    .rounded_md()
                    .bg(theme.background)
                    .p_3()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .on_mouse_down(MouseButton::Left, cx.listener(move |_, _, _, cx| {
                        if running {
                            return;
                        }
                        st.update(cx, |s, _| {
                            s.advanced_open = !s.advanced_open;
                        });
                        cx.notify();
                    }))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(div().text_sm().font_semibold().text_color(theme.foreground).child("⚙ 高级配置")),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(theme.muted_foreground)
                                    .child(if advanced_open { "收起 ▾" } else { "展开 ›" }),
                            )
                    )
                    .when(!advanced_open, |d| {
                        d.child(
                            div()
                                .text_sm()
                                .text_color(theme.muted_foreground)
                                .child("被动模式、被动端口范围、连接超时"),
                        )
                    })
            )
            .when(advanced_open, |d| {
                d.child(
                    div()
                        .mt_1()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .px_2()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_3()
                                .child(div().text_sm().font_medium().text_color(theme.muted_foreground).child("被动端口范围"))
                                .child(Input::new(&app.file_server_passive_start_input).w_16().h_8().appearance(false).bg(theme.background).border_1().border_color(theme.border).rounded_md().disabled(running))
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(theme.muted_foreground)
                                        .child("—"),
                                )
                                .child(Input::new(&app.file_server_passive_end_input).w_16().h_8().appearance(false).bg(theme.background).border_1().border_color(theme.border).rounded_md().disabled(running)),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_3()
                                .child(div().text_sm().font_medium().text_color(theme.muted_foreground).child("连接超时(秒)"))
                                .child(Input::new(&app.file_server_timeout_input).w_16().h_8().appearance(false).bg(theme.background).border_1().border_color(theme.border).rounded_md().disabled(running)),
                        ),
                )
            })
        .into_any_element()
    }

    /// 启动 / 停止按钮
    fn start_button(
        state: Entity<FileServerState>,
        protocol: FileProtocol,
        is_running: bool,
        cx: &mut Context<NetLiteApp>,
    ) -> impl IntoElement {
        let theme = cx.theme().clone();
        let running = is_running;
        let st = state.clone();
        let protocol = protocol.clone();

        div()
            .cursor_pointer()
            .flex()
            .items_center()
            .justify_center()
            .gap_2()
            .px_3()
            .py_1p5()
            .rounded_md()
            .when(running, |d| {
                d.bg(theme.danger)
                    .hover(|style| style.bg(theme.danger.opacity(0.9)))
            })
            .when(!running, |d| {
                d.bg(theme.primary)
                    .hover(|style| style.bg(theme.primary.opacity(0.9)))
            })
            .text_sm()
            .font_semibold()
            .text_color(theme.background)
            .child(if running { "停止服务" } else { "启动服务" })
            .on_mouse_down(MouseButton::Left, cx.listener(move |app: &mut NetLiteApp, _, _, cx| {
                let st = st.clone();
                let protocol = protocol.clone();
                let running = running;
                if running {
                    FileServerState::stop_service(protocol, &st, cx);
                } else {
                    FileServerState::start_service(protocol, app, &st, cx);
                }
            }))
    }

    /// 底部：分隔线 + 状态文本 + 启动/停止按钮
    fn bottom_control(state: Entity<FileServerState>, protocol: FileProtocol, is_running: bool, cx: &mut Context<NetLiteApp>) -> impl IntoElement {
        let s = state.read(cx);
        let theme = cx.theme().clone();
        let server = match protocol {
            FileProtocol::FTP => &s.ftp_server,
            FileProtocol::TFTP => &s.tftp_server,
        };
        let status = server.status;
        let st = state.clone();
        drop(s);

        let (status_text, status_color) = match status {
            FileServerStatus::Stopped => ("服务尚未启动", theme.muted_foreground),
            FileServerStatus::Starting => ("启动中…", theme.warning),
            FileServerStatus::Running => ("运行中", theme.primary),
            FileServerStatus::Stopping => ("停止中…", theme.warning),
            FileServerStatus::Failed => ("错误", theme.danger_foreground),
        };

        div()
            .flex()
            .flex_col()
            .gap_3()
            .w_full()
            .child(
                div()
                    .h_px()
                    .w_full()
                    .bg(theme.border),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .w_full()
                    .child(div().text_sm().font_medium().text_color(status_color).child(status_text))
                    .child(Self::start_button(st, protocol, is_running, cx)),
            )
            .into_any_element()
    }

    /// TFTP 无身份认证警告
    fn tftp_auth_warning(cx: &mut Context<NetLiteApp>) -> impl IntoElement {
        let theme = cx.theme().clone();
        div()
            .rounded_md()
            .border_1()
            .border_color(theme.warning)
            .bg(theme.warning.opacity(0.1))
            .px_3()
            .py_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().text_sm().text_color(theme.warning).child("⚠"))
                    .child(div().text_sm().text_color(theme.warning).child("TFTP 不提供身份认证，请仅在可信网络中使用。")),
            )
            .into_any_element()
    }

    /// 单服务：状态栏 + 统计（连接数 / 上行 / 下行）
    fn server_status_bar(
        protocol: FileProtocol,
        state: Entity<FileServerState>,
        cx: &mut Context<NetLiteApp>,
    ) -> impl IntoElement {
        let s = state.read(cx);
        let theme = cx.theme().clone();
        let server = match protocol {
            FileProtocol::FTP => &s.ftp_server,
            FileProtocol::TFTP => &s.tftp_server,
        };
        let status = server.status;
        let connected = server.active_connections;
        let tx = server.uploaded_bytes;
        let rx = server.downloaded_bytes;
        drop(s);

        let (status_text, status_color) = match status {
            FileServerStatus::Stopped => ("已停止", theme.muted_foreground),
            FileServerStatus::Starting => ("启动中…", theme.warning),
            FileServerStatus::Running => ("运行中", theme.primary),
            FileServerStatus::Stopping => ("停止中…", theme.warning),
            FileServerStatus::Failed => ("错误", theme.danger_foreground),
        };

        div()
            .flex()
            .items_center()
            .justify_between()
            .w_full()
            .px_4()
            .py_2()
            .border_b_1()
            .border_color(theme.border)
            .bg(theme.secondary.opacity(0.5))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .size_2p5()
                            .rounded_full()
                            .bg(if status == FileServerStatus::Running { theme.primary } else { theme.muted_foreground }),
                    )
                    .child(div().text_sm().font_medium().text_color(theme.foreground).child(status_text)),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_4()
                    .child(Self::stat_item("连接", format!("{}", connected), &theme, cx))
                    .child(Self::stat_item("上行", format_bytes(tx), &theme, cx))
                    .child(Self::stat_item("下行", format_bytes(rx), &theme, cx)),
            )
            .into_any_element()
    }

    fn stat_item(label: &str, value: String, theme: &Theme, cx: &mut Context<NetLiteApp>) -> impl IntoElement {
        div()
            .flex()
            .items_baseline()
            .gap_1()
            .child(div().text_sm().text_color(theme.muted_foreground).child(label.to_string()))
            .child(div().text_sm().font_semibold().text_color(theme.foreground).child(value))
    }

    /// 主体：共享目录 + 传输记录（可滚动）
    fn render_body(state: Entity<FileServerState>, cx: &mut Context<NetLiteApp>) -> impl IntoElement {
        let s = state.read(cx);
        let theme = cx.theme().clone();
        let directory = s.directory.clone();
        let records = s.records.clone();
        drop(s);

        div()
            .flex()
            .flex_col()
            .gap_4()
            .p_4()
            .child(Self::render_directory_section(directory.clone(), theme.clone(), cx))
            .child(Self::render_records_section(records.clone(), theme.clone(), state.clone(), cx))
            .into_any_element()
    }

    /// 文件名过长时截断并省略，完整名称通过 Tooltip 展示（避免撑宽页面）
    fn display_name(name: &str, max_chars: usize) -> String {
        if name.chars().count() > max_chars {
            let truncated: String = name.chars().take(max_chars).collect();
            format!("{}…", truncated)
        } else {
            name.to_string()
        }
    }

    /// 共享目录区块（文件列表内部滚动，最大高度约 320px；无文件时仅约 100px）
    fn render_directory_section(directory: Vec<DirRow>, theme: Theme, cx: &mut Context<NetLiteApp>) -> impl IntoElement {
        div()
            .rounded_lg()
            .border_1()
            .border_color(theme.border)
            .bg(theme.secondary.opacity(0.5))
            .child(
                div()
                    .px_4()
                    .py_2p5()
                    .border_b_1()
                    .border_color(theme.border)
                    .bg(theme.background)
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(div().text_sm().font_medium().text_color(theme.foreground).child("共享目录"))
                    .child(div().text_xs().text_color(theme.muted_foreground).child(format!("{} 个项目", directory.len()))),
            )
            .child(
                div()
                    .h(px(360.0))
                    .overflow_y_scrollbar()
                    .overflow_x_hidden()
                    .w_full()
                    .child(
                        if directory.is_empty() {
                            div()
                                .px_4()
                                .py_6()
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(div().text_sm().text_color(theme.muted_foreground).child("尚未选择文件目录"))
                        } else {
                            div()
                                .flex()
                                .flex_col()
                                .children(directory.iter().enumerate().map(|(row_idx, row)| {
                                    let full_name = row.name.clone();
                                    div()
                                        .px_4()
                                        .py_2()
                                        .border_b_1()
                                        .border_color(theme.border)
                                        .flex()
                                        .items_center()
                                        .justify_between()
                                        .child(
                                            div()
                                                .flex()
                                                .items_center()
                                                .gap_2()
                                                .child(if row.is_dir { "📁" } else { "📄" })
                                                .child(
                                                    div()
                                                        .id(ElementId::Name(format!("fs-dir-row-{}", row_idx).into()))
                                                        .hover(|s| s)
                                                        .tooltip(move |window, cx| Tooltip::new(full_name.clone()).build(window, cx))
                                                        .child(Self::display_name(&row.name, 26)),
                                                ),
                                        )
                                        .child(div().text_xs().text_color(theme.muted_foreground).child(format_bytes(row.size)))
                                }))
                        },
                    )
            )
            .into_any_element()
    }

    /// 错误提示区块（服务异常或启动失败时显示）
    fn render_error_section(error: Option<String>, theme: Theme, status: FileServerStatus) -> impl IntoElement {
        match error {
            Some(msg) if !msg.trim().is_empty() => div()
                .rounded_lg()
                .border_1()
                .border_color(theme.danger.opacity(0.5))
                .bg(theme.danger.opacity(0.08))
                .px_4()
                .py_3()
                .flex()
                .items_start()
                .gap_2()
                .child(div().text_sm().child("⚠️"))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(div().text_sm().font_medium().text_color(theme.danger_foreground).child("服务错误"))
                        .child(div().text_xs().text_color(theme.muted_foreground).child(msg.trim().to_string())),
                ),
            _ if status == FileServerStatus::Starting => div()
                .rounded_lg()
                .border_1()
                .border_color(theme.warning.opacity(0.5))
                .bg(theme.warning.opacity(0.08))
                .px_4()
                .py_3()
                .flex()
                .items_center()
                .gap_2()
                .child(div().text_sm().child("⏳"))
                .child(div().text_sm().text_color(theme.muted_foreground).child("服务启动中，请稍候…")),
            _ => div(),
        }
    }

    /// 传输记录区块（列表内部滚动，最大高度约 360px；无记录时仅约 100px）
    fn render_records_section(records: Vec<TransferRecord>, theme: Theme, state: Entity<FileServerState>, cx: &mut Context<NetLiteApp>) -> impl IntoElement {
        div()
            .rounded_lg()
            .border_1()
            .border_color(theme.border)
            .bg(theme.secondary.opacity(0.5))
            .child(
                div()
                    .px_4()
                    .py_2p5()
                    .border_b_1()
                    .border_color(theme.border)
                    .bg(theme.background)
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(div().text_sm().font_medium().text_color(theme.foreground).child("传输记录"))
                    .child(div().text_xs().text_color(theme.muted_foreground).child(format!("{} 条记录", records.len()))),
            )
            .child(
                div()
                    .h(px(360.0))
                    .overflow_y_scrollbar()
                    .overflow_x_hidden()
                    .w_full()
                    .child(
                        if records.is_empty() {
                            div()
                                .px_4()
                                .py_6()
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(div().text_sm().text_color(theme.muted_foreground).child("暂无传输记录"))
                        } else {
                            div()
                                .flex()
                                .flex_col()
                                .children(records.iter().enumerate().rev().map(|(i, rec)| {
                                    Self::render_record_row(i, rec.clone(), theme.clone(), state.clone(), cx)
                                }))
                        },
                    )
            )
            .into_any_element()
    }

    /// 单条传输记录行（点击展开详情）
    fn render_record_row(index: usize, rec: TransferRecord, theme: Theme, state: Entity<FileServerState>, cx: &mut Context<NetLiteApp>) -> AnyElement {
        let st = state.clone();
        let filename_tooltip = rec.filename.clone();
        let status_color = match rec.status {
            TransferStatus::Success => theme.primary,
            TransferStatus::Failure => theme.danger_foreground,
            TransferStatus::InProgress => theme.warning,
        };
        let status_text = match rec.status {
            TransferStatus::Success => "成功",
            TransferStatus::Failure => "失败",
            TransferStatus::InProgress => "进行中",
        };

        div()
            .cursor_pointer()
            .px_4()
            .py_2()
            .border_b_1()
            .border_color(theme.border)
            .when(rec.status == TransferStatus::InProgress, |d| d.opacity(0.7))
            .hover(|style| style.bg(theme.border.opacity(0.5)))
            .on_mouse_down(MouseButton::Left, cx.listener(move |_, _, _, cx| {
                let st = st.clone();
                st.update(cx, |s, _| {
                    s.selected_record = Some(index);
                    s.show_detail = true;
                });
                cx.notify();
            }))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(
                                div()
                                    .px_2()
                                    .py_0p5()
                                    .rounded_sm()
                                    .border_1()
                                    .border_color(status_color.opacity(0.5))
                                    .text_xs()
                                    .font_medium()
                                    .text_color(status_color)
                                    .child(status_text),
                            )
                            .child(
                                div()
                                    .id(ElementId::Name(format!("fs-rec-row-{}", index).into()))
                                    .hover(|s| s)
                                    .tooltip(move |window, cx| Tooltip::new(filename_tooltip.clone()).build(window, cx))
                                    .child(Self::display_name(&rec.filename, 26)),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_4()
                            .child(div().text_xs().text_color(theme.muted_foreground).child(rec.operation.label().to_string()))
                            .child(div().text_xs().text_color(theme.muted_foreground).child(format_record_time(&rec.timestamp)))
                            .child(div().text_xs().text_color(theme.muted_foreground).child(format_bytes(rec.size))),
                    ),
            )
            .into_any_element()
    }

    /// 详情面板（覆盖层）
    fn render_detail_modal(state: Entity<FileServerState>, cx: &mut Context<NetLiteApp>) -> impl IntoElement {
        let s = state.read(cx);
        let theme = cx.theme().clone();
        let rec = s.selected_record.and_then(|i| s.records.get(i)).cloned();
        let st = state.clone();
        drop(s);

        match rec {
            None => div().into_any_element(),
            Some(rec) => {
                let status_color = match rec.status {
                    TransferStatus::Success => theme.primary,
                    TransferStatus::Failure => theme.danger_foreground,
                    TransferStatus::InProgress => theme.warning,
                };
                let status_text = match rec.status {
                    TransferStatus::Success => "成功",
                    TransferStatus::Failure => "失败",
                    TransferStatus::InProgress => "进行中",
                };
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(gpui::rgba(0x00000055))
                    .child(
                        div()
                            .absolute()
                            .inset_0()
                            .on_mouse_down(MouseButton::Left, cx.listener(move |_, _, _, cx| {
                                st.update(cx, |s, _| {
                                    s.show_detail = false;
                                    s.selected_record = None;
                                });
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .absolute()
                            .w_full()
                            .max_w_128()
                            .rounded_lg()
                            .border_1()
                            .border_color(theme.border)
                            .bg(theme.background)
                            .overflow_hidden()
                            .shadow_lg()
                            .on_mouse_down(MouseButton::Left, cx.listener(|_, _, _, cx| {
                                cx.stop_propagation();
                            }))
                            .child(
                                div()
                                    .px_4()
                                    .py_3()
                                    .border_b_1()
                                    .border_color(theme.border)
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .child(div().text_base().font_semibold().text_color(theme.foreground).child("传输详情"))
                                    .child(
                                        div()
                                            .px_2()
                                            .py_0p5()
                                            .rounded_sm()
                                            .border_1()
                                            .border_color(status_color.opacity(0.5))
                                            .text_xs()
                                            .font_medium()
                                            .text_color(status_color)
                                            .child(status_text),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_3()
                                    .px_4()
                                    .py_4()
                                    .child(Self::detail_kv("文件名", rec.filename.clone(), theme.clone()))
                                    .child(Self::detail_kv("方向", rec.operation.label().to_string(), theme.clone()))
                                    .child(Self::detail_kv("大小", format_bytes(rec.size), theme.clone()))
                                    .child(Self::detail_kv("时长", format_duration(rec.duration_ms), theme.clone()))
                                    .child(Self::detail_kv("时间", rec.timestamp.clone(), theme.clone()))
                                    .child(Self::detail_kv("状态", status_text.to_string(), theme.clone()))
                                    .when(!rec.source_addr.is_empty(), |div| {
                                        div.child(Self::detail_kv("来源地址", rec.source_addr.clone(), theme.clone()))
                                    })
                            )
                    ).into_any_element()
            }
        }
    }

    fn detail_kv(label: &str, value: String, theme: Theme) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(div().text_xs().text_color(theme.muted_foreground).child(label.to_string()))
            .child(div().text_sm().font_medium().text_color(theme.foreground).child(value))
    }
}

/// 列出根目录内容，返回排序后的 DirRow
fn list_directory(root: &Path) -> Vec<DirRow> {
    let mut rows = Vec::new();
    let Ok(rd) = fs::read_dir(root) else {
        return rows;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        let metadata = path.metadata();
        let (is_dir, size, modified) = match metadata {
            Ok(m) => (
                m.is_dir(),
                m.len(),
                m.modified()
                    .ok()
                    .map(|t| {
                        Local::now()
                            .format("%Y-%m-%d %H:%M")
                            .to_string()
                    })
                    .unwrap_or_default(),
            ),
            Err(_) => (false, 0, String::new()),
        };
        rows.push(DirRow {
            name: entry.file_name().to_string_lossy().to_string(),
            size,
            is_dir,
            modified,
        });
    }
    // 目录优先，其次按名称排序
    rows.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    rows
}

/// 格式化传输记录的时间（取 HH:MM:SS）
fn format_record_time(timestamp: &str) -> String {
    // timestamp 格式为 "YYYY-MM-DD HH:MM:SS"，仅展示时分秒
    if let Some(idx) = timestamp.find(' ') {
        timestamp[idx + 1..].to_string()
    } else {
        timestamp.to_string()
    }
}

// ---------------------------------------------------------------------------
// 服务端实现：FTP / TFTP（std::thread + std::net 自研）
// ---------------------------------------------------------------------------

/// 写入一条传输记录并更新统计
fn record_transfer(ctx: &Arc<ServerContext>, operation: TransferOperation, filename: &str, size: u64, status: TransferStatus, error: Option<String>) {
    let timestamp = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    if let Ok(mut r) = ctx.records.lock() {
        r.push_back(TransferRecord {
            timestamp,
            protocol: ctx.protocol,
            source_addr: String::new(),
            operation,
            filename: filename.to_string(),
            size,
            duration_ms: 0,
            status,
            error,
        });
        while r.len() > MAX_RECORDS {
            r.pop_front();
        }
    }
    if let Ok(mut s) = ctx.stats.lock() {
        match operation {
            TransferOperation::Upload => {
                s.upload_count += 1;
                s.uploaded_bytes += size;
            }
            TransferOperation::Download => {
                s.download_count += 1;
                s.downloaded_bytes += size;
            }
        }
    }
}

// ============================ FTP ============================

/// FTP 会话（每个控制连接一份）
struct FtpSession {
    cwd: PathBuf,
    submitted_user: String,
    authenticated: bool,
    passive_listener: Option<TcpListener>,
    data_addr: Option<SocketAddr>,
    /// 控制连接在本机绑定的 IP（客户端实际连接的那个本地地址）。
    /// 当 listen_ip=0.0.0.0 时，PASV 用此 IP 作为数据连接的 advertise 地址，
    /// 使远程客户端也能回到正确的本机地址建立数据连接。
    local_ip: Ipv4Addr,
}

/// FTP 服务端主循环：接受控制连接并分派线程处理
fn run_ftp_server(ctx: Arc<ServerContext>, listener: TcpListener) {
    // std::net::TcpListener 的 accept() 不支持读超时，且本环境的 std 未提供
    // try_accept/shutdown，这里用非阻塞 accept + 运行标志轮询来响应停止请求。
    let _ = listener.set_nonblocking(true);
    while ctx.running.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, addr)) => {
                if let Ok(mut s) = ctx.stats.lock() {
                    s.active_connections += 1;
                }
                let cctx = ctx.clone();
                thread::spawn(move || handle_ftp_connection(stream, addr, cctx));
            }
            Err(_) => {
                if !ctx.running.load(Ordering::Relaxed) {
                    break;
                }
                // would-block（停止中）或非致命错误：短暂休眠后再次轮询
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
    if let Ok(mut s) = ctx.stats.lock() {
        s.active_connections = 0;
    }
}

/// 处理单个 FTP 控制连接
fn handle_ftp_connection(mut stream: TcpStream, addr: SocketAddr, ctx: Arc<ServerContext>) {
    // 本环境的 std 未提供 TCP 读/写超时，改用非阻塞模式，由 read_line 处理 WouldBlock。
    let _ = stream.set_nonblocking(true);
    let local_ip = match stream.local_addr().map(|a| a.ip()) {
        Ok(std::net::IpAddr::V4(ip)) => ip,
        _ => Ipv4Addr::LOCALHOST,
    };
    let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
    let mut sess = FtpSession {
        cwd: ctx.root.to_path_buf(),
        submitted_user: String::new(),
        authenticated: false,
        passive_listener: None,
        data_addr: None,
        local_ip,
    };

    let _ = write_all(&mut stream, b"220 NetLite FTP server ready\r\n");
    while ctx.running.load(Ordering::Relaxed) {
        let line = match read_line(&mut reader) {
            Ok(l) => l,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => continue, // 超时：检查运行标志
            Err(_) => break,
        };
        if line.trim().is_empty() {
            continue;
        }
        match handle_ftp_command(&mut sess, &line, &ctx, &mut stream) {
            Some(resp) => {
                if write_all(&mut stream, resp.as_bytes()).is_err() {
                    break;
                }
            }
            None => {
                let _ = write_all(&mut stream, b"221 Goodbye\r\n");
                break;
            }
        }
    }

    if let Some(l) = sess.passive_listener.take() {
        drop(l);
    }
    if let Ok(mut s) = ctx.stats.lock() {
        s.active_connections = s.active_connections.saturating_sub(1);
    }
}

/// 处理一条 FTP 命令，返回响应文本（None 表示 QUIT）
fn handle_ftp_command(sess: &mut FtpSession, line: &str, ctx: &Arc<ServerContext>, stream: &mut TcpStream) -> Option<String> {
    let mut parts = line.splitn(2, ' ');
    let cmd = parts.next().unwrap_or("").to_uppercase();
    let arg = parts.next().unwrap_or("").trim();

    match cmd.as_str() {
        "QUIT" => None,
        "NOOP" => Some("200 OK\r\n".to_string()),
        "SYST" => Some("215 UNIX Type: L8\r\n".to_string()),
        // 注意：WinSCP 的 CString::operator[] 是 1 索引（Delphi 传统），
        // 多行应答的“结束行”检测要求行长度 > 3 且前 4 字符等于 "211 "。
        // 标准结束行 "211" 仅 3 字符，会导致 WinSCP 永远等不到结束行而超时（控制连接超时）。
        // 这里在结束行加一个尾随空格，使 "211 " 能被 WinSCP 正确识别为多行应答的结束。
        "FEAT" => Some("211-Features:\r\n EPRT\r\n EPSV\r\n8BITBIN\r\n211 \r\n".to_string()),
        "USER" => {
            sess.submitted_user = arg.to_string();
            // 未配置账号则允许匿名；否则要求密码
            if ctx.ftp.username.is_empty() && ctx.ftp.password.is_empty() {
                sess.authenticated = true;
                Some("230 Login successful\r\n".to_string())
            } else {
                Some("331 Please enter password\r\n".to_string())
            }
        }
        "PASS" => {
            let ok = !ctx.ftp.username.is_empty()
                && sess.submitted_user == ctx.ftp.username
                && arg == ctx.ftp.password;
            if ok {
                sess.authenticated = true;
            }
            Some(if ok {
                "230 Login successful\r\n".to_string()
            } else {
                "530 Login incorrect\r\n".to_string()
            })
        }
        "PWD" => Some(format!("257 \"{}\" is the current directory\r\n", sess.cwd.display())),
        "CDUP" => {
            if let Some(parent) = sess.cwd.parent() {
                sess.cwd = parent.to_path_buf();
                Some("250 Directory changed\r\n".to_string())
            } else {
                Some("550 Failed to change directory\r\n".to_string())
            }
        }
        "CWD" => {
            if arg.is_empty() {
                Some("501 Usage: CWD <path>\r\n".to_string())
            } else {
                let target = secure_join(&sess.cwd, arg);
                match target {
                    Some(p) if p.is_dir() => {
                        sess.cwd = p;
                        Some("250 Directory changed\r\n".to_string())
                    }
                    _ => Some("550 Failed to change directory\r\n".to_string()),
                }
            }
        }
        "TYPE" => Some("200 Switching to binary mode\r\n".to_string()),
        "MODE" => Some("200 OK\r\n".to_string()),
        "STRU" => Some("200 OK\r\n".to_string()),
        "OPTS" => {
            // 忽略 UTF8 等选项，统一返回成功（兼容 WinSCP 等客户端）
            Some("200 OK\r\n".to_string())
        }
        "PASV" => {
            let ip = advertise_ip_for(&sess, ctx);
            match open_passive_listener(ctx, ip) {
                Some(l) => {
                    let port = l
                        .local_addr()
                        .map(|a| a.port())
                        .unwrap_or(0);
                    let a = ip.octets();
                    sess.passive_listener = Some(l);
                    Some(format!(
                        "227 Entering Passive Mode ({},{},{},{},{},{})\r\n",
                        a[0], a[1], a[2], a[3], port >> 8, port & 0xff
                    ))
                }
                None => Some("421 Unable to establish passive mode\r\n".to_string()),
            }
        }
        "EPSV" => {
            // 扩展被动模式：229 Entering Extended Passive Mode (|||PORT|)
            let ip = advertise_ip_for(&sess, ctx);
            match open_passive_listener(ctx, ip) {
                Some(l) => {
                    let port = l
                        .local_addr()
                        .map(|a| a.port())
                        .unwrap_or(0);
                    sess.passive_listener = Some(l);
                    Some(format!("229 Entering Extended Passive Mode (|||{port}|)\r\n"))
                }
                None => Some("421 Unable to establish passive mode\r\n".to_string()),
            }
        }
        "PORT" => {
            let v: Vec<u8> = arg.split(',').filter_map(|x| x.trim().parse::<u8>().ok()).collect();
            if v.len() == 6 {
                let ip = Ipv4Addr::new(v[0], v[1], v[2], v[3]);
                let port = ((v[4] as u16) << 8) | v[5] as u16;
                sess.data_addr = Some(SocketAddr::from((ip, port)));
                Some("200 OK\r\n".to_string())
            } else {
                Some("501 Invalid PORT argument\r\n".to_string())
            }
        }
        "LIST" => transfer_list(sess, stream, arg, ctx, true),
        "NLST" => transfer_list(sess, stream, arg, ctx, false),
        "RETR" => transfer_file(sess, stream, arg, ctx, TransferOperation::Download),
        "STOR" => transfer_file(sess, stream, arg, ctx, TransferOperation::Upload),
        "SIZE" => {
            if let Some(p) = secure_join(&sess.cwd, arg) {
                if let Ok(m) = p.metadata() {
                    Some(format!("213 {}\r\n", m.len()))
                } else {
                    Some("550 File not found\r\n".to_string())
                }
            } else {
                Some("550 File not found\r\n".to_string())
            }
        }
        _ => Some("500 Command not recognized\r\n".to_string()),
    }
}

/// 打开一个被动模式监听器并绑定到 `advertise_ip`（advertise 与绑定同 IP，使远程客户端可回到该地址建立数据连接）
fn open_passive_listener(ctx: &Arc<ServerContext>, advertise_ip: Ipv4Addr) -> Option<TcpListener> {
    let mut p = ctx.ftp.passive_start;
    loop {
        if let Ok(l) = TcpListener::bind((advertise_ip, p)) {
            return Some(l);
        }
        if p == ctx.ftp.passive_end {
            return None;
        }
        p += 1;
    }
}

/// 计算 PASV 应 advertise 的 IP：显式 listen_ip 优先，否则用控制连接的本地 IP（远程客户端可回连）
fn advertise_ip_for(sess: &FtpSession, ctx: &Arc<ServerContext>) -> Ipv4Addr {
    if ctx.ftp.listen_ip != Ipv4Addr::UNSPECIFIED {
        ctx.ftp.listen_ip
    } else {
        sess.local_ip
    }
}

/// 打开数据传输连接（被动模式 accept / 主动模式 connect）
fn open_data_connection(sess: &mut FtpSession, ctx: &Arc<ServerContext>) -> Result<TcpStream, String> {
    if let Some(listener) = sess.passive_listener.take() {
        match listener.accept() {
            Ok((s, _)) => Ok(s),
            Err(e) => Err(format!("data accept: {e}")),
        }
    } else if let Some(addr) = sess.data_addr.take() {
        match TcpStream::connect_timeout(&addr, Duration::from_secs(ctx.ftp.timeout_secs.min(30).max(5))) {
            Ok(s) => Ok(s),
            Err(e) => Err(format!("data connect: {e}")),
        }
    } else {
        Err("no data connection".to_string())
    }
}

/// LIST / NLST 目录列表传输
fn transfer_list(sess: &mut FtpSession, stream: &mut TcpStream, arg: &str, ctx: &Arc<ServerContext>, long: bool) -> Option<String> {
    let _ = arg; // LIST 忽略参数，列出当前目录
    if !sess.authenticated {
        return Some("530 Not logged in\r\n".to_string());
    }
    // 先发 150 预告（在 accept 数据连接之前），否则等待 150 后才连数据端口的客户端会与 accept 死锁
    let _ = write_all(stream, b"150 Opening data connection\r\n");
    let mut data = match open_data_connection(sess, ctx) {
        Ok(d) => d,
        Err(_) => {
            let _ = write_all(stream, b"425 Cannot open data connection\r\n");
            return Some("425 Cannot open data connection\r\n".to_string());
        }
    };
    let entries = match fs::read_dir(&sess.cwd) {
        Ok(rd) => rd.flatten().collect::<Vec<_>>(),
        Err(_) => {
            let _ = write_all(&mut data, b"550 Directory unavailable\r\n");
            return Some("550 Directory unavailable\r\n".to_string());
        }
    };
    for entry in &entries {
        let name = entry.file_name().to_string_lossy().to_string();
        let is_dir = entry.path().is_dir();
        if long {
            let perm = if is_dir { "drwxr-xr-x" } else { "-rw-r--r--" };
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            let modified = entry
                .metadata()
                .and_then(|m| m.modified())
                .map(|t| Local::now().format("%Y-%m-%d %H:%M").to_string())
                .unwrap_or_default();
            let line = format!("{} 1 owner group {:>10} {} {}\r\n", perm, size, modified, name);
            let _ = write_all(&mut data, line.as_bytes());
        } else {
            let _ = write_all(&mut data, format!("{}\r\n", name).as_bytes());
        }
    }
    drop(entries);
    Some("226 Transfer complete\r\n".to_string())
}

/// RETR / STOR 文件传输
fn transfer_file(sess: &mut FtpSession, stream: &mut TcpStream, arg: &str, ctx: &Arc<ServerContext>, operation: TransferOperation) -> Option<String> {
    if !sess.authenticated {
        return Some("530 Not logged in\r\n".to_string());
    }
    let allowed = match operation {
        TransferOperation::Upload => ctx.allow_upload,
        TransferOperation::Download => ctx.allow_download,
    };
    if !allowed {
        return Some("550 Permission denied\r\n".to_string());
    }
    let full = match secure_join(&sess.cwd, arg) {
        Some(p) => p,
        None => return Some("550 Invalid path\r\n".to_string()),
    };
    // 先按操作准备文件并发送 150 预告，再 accept 数据连接，避免等待 150 后才连数据端口的客户端死锁
    match operation {
        TransferOperation::Download => {
            // 先打开文件确认存在（不存在直接 550，不发 150）
            let mut file = match fs::File::open(&full) {
                Ok(f) => f,
                Err(_) => return Some("550 File not found\r\n".to_string()),
            };
            let size = file.metadata().ok().map(|m| m.len()).unwrap_or(0);
            let _ = write_all(stream, b"150 Opening data connection\r\n");
            let mut data = match open_data_connection(sess, ctx) {
                Ok(d) => d,
                Err(_) => {
                    let _ = write_all(stream, b"425 Cannot open data connection\r\n");
                    return Some("425 Cannot open data connection\r\n".to_string());
                }
            };
            let mut buf = [0u8; 8192];
            let mut total = 0u64;
            loop {
                match file.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if data.write(&buf[..n]).is_err() {
                            break;
                        }
                        total += n as u64;
                    }
                    Err(_) => break,
                }
            }
            let status = if total == size { TransferStatus::Success } else { TransferStatus::Failure };
            record_transfer(ctx, operation, arg, total, status, None);
            Some(if status == TransferStatus::Success {
                "226 Transfer complete\r\n".to_string()
            } else {
                "426 Transfer failed\r\n".to_string()
            })
        }
        TransferOperation::Upload => {
            // 先发 150 预告，再 accept 数据连接
            let _ = write_all(stream, b"150 Accepted file\r\n");
            let mut data = match open_data_connection(sess, ctx) {
                Ok(d) => d,
                Err(_) => {
                    let _ = write_all(stream, b"425 Cannot open data connection\r\n");
                    return Some("425 Cannot open data connection\r\n".to_string());
                }
            };
            let mut file = match fs::File::create(&full) {
                Ok(f) => f,
                Err(_) => {
                    let _ = write_all(&mut data, b"550 Cannot create file\r\n");
                    return Some("550 Cannot create file\r\n".to_string());
                }
            };
            let mut buf = [0u8; 8192];
            let mut total = 0u64;
            loop {
                match data.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => match file.write(&buf[..n]) {
                        Ok(_) => total += n as u64,
                        Err(_) => break,
                    },
                    Err(_) => break,
                }
            }
            let _ = file.flush();
            let status = TransferStatus::Success;
            record_transfer(ctx, operation, arg, total, status, None);
            Some("226 Transfer complete\r\n".to_string())
        }
    }
}

/// 向流写入全部字节
fn write_all<W: Write>(stream: &mut W, data: &[u8]) -> std::io::Result<()> {
    stream.write_all(data)
}

/// 读取一行（以 \n 结尾）；超时时返回 WouldBlock
fn read_line<R: std::io::BufRead>(reader: &mut R) -> std::io::Result<String> {
    let mut buf = Vec::new();
    let mut b = [0u8; 1];
    loop {
        match reader.read(&mut b) {
            Ok(0) => return Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof)),
            Ok(_) => {
                let c = b[0];
                if c == b'\n' {
                    break;
                }
                buf.push(c);
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if buf.is_empty() {
                    return Err(std::io::Error::from(std::io::ErrorKind::WouldBlock));
                }
            }
            Err(e) => return Err(e),
        }
    }
    // 去掉末尾可能的 \r
    if buf.last() == Some(&b'\r') {
        buf.pop();
    }
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

// ============================ TFTP ============================

/// TFTP 服务端主循环（单线程：依次处理每个请求）
fn run_tftp_server(ctx: Arc<ServerContext>, listener: UdpSocket) {
    // 本环境的 std 未提供 UdpSocket::set_timeout，改用 set_read_timeout 以便响应停止请求。
    let _ = listener.set_read_timeout(Some(Duration::from_secs(1)));
    let mut buf = [0u8; 2048];
    while ctx.running.load(Ordering::Relaxed) {
        match listener.recv_from(&mut buf) {
            Ok((n, addr)) => {
                let pkt = buf[..n].to_vec();
                handle_tftp_request(&listener, pkt, addr, &ctx);
            }
            Err(_) => {
                if !ctx.running.load(Ordering::Relaxed) {
                    break;
                }
            }
        }
    }
}

/// 处理一条 TFTP 请求（READ / WRITE）
fn handle_tftp_request(listener: &UdpSocket, pkt: Vec<u8>, addr: SocketAddr, ctx: &Arc<ServerContext>) {
    if pkt.len() < 4 {
        return;
    }
    let opcode = ((pkt[0] as u16) << 8) | pkt[1] as u16;
    match opcode {
        1 => tftp_read(listener, pkt, addr, ctx),
        2 => tftp_write(listener, pkt, addr, ctx),
        _ => {}
    }
}

/// TFTP 读（下载）
fn tftp_read(
    listener: &UdpSocket,
    pkt: Vec<u8>,
    addr: SocketAddr,
    ctx: &Arc<ServerContext>,
) {
    // pkt 格式：opcode(2) + filename(\0) + mode(\0)；filename 从偏移 2 开始，以第一个 \0 截止
    let filename = String::from_utf8_lossy(&pkt[2..]).split('\0').next().unwrap_or("").to_string();
    if !ctx.allow_download {
        send_err(listener, addr, 2, &filename, "permission denied");
        return;
    }
    let full = match secure_join(&ctx.root, &filename) {
        Some(p) => p,
        None => {
            send_err(listener, addr, 2, &filename, "not found");
            return;
        }
    };
    let mut file = match fs::File::open(&full) {
        Ok(f) => f,
        Err(_) => {
            send_err(listener, addr, 2, &filename, "cannot open");
            return;
        }
    };
    let size = file.metadata().ok().map(|m| m.len()).unwrap_or(0);
    let start = Instant::now();

    let mut block: u16 = 1;
    let mut data = [0u8; TFTP_BLOCK_SIZE];
    let n = file.read(&mut data).unwrap_or(0);
    send_data(listener, addr, block, &data[..n]);
    let mut last = n < TFTP_BLOCK_SIZE;
    while !last && ctx.running.load(Ordering::Relaxed) {
        if !wait_ack(listener, block) {
            if !ctx.running.load(Ordering::Relaxed) {
                break;
            }
        }
        block += 1;
        let n = file.read(&mut data).unwrap_or(0);
        send_data(listener, addr, block, &data[..n]);
        last = n < TFTP_BLOCK_SIZE;
    }
    // 确认最后一个块
    let _ = wait_ack(listener, block);

    let status = if ctx.running.load(Ordering::Relaxed) {
        TransferStatus::Success
    } else {
        TransferStatus::Failure
    };
    record_transfer(ctx, TransferOperation::Download, &filename, size, status, None);
}

/// TFTP 写（上传）
fn tftp_write(
    listener: &UdpSocket,
    pkt: Vec<u8>,
    addr: SocketAddr,
    ctx: &Arc<ServerContext>,
) {
    // pkt 格式：opcode(2) + filename(\0) + mode(\0)；filename 从偏移 2 开始，以第一个 \0 截止
    let filename = String::from_utf8_lossy(&pkt[2..]).split('\0').next().unwrap_or("").to_string();
    if !ctx.allow_upload {
        send_err(listener, addr, 2, &filename, "permission denied");
        return;
    }
    let full = match secure_join(&ctx.root, &filename) {
        Some(p) => p,
        None => {
            send_err(listener, addr, 2, &filename, "not found");
            return;
        }
    };
    let mut file = match fs::File::create(&full) {
        Ok(f) => f,
        Err(_) => {
            send_err(listener, addr, 2, &filename, "cannot create");
            return;
        }
    };
    let start = Instant::now();
    // 确认 0 块，开始接收
    send_ack(listener, addr, 0);

    let mut block: u16 = 1;
    let mut data = [0u8; TFTP_BLOCK_SIZE];
    let mut total = 0u64;
    let mut last = false;
    while ctx.running.load(Ordering::Relaxed) {
        match recv_packet(listener, &mut data) {
            Ok((op, blk, len)) if op == 3 && blk == block => {
                if len > 0 {
                    // data 前 4 字节为 opcode+block 头，仅写入后面的 payload
                    if file.write(&data[4..4 + len]).is_err() {
                        break;
                    }
                    total += len as u64;
                }
                send_ack(listener, addr, block);
                block += 1;
                if len < TFTP_BLOCK_SIZE {
                    last = true;
                }
            }
            Ok(_) => { /* 忽略重复/乱序包 */ }
            Err(_) => {
                if !ctx.running.load(Ordering::Relaxed) {
                    break;
                }
            }
        }
        if last {
            break;
        }
    }
    let _ = file.flush();

    let status = if ctx.running.load(Ordering::Relaxed) {
        TransferStatus::Success
    } else {
        TransferStatus::Failure
    };
    record_transfer(ctx, TransferOperation::Upload, &filename, total, status, None);
}

/// 发送一个 DATA 块（opcode 3）
fn send_data(listener: &UdpSocket, addr: SocketAddr, block: u16, data: &[u8]) {
    let mut pkt = Vec::with_capacity(4 + data.len());
    pkt.extend_from_slice(&[0x00, 0x03]);
    pkt.extend_from_slice(&block.to_be_bytes());
    pkt.extend_from_slice(data);
    let _ = listener.send_to(&pkt, addr);
}

/// 发送一个 ACK（opcode 4）
fn send_ack(listener: &UdpSocket, addr: SocketAddr, block: u16) {
    let mut pkt = [0u8; 4];
    pkt[0] = 0x00;
    pkt[1] = 0x04;
    pkt[2..4].copy_from_slice(&block.to_be_bytes());
    let _ = listener.send_to(&pkt, addr);
}

/// 发送一个错误包（opcode 5）
fn send_err(listener: &UdpSocket, addr: SocketAddr, code: u16, filename: &str, msg: &str) {
    let mut pkt = Vec::new();
    pkt.extend_from_slice(&[0x00, 0x05]);
    pkt.extend_from_slice(&code.to_be_bytes());
    let mut m = msg.to_string();
    if filename.contains('.') {
        m = format!("{msg} {filename}");
    }
    pkt.extend_from_slice(m.as_bytes());
    let _ = listener.send_to(&pkt, addr);
}

/// 等待指定块号的 ACK；超时返回 false
fn wait_ack(listener: &UdpSocket, expected: u16) -> bool {
    let mut buf = [0u8; 512];
    loop {
        match listener.recv_from(&mut buf) {
            Ok((n, _)) if n >= 4 => {
                let op = ((buf[0] as u16) << 8) | buf[1] as u16;
                let blk = u16::from_be_bytes([buf[2], buf[3]]);
                if op == 4 && blk == expected {
                    return true;
                }
            }
            Ok(_) => {}
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => return false,
            Err(_) => return false,
        }
    }
}

/// 接收一个数据包，返回 (opcode, block, len)
fn recv_packet(listener: &UdpSocket, buf: &mut [u8]) -> std::io::Result<(u16, u16, usize)> {
    let n = listener.recv(buf)?;
    if n < 4 {
        return Err(std::io::Error::from(std::io::ErrorKind::InvalidData));
    }
    let op = ((buf[0] as u16) << 8) | buf[1] as u16;
    let blk = u16::from_be_bytes([buf[2], buf[3]]);
    Ok((op, blk, n - 4))
}

#[cfg(test)]
mod tftp_e2e_tests {
    // 仅测试真实协议逻辑，不依赖 GUI 运行时。
    use super::{
        run_tftp_server, FtpServerConfig, FileProtocol, ServerContext, SharedStats,
    };
    use std::collections::VecDeque;
    use std::io::Read;
    use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::Duration;

    fn make_ctx(root: PathBuf) -> Arc<ServerContext> {
        Arc::new(ServerContext {
            root: Arc::new(root),
            protocol: FileProtocol::TFTP,
            ftp: FtpServerConfig {
                username: String::new(),
                password: String::new(),
                passive_start: 0,
                passive_end: 0,
                timeout_secs: 0,
                listen_ip: Ipv4Addr::LOCALHOST,
            },
            allow_upload: true,
            allow_download: true,
            stats: Arc::new(Mutex::new(SharedStats::default())),
            records: Arc::new(Mutex::new(VecDeque::new())),
            running: Arc::new(AtomicBool::new(true)),
        })
    }

    fn send(client: &UdpSocket, dst: SocketAddr, data: &[u8]) {
        client.send_to(data, dst).expect("udp send");
    }

    fn recv<'a>(client: &UdpSocket, buf: &'a mut [u8]) -> (usize, &'a [u8]) {
        let (n, _) = client.recv_from(buf).expect("udp recv");
        (n, &buf[..n])
    }

    #[test]
    fn download_transfers_full_content() {
        let dir = std::env::temp_dir().join(format!("netlite_tftp_dl_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file_path = dir.join("download.txt");
        let content = b"hello tftp download \x00\x01\x02 binary payload";
        std::fs::write(&file_path, content).unwrap();

        let ctx = make_ctx(dir.clone());
        let listener = UdpSocket::bind("127.0.0.1:0").unwrap();
        let server_addr = listener.local_addr().unwrap();
        let server_ctx = Arc::clone(&ctx);
        let server = thread::spawn(move || run_tftp_server(server_ctx, listener));

        let client = UdpSocket::bind("127.0.0.1:0").unwrap();
        client.set_read_timeout(Some(Duration::from_secs(2))).unwrap();

        // READ 请求：opcode(00 01) + "download.txt\0octet\0"
        let mut req = vec![0x00, 0x01];
        req.extend_from_slice(b"download.txt\0octet\0");
        send(&client, server_addr, &req);

        // 接收 DATA 块 1 并 ACK
        let mut buf = [0u8; 520];
        let (n, pkt) = recv(&client, &mut buf);
        assert_eq!(&pkt[0..2], &[0x00, 0x03], "expected DATA opcode");
        assert_eq!(&pkt[2..4], &[0x00, 0x01], "expected block 1");
        let received = &pkt[4..n];
        send(&client, server_addr, &[0x00, 0x04, 0x00, 0x01]);

        assert_eq!(received, content, "downloaded content must match");

        ctx.running.store(false, Ordering::Relaxed);
        let _ = server.join();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn upload_transfers_full_content() {
        let dir = std::env::temp_dir().join(format!("netlite_tftp_ul_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let ctx = make_ctx(dir.clone());
        let listener = UdpSocket::bind("127.0.0.1:0").unwrap();
        let server_addr = listener.local_addr().unwrap();
        let server_ctx = Arc::clone(&ctx);
        let server = thread::spawn(move || run_tftp_server(server_ctx, listener));

        let client = UdpSocket::bind("127.0.0.1:0").unwrap();
        client.set_read_timeout(Some(Duration::from_secs(2))).unwrap();

        // WRITE 请求：opcode(00 02) + "upload.txt\0octet\0"
        let mut req = vec![0x00, 0x02];
        req.extend_from_slice(b"upload.txt\0octet\0");
        send(&client, server_addr, &req);

        // 接收 ACK 0
        let mut buf = [0u8; 520];
        let (_n, pkt) = recv(&client, &mut buf);
        assert_eq!(&pkt[0..2], &[0x00, 0x04], "expected ACK opcode");
        assert_eq!(&pkt[2..4], &[0x00, 0x00], "expected block 0");

        // 上传内容并发送 DATA 块 1
        let content = b"hello tftp upload \x00\x01\x02 binary payload";
        let mut data = vec![0x00, 0x03, 0x00, 0x01];
        data.extend_from_slice(content);
        send(&client, server_addr, &data);

        // 接收 ACK 1
        let (_n, pkt) = recv(&client, &mut buf);
        assert_eq!(&pkt[0..2], &[0x00, 0x04], "expected ACK opcode");
        assert_eq!(&pkt[2..4], &[0x00, 0x01], "expected block 1");

        // 校验文件内容
        let mut written = Vec::new();
        std::fs::File::open(dir.join("upload.txt"))
            .unwrap()
            .read_to_end(&mut written)
            .unwrap();
        assert_eq!(written, content, "uploaded content must match");

        ctx.running.store(false, Ordering::Relaxed);
        let _ = server.join();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
