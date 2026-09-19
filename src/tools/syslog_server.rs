//! Syslog 服务器：轻量级 Syslog 接收器 + 原始日志查看器
//!
//! 设计原则（来自设计方案）：
//! “NetLite 负责接收和展示，不擅自改变设备原始日志”。
//!
//! - 只负责接收（UDP/TCP）并按原始字节展示，不做任何格式解析或改写。
//! - 仅记录 NetLite 的“接收时间”，不解析设备日志内部的时间字段。
//! - V1 无数据库、无 ES、无仪表盘/告警、无权限/多租户、无规则引擎、无 TLS。

use std::collections::{HashSet, VecDeque};
use std::io::BufRead;
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use gpui::prelude::*;
use gpui::*;
use gpui_component::clipboard::Clipboard;
use gpui_component::input::Input;
use gpui_component::scroll::ScrollableElement;
use gpui_component::{ActiveTheme, Icon, IconName, StyledExt};

use crate::app::NetLiteApp;

/// 内存中保留的最大接收条目数（超过后丢弃最旧的）
const MAX_RECEIVED: usize = 10000;
/// 轮询间隔（毫秒）
const POLL_INTERVAL_MS: u64 = 100;
/// 接收套接字读超时（毫秒）——用于响应停止请求
const READ_TIMEOUT_MS: u64 = 200;
/// 详情面板宽度（首选值，窗口变窄时按 min_w 缩小）
const DETAIL_WIDTH: Pixels = px(560.0);
/// 判定“已滚动到底部”的像素容差
const BOTTOM_EPSILON: Pixels = px(2.0);
/// 表格行原始信息预览最大字符数
const RAW_PREVIEW_MAX: usize = 80;

/// Syslog 传输协议
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SyslogProtocol {
    UDP,
    TCP,
}

impl SyslogProtocol {
    fn label(self) -> &'static str {
        match self {
            SyslogProtocol::UDP => "UDP",
            SyslogProtocol::TCP => "TCP",
        }
    }
}

/// 接收服务状态
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SyslogStatus {
    /// 未监听
    Idle,
    /// 正在监听
    Listening,
    /// 监听失败（如端口被占用）
    Failed,
}

/// 单条 Syslog 记录
///
/// `raw_message` 严格保留设备发送的原始内容，NetLite 不做任何改写。
#[derive(Clone, Serialize, Deserialize)]
pub struct SyslogEntry {
    /// NetLite 接收到该日志的时间（YYYY-MM-DD HH:MM:SS）
    pub received_at: String,
    /// 发送方的 IP 地址
    pub source_ip: String,
    /// 发送方的端口
    pub source_port: String,
    /// 传输协议
    pub protocol: String,
    /// 设备发送的原始日志内容（保持原样）
    pub raw_message: String,
}

/// Syslog 服务器工具的状态
pub struct SyslogServerState {
    /// 接收服务状态
    pub status: SyslogStatus,
    /// 错误信息（如端口被占用）
    pub error_message: Option<String>,
    /// 当前选中的协议（UDP / TCP）
    pub protocol: SyslogProtocol,

    /// 已接收的全部记录（受 MAX_RECEIVED 限制）
    pub received: Vec<SyslogEntry>,
    /// 当前正在展示的记录（受“暂停显示”影响，可能滞后于 received）
    pub displayed: Vec<SyslogEntry>,
    /// received 中已同步进 displayed 的数量
    pub synced_count: usize,
    /// 累计接收总数（不受暂停/清空影响，仅随接收增长）
    pub total: u64,
    /// 已知的来源设备数量
    pub source_devices: HashSet<String>,

    /// 是否自动滚动到底部
    pub auto_scroll: bool,
    /// 是否暂停显示（后台仍在接收）
    pub paused: bool,
    /// 详情面板选中的行索引（指向 displayed）
    pub selected_index: Option<usize>,
    /// 滚动句柄，用于程序化自动滚动
    pub scroll_handle: ScrollHandle,
    /// 当前是否正在跟随到底部（用于控制”查看最新”按钮的显隐）
    pub following: bool,
    /// 是否展示日志详情面板（点击行内”详情”按钮后为 true）
    pub show_detail: bool,
    /// 是否显示”清空”确认
    pub show_clear_confirm: bool,
    /// 接收速率（条/秒，最近 1 秒滑动窗口）
    pub rate: f64,

    /// 最近接收时刻（用于计算速率）
    recv_times: VecDeque<Instant>,

    /// 与后台接收线程共享的接收缓冲区
    pub recv_shared: Arc<Mutex<Vec<SyslogEntry>>>,
    /// 接收线程的运行标志
    pub running: Arc<AtomicBool>,
    /// 接收线程回传的错误信息
    pub error_shared: Arc<Mutex<Option<String>>>,
}

impl SyslogServerState {
    pub fn new(_cx: &mut Context<Self>) -> Self {
        Self {
            status: SyslogStatus::Idle,
            error_message: None,
            protocol: SyslogProtocol::UDP,
            received: Vec::new(),
            displayed: Vec::new(),
            synced_count: 0,
            total: 0,
            source_devices: HashSet::new(),
            auto_scroll: true,
            paused: false,
            selected_index: None,
            scroll_handle: ScrollHandle::new(),
            following: false,
            show_detail: false,
            show_clear_confirm: false,
            rate: 0.0,
            recv_times: VecDeque::new(),
            recv_shared: Arc::new(Mutex::new(Vec::new())),
            running: Arc::new(AtomicBool::new(false)),
            error_shared: Arc::new(Mutex::new(None)),
        }
    }

    /// 将后台线程接收到的记录并入状态，并处理自动滚动。每次由轮询任务调用。
    fn drain_received(&mut self, entries: Vec<SyslogEntry>) {
        if entries.is_empty() {
            return;
        }

        // 速率：最近 1 秒滑动窗口
        for _ in 0..entries.len() {
            self.recv_times.push_back(Instant::now());
        }
        let cutoff = Instant::now() - Duration::from_secs(1);
        while let Some(front) = self.recv_times.front() {
            if *front < cutoff {
                self.recv_times.pop_front();
            } else {
                break;
            }
        }
        self.rate = self.recv_times.len() as f64;

        // 统计来源设备：按来源 IP 去重计数
        for entry in entries.iter() {
            self.source_devices.insert(entry.source_ip.clone());
        }

        // 并入已接收缓冲，并做上限裁剪
        self.total += entries.len() as u64;
        self.received.extend(entries);
        if self.received.len() > MAX_RECEIVED {
            let excess = self.received.len() - MAX_RECEIVED;
            self.received.drain(0..excess);
        }

        // 未暂停时，将新记录同步进展示缓冲
        if !self.paused {
            // received[0] 对应的逻辑序号（考虑最旧记录被裁剪）
            let base = (self.total - self.received.len() as u64) as usize;
            for (i, entry) in self.received.iter().enumerate() {
                if base + i >= self.synced_count {
                    self.displayed.push(entry.clone());
                }
            }
            self.synced_count = self.total as usize;
        }

        // 判断是否处于底部，并决定是否自动滚动
        let max_y = self.scroll_handle.max_offset().y;
        let offset_y = self.scroll_handle.offset().y;
        let at_bottom = max_y <= BOTTOM_EPSILON || (-offset_y) >= (max_y - BOTTOM_EPSILON);
        self.following = self.auto_scroll && !self.paused && at_bottom;
        if self.following {
            self.scroll_handle.scroll_to_bottom();
        }
    }

    /// 根据当前搜索关键字过滤出待展示的行
    fn visible_rows(&self, query: &str) -> Vec<&SyslogEntry> {
        let query = query.trim().to_lowercase();
        if query.is_empty() {
            return self.displayed.iter().collect();
        }
        self.displayed
            .iter()
            .filter(|e| {
                e.raw_message.to_lowercase().contains(&query)
                    || e.source_ip.to_lowercase().contains(&query)
                    || e.source_port.to_lowercase().contains(&query)
                    || e.protocol.to_lowercase().contains(&query)
            })
            .collect()
    }

    /// 选中的记录
    fn selected_entry(&self) -> Option<&SyslogEntry> {
        self.selected_index.and_then(|i| self.displayed.get(i))
    }
}

/// Syslog 服务器工具
pub struct SyslogServerTool<'a> {
    app: &'a NetLiteApp,
    state: Entity<SyslogServerState>,
}

impl<'a> SyslogServerTool<'a> {
    pub fn new(app: &'a NetLiteApp) -> Self {
        Self {
            app,
            state: app.syslog_server_state.clone(),
        }
    }

    pub fn render(self, _window: &mut Window, cx: &mut Context<NetLiteApp>) -> AnyElement {
        let state = self.state.clone();
        let theme = cx.theme().clone();

        let s = state.read(cx);
        let error = s.error_message.clone();
        let show_clear_confirm = s.show_clear_confirm;

        div()
            .flex()
            .flex_col()
            .flex_1()
            .overflow_hidden()
            .bg(theme.background)
            .child(Self::render_config_bar(state.clone(), self.app, cx))
            .child(Self::render_stats_bar(state.clone(), cx))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .child(Self::render_search_bar(state.clone(), self.app, cx))
                    .child(Self::render_log_area(state.clone(), self.app, cx))
                    .children(
                        show_clear_confirm
                            .then(|| Self::render_clear_confirm(state.clone(), cx).into_any_element()),
                    ),
            )
            .child(Self::render_footer(state.clone(), cx))
            .children(error.as_ref().map(|err| {
                div()
                    .absolute()
                    .bottom_6()
                    .left_0()
                    .right_0()
                    .flex()
                    .justify_center()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .px_3()
                            .py_1p5()
                            .rounded_md()
                            .bg(theme.red.opacity(0.15))
                            .border_1()
                            .border_color(theme.red.opacity(0.4))
                            .text_xs()
                            .font_medium()
                            .text_color(theme.red)
                            .child(Icon::new(IconName::TriangleAlert).size(px(12.0)))
                            .child(err.clone())
                    )
                    .into_any_element()
            }))
            // 点击工具区域任意位置（详情面板外）时自动关闭日志详情
            .on_mouse_down(MouseButton::Left, cx.listener(move |_, _, _, cx| {
                let st = state.clone();
                st.update(cx, |s, _| {
                    s.show_detail = false;
                });
                cx.notify();
            }))
            .into_any_element()
    }

    /// 顶部：监听配置 + 开始/停止
    fn render_config_bar(
        state: Entity<SyslogServerState>,
        app: &NetLiteApp,
        cx: &mut Context<NetLiteApp>,
    ) -> impl IntoElement {
        let s = state.read(cx);
        let theme = cx.theme().clone();
        let is_listening = s.status == SyslogStatus::Listening;

        div()
            .flex()
            .items_center()
            .gap_3()
            .w_full()
            .px_4()
            .py_2()
            .bg(theme.background)
            .border_b_1()
            .border_color(theme.border)
            .child(status_dot(s.status))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .text_sm()
                            .font_medium()
                            .text_color(theme.muted_foreground)
                            .child("监听 IP"),
                    )
                    .child(Input::new(&app.syslog_ip_input).w_40().h_8().appearance(false))
                    .child(
                        div()
                            .text_sm()
                            .font_medium()
                            .text_color(theme.muted_foreground)
                            .child("端口"),
                    )
                    .child(Input::new(&app.syslog_port_input).w_20().h_8().appearance(false)),
            )
            .child(protocol_toggle(state.clone(), s.protocol, is_listening, cx))
            .child(start_button(state.clone(), is_listening, cx))
            .into_any_element()
    }

    /// 统计栏：收到日志 / 来源设备 / 接收速率
    fn render_stats_bar(state: Entity<SyslogServerState>, cx: &mut Context<NetLiteApp>) -> impl IntoElement {
        let s = state.read(cx);
        let theme = cx.theme().clone();
        let total = s.total;
        let devices = s.source_devices.len();
        let rate = s.rate;

        div()
            .flex()
            .items_center()
            .gap_4()
            .w_full()
            .px_4()
            .py_1p5()
            .bg(theme.background.opacity(0.4))
            .border_b_1()
            .border_color(theme.border)
            .child(stat_item("收到日志", &total.to_string(), gpui::rgb(0x55AA7F)))
            .child(stat_item("来源设备", &devices.to_string(), gpui::rgb(0x5AA3F5)))
            .child(stat_item("接收速率", &format!("{:.1}/s", rate), gpui::rgb(0xF5C542)))
            .into_any_element()
    }

    /// 搜索栏：搜索框 + 查看最新 + 计数
    fn render_search_bar(
        state: Entity<SyslogServerState>,
        app: &NetLiteApp,
        cx: &mut Context<NetLiteApp>,
    ) -> impl IntoElement {
        let s = state.read(cx);
        let theme = cx.theme().clone();
        let query = app.syslog_search_input.read_with(cx, |i, _| i.text().to_string());
        let visible = s.visible_rows(&query).len();

        div()
            .flex()
            .items_center()
            .gap_2()
            .w_full()
            .px_4()
            .py_2()
            .bg(theme.background)
            .border_b_1()
            .border_color(theme.border)
            .child(Input::new(&app.syslog_search_input).w_0().flex_1().h_8().appearance(false))
            .children(if s.following || s.paused {
                None
            } else {
                Some(Self::jump_latest_button(state.clone(), cx).into_any_element())
            })
            .child(
                div()
                    .text_xs()
                    .font_medium()
                    .text_color(theme.muted_foreground)
                    .child(format!("显示 {} 条", visible)),
            )
            .into_any_element()
    }

    /// 日志表格 + 右侧详情面板
    fn render_log_area(
        state: Entity<SyslogServerState>,
        app: &NetLiteApp,
        cx: &mut Context<NetLiteApp>,
    ) -> impl IntoElement {
        let theme = cx.theme().clone();
        let query = app.syslog_search_input.read_with(cx, |i, _| i.text().to_string());
        // 先在块内读取状态并拷贝出所需数据，读完即释放对 cx 的不可变借用，
        // 避免后续 header_row(cx) / render_detail(state, cx) 的可变借用冲突。
        let (scroll_handle, rows, selected_index, show_detail) = {
            let s = state.read(cx);
            (
                s.scroll_handle.clone(),
                s.visible_rows(&query)
                    .into_iter()
                    .cloned()
                    .collect::<Vec<SyslogEntry>>(),
                s.selected_index,
                s.show_detail,
            )
        };

        div()
            .flex()
            .flex_1()
            .min_h_0()
            .bg(theme.background)
            .child(
                div()
                    .id(ElementId::Name("syslog_log_area".into()))
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .overflow_y_scroll()
                    .track_scroll(&scroll_handle)
                    .vertical_scrollbar(&scroll_handle)
                    .child(header_row(cx))
                    .children(render_rows(
                        state.clone(), cx, rows, selected_index, show_detail,
                    )),
            )
            .when(show_detail, |div| {
                div.child(Self::render_detail_modal(state, cx))
            })
            .into_any_element()
    }

    /// 日志详情弹窗：点击“详情”时居中弹出，点击蒙版或关闭按钮收起。
    fn render_detail_modal(state: Entity<SyslogServerState>, cx: &mut Context<NetLiteApp>) -> impl IntoElement {
        let theme = cx.theme().clone();
        // 先在块内读取状态并拷贝出所需数据，读完即释放对 cx 的借用。
        let selected_entry = state.read(cx).selected_entry().cloned();
        // 蒙版闭包单独持有 state 的一个克隆，避免与后续 close_button(state.clone()) 的移动冲突。
        let modal_state = state.clone();

        // 点击蒙版收起详情
        let hide = cx.listener(move |_, _, _, cx| {
            modal_state.update(cx, |s, _| {
                s.show_detail = false;
            });
            cx.notify();
        });

        div()
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .child(
                // 蒙版：覆盖整个日志区，点击收起
                div()
                    .absolute()
                    .inset_0()
                    .bg(gpui::rgba(0x80000000))
                    .on_mouse_down(MouseButton::Left, hide),
            )
            .child(
                div()
                    .w(DETAIL_WIDTH)
                    .max_w(px(640.0))
                    .max_h(px(560.0))
                    .bg(theme.background)
                    .border_1()
                    .border_color(theme.border)
                    .rounded_lg()
                    .shadow_2xl()
                    .flex()
                    .flex_col()
                    .overflow_hidden()
                    // 点击弹窗本体不收起
                    .on_mouse_down(MouseButton::Left, cx.listener(|_, _, _, cx| {
                        cx.stop_propagation();
                    }))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .px_4()
                            .py_3()
                            .border_b_1()
                            .border_color(theme.border)
                            .child(
                                div()
                                    .text_sm()
                                    .font_bold()
                                    .text_color(theme.foreground)
                                    .child("日志详情"),
                            )
                            .child(close_button(state.clone(), cx)),
                    )
                    .child(
                        div()
                            .relative()
                            .flex_1()
                            .min_h_0()
                            .overflow_hidden()
                            .px_4()
                            .py_3()
                            .gap_3()
                            .children({
                                let items: Vec<AnyElement> = match selected_entry {
                                    Some(entry) => {
                                        let mut v: Vec<AnyElement> = Vec::with_capacity(7);
                                        for (label, value) in [
                                            ("接收时间", &entry.received_at),
                                            ("来源 IP", &entry.source_ip),
                                            ("来源端口", &entry.source_port),
                                            ("协议", &entry.protocol),
                                        ] {
                                            v.push(detail_row(label, value, cx).into_any_element());
                                        }
                                        v.push(
                                            div()
                                                .text_sm()
                                                .font_medium()
                                                .text_color(theme.muted_foreground)
                                                .child("原始信息")
                                                .into_any_element(),
                                        );
                                        v.push(
                                            div()
                                                .rounded_md()
                                                .px_2()
                                                .py_1p5()
                                                .flex_1()
                                                .min_h_0()
                                                .overflow_hidden()
                                                .bg(theme.border.opacity(0.3))
                                                .text_xs()
                                                .font_medium()
                                                .text_color(theme.foreground.opacity(0.9))
                                                .children(
                                                    entry
                                                        .raw_message
                                                        .split('\n')
                                                        .map(|line| {
                                                            div().child(line.to_string()).into_any_element()
                                                        })
                                                )
                                                .into_any_element(),
                                        );
                                        v.push(Self::copy_button(entry.raw_message.clone(), cx).into_any_element());
                                        v
                                    }
                                    None => vec![div()
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .flex_1()
                                        .text_sm()
                                        .text_color(theme.muted_foreground.opacity(0.6))
                                        .child("点击某行查看原始日志")
                                        .into_any_element()],
                                };
                                items
                            })
                    )
            )
    }

    /// 底部操作栏：暂停 / 自动滚动 / 清空 / 导出
    fn render_footer(
        state: Entity<SyslogServerState>,
        cx: &mut Context<NetLiteApp>,
    ) -> impl IntoElement {
        let s = state.read(cx);
        let theme = cx.theme().clone();
        let is_listening = s.status == SyslogStatus::Listening;
        let paused = s.paused;
        let auto_scroll = s.auto_scroll;

        div()
            .flex()
            .items_center()
            .gap_2()
            .w_full()
            .px_4()
            .py_2()
            .bg(theme.background)
            .border_t_1()
            .border_color(theme.border)
            .child(pause_button(state.clone(), is_listening, paused, cx))
            .child(auto_scroll_toggle(state.clone(), auto_scroll, is_listening, cx))
            .child(div().w_px().h_5().bg(theme.border))
            .child(clear_button(state.clone(), is_listening, cx))
            .child(div().flex_1())
            .child(export_button("TXT", ExportFormat::Txt, cx))
            .child(export_button("CSV", ExportFormat::Csv, cx))
            .child(export_button("JSON", ExportFormat::Json, cx))
            .into_any_element()
    }

    /// 清空确认条
    fn render_clear_confirm(
        state: Entity<SyslogServerState>,
        cx: &mut Context<NetLiteApp>,
    ) -> impl IntoElement {
        let theme = cx.theme().clone();
        let state_cancel = state.clone();

        div()
            .flex()
            .items_center()
            .justify_center()
            .gap_3()
            .w_full()
            .px_4()
            .py_2()
            .bg(theme.red.opacity(0.08))
            .child(
                div()
                    .text_xs()
                    .font_medium()
                    .text_color(theme.red)
                    .child("确定要清空已接收的日志吗？此操作不可撤销。"),
            )
            .child(
                small_button(
                    cx,
                    "取消",
                    theme.border,
                    theme.foreground,
                    true,
                    move |_, _, _, cx| {
                        let st = state_cancel.clone();
                        st.update(cx, |s, _| {
                            s.show_clear_confirm = false;
                        });
                        cx.notify();
                    },
                )
                .into_any_element(),
            )
            .child(
                small_button(
                    cx,
                    "确定清空",
                    theme.red,
                    theme.background,
                    true,
                    move |_, _, _, cx| {
                        let st = state.clone();
                        st.update(cx, |s, _| {
                            s.show_clear_confirm = false;
                            s.received.clear();
                            s.displayed.clear();
                            s.synced_count = 0;
                            s.total = 0;
                            s.source_devices.clear();
                            s.rate = 0.0;
                            s.selected_index = None;
                            s.paused = false;
                            s.following = false;
                        });
                        cx.notify();
                    },
                )
                .into_any_element(),
            )
            .into_any_element()
    }

    /// 跳到最新（自动滚动到底部）
    fn jump_latest_button(state: Entity<SyslogServerState>, cx: &mut Context<NetLiteApp>) -> impl IntoElement {
        small_button(
            cx,
            "↓ 查看最新",
            cx.theme().primary,
            cx.theme().background,
            true,
            move |_, _, _, cx| {
                let st = state.clone();
                st.update(cx, |s, _| {
                    s.following = true;
                    s.scroll_handle.scroll_to_bottom();
                });
                cx.notify();
            },
        )
    }

    /// 复制单条原始日志
    fn copy_button(raw: String, cx: &mut Context<NetLiteApp>) -> impl IntoElement {
        div()
            .flex()
            .items_center()
            .gap_1p5()
            .px_2p5()
            .py_1p5()
            .rounded_md()
            .bg(cx.theme().border.opacity(0.4))
            .text_xs()
            .font_medium()
            .text_color(cx.theme().foreground)
            .child(Icon::new(IconName::Copy).size(px(12.0)))
            .child("复制原始日志")
            .child(
                Clipboard::new(ElementId::Name("syslog-copy".into()))
                    .value(raw)
                    .into_any_element(),
            )
            .into_any_element()
    }

    /// 开始监听
    fn start_listening(app: &NetLiteApp, state: &Entity<SyslogServerState>, cx: &mut Context<NetLiteApp>) {
        // 读取配置：监听 IP / 端口 / 协议
        let ip_str = app.syslog_ip_input.read_with(cx, |i, _| i.text().to_string());
        let ip_str = ip_str.trim();
        let ip_str = if ip_str.is_empty() { "0.0.0.0" } else { ip_str };
        let port_text = app.syslog_port_input.read_with(cx, |i, _| i.text().to_string());
        let port: u16 = match port_text.trim().parse() {
            Ok(p) if (1..=65535).contains(&p) => p,
            _ if port_text.trim().is_empty() => 514,
            _ => {
                state.update(cx, |s, _| {
                    s.error_message = Some("端口无效，请输入 1-65535".into());
                    s.status = SyslogStatus::Failed;
                });
                cx.notify();
                return;
            }
        };
        let protocol = app.syslog_server_state.read(cx).protocol;
        let addr = match ip_str.parse::<IpAddr>() {
            Ok(ip) => SocketAddr::from((ip, port)),
            Err(_) => {
                state.update(cx, |s, _| {
                    s.error_message = Some("监听 IP 格式错误".into());
                    s.status = SyslogStatus::Failed;
                });
                cx.notify();
                return;
            }
        };

        // 创建共享缓冲与运行标志
        let recv_shared = Arc::new(Mutex::new(Vec::new()));
        let running = Arc::new(AtomicBool::new(true));
        let error_shared = Arc::new(Mutex::new(None));

        let t_recv = recv_shared.clone();
        let t_running = running.clone();
        let t_error = error_shared.clone();
        // 接收线程：绑定套接字并循环接收，将原始日志写入共享缓冲
        thread::spawn(move || {
            run_server(addr, protocol, t_recv, t_running, t_error);
        });

        // 轮询任务：每 100ms 将接收缓冲并入 UI 状态
        let p_recv = recv_shared.clone();
        let p_running = running.clone();
        let p_error = error_shared.clone();
        state.update(cx, |s, _| {
            s.recv_shared = recv_shared;
            s.running = running;
            s.error_shared = error_shared;
            s.status = SyslogStatus::Listening;
            s.error_message = None;
            s.following = false;
        });
        cx.notify();

        let p_state = state.clone();
        cx.spawn(async move |_, cx| {
            loop {
                smol::Timer::after(Duration::from_millis(POLL_INTERVAL_MS)).await;
                let drained = std::mem::take(&mut *p_recv.lock().unwrap());
                let stopped = !p_running.load(Ordering::Relaxed);
                let has_entries = !drained.is_empty();
                p_state.update(cx, |s, _| {
                    if has_entries {
                        s.drain_received(drained);
                    }
                    if stopped {
                        if let Some(err) = p_error.lock().unwrap().take() {
                            s.error_message = Some(err);
                            s.status = SyslogStatus::Failed;
                        } else {
                            s.status = SyslogStatus::Idle;
                        }
                    }
                });
                if stopped || has_entries {
                    cx.refresh();
                }
                if stopped {
                    break;
                }
            }
        })
        .detach();
    }

    /// 停止监听
    fn stop_listening(state: &Entity<SyslogServerState>, cx: &mut Context<NetLiteApp>) {
        let st = state.clone();
        st.update(cx, |s, _| {
            s.running.store(false, Ordering::Relaxed);
            s.status = SyslogStatus::Idle;
        });
        cx.notify();
    }
}

// ---------------------------------------------------------------------------
// 自由函数：按钮 / 行辅助
// ---------------------------------------------------------------------------

/// 状态指示点
fn status_dot(status: SyslogStatus) -> impl IntoElement {
    let (color, text) = match status {
        SyslogStatus::Idle => (gpui::rgb(0xCBCBCB), "未监听"),
        SyslogStatus::Listening => (gpui::rgb(0x55AA7F), "正在监听"),
        SyslogStatus::Failed => (gpui::rgb(0xFF8E77), "监听失败"),
    };
    div()
        .flex()
        .items_center()
        .gap_2()
        .child(div().w_2p5().h_2p5().rounded_full().bg(color))
        .child(div().text_xs().font_medium().text_color(gpui::rgb(0x666666)).child(text.to_string()))
}

/// 统计项
fn stat_item(label: &str, value: &str, accent: Rgba) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .gap_1p5()
        .child(div().w_2p5().h_2p5().rounded_full().bg(accent))
        .child(div().text_xs().text_color(gpui::rgb(0x888888)).child(label.to_string()))
        .child(div().text_sm().font_bold().text_color(gpui::rgb(0x222222)).child(value.to_string()))
}

/// 协议切换（UDP / TCP）
fn protocol_toggle(
    state: Entity<SyslogServerState>,
    protocol: SyslogProtocol,
    disabled: bool,
    cx: &mut Context<NetLiteApp>,
) -> impl IntoElement {
    let theme = cx.theme().clone();
    let segments = [SyslogProtocol::UDP, SyslogProtocol::TCP];
    div()
        .flex()
        .rounded_md()
        .border_1()
        .border_color(if disabled { theme.border } else { theme.primary })
        .opacity(if disabled { 0.5 } else { 1.0 })
        .overflow_hidden()
        .children(segments.iter().map(|p| {
            let active = *p == protocol;
            let proto = *p;
            let st = state.clone();
            div()
                .px_3()
                .py_1p5()
                .text_xs()
                .font_medium()
                .bg(if active { theme.primary } else { theme.background })
                .text_color(if active { theme.background } else { theme.foreground })
                .on_mouse_down(MouseButton::Left, cx.listener(move |_, _, _, cx| {
                    if disabled {
                        return;
                    }
                    st.update(cx, |s, _| {
                        s.protocol = proto;
                    });
                    cx.notify();
                }))
                .child(proto.label())
        }))
}

/// 开始 / 停止按钮
fn start_button(state: Entity<SyslogServerState>, is_listening: bool, cx: &mut Context<NetLiteApp>) -> impl IntoElement {
    let theme = cx.theme().clone();
    // 开始 / 停止按钮始终可点击（处理函数内部按状态分支），故 enabled 恒为 true。
    let (label, bg, text) = if is_listening {
        // 停止监听：红色实心 + 白字，避免灰色禁用样式误导为不可点击。
        ("停止监听", gpui::rgb(0xFF5A5A).into(), gpui::rgb(0xFFFFFF).into())
    } else {
        ("开始监听", theme.primary, theme.background)
    };
    small_button(cx, label, bg, text, true, move |app: &mut NetLiteApp, _, _, cx| {
        let state = state.clone();
        if is_listening {
            SyslogServerTool::stop_listening(&state, cx);
        } else {
            SyslogServerTool::start_listening(app, &state, cx);
        }
    })
}

/// 暂停 / 继续显示
fn pause_button(
    state: Entity<SyslogServerState>,
    enabled: bool,
    paused: bool,
    cx: &mut Context<NetLiteApp>,
) -> impl IntoElement {
    let theme = cx.theme().clone();
    let label = if paused { "继续显示" } else { "暂停显示" };
    small_button(
        cx,
        label,
        if paused { theme.primary } else { theme.border },
        theme.foreground,
        enabled,
        move |_, _, _, cx| {
            let st = state.clone();
            st.update(cx, |s, _| {
                s.paused = !s.paused;
            });
            cx.notify();
        },
    )
}

/// 自动滚动开关
fn auto_scroll_toggle(
    state: Entity<SyslogServerState>,
    enabled: bool,
    is_listening: bool,
    cx: &mut Context<NetLiteApp>,
) -> impl IntoElement {
    let theme = cx.theme().clone();
    small_button(
        cx,
        if enabled { "自动滚动: 开" } else { "自动滚动: 关" },
        if enabled { theme.primary } else { theme.border },
        theme.foreground,
        is_listening,
        move |_, _, _, cx| {
            let st = state.clone();
            st.update(cx, |s, _| {
                s.auto_scroll = !s.auto_scroll;
            });
            cx.notify();
        },
    )
}

/// 清空按钮
fn clear_button(state: Entity<SyslogServerState>, enabled: bool, cx: &mut Context<NetLiteApp>) -> impl IntoElement {
    small_button(
        cx,
        "清空",
        cx.theme().red,
        cx.theme().background,
        enabled,
        move |_, _, _, cx| {
            let st = state.clone();
            st.update(cx, |s, _| {
                s.show_clear_confirm = true;
            });
            cx.notify();
        },
    )
}

/// 通用小按钮
fn small_button<F>(
    cx: &mut Context<NetLiteApp>,
    label: &str,
    bg: Hsla,
    text_color: Hsla,
    enabled: bool,
    handler: F,
) -> impl IntoElement
where
    F: Fn(&mut NetLiteApp, &MouseDownEvent, &mut Window, &mut Context<NetLiteApp>) + 'static,
{
    div()
        .px_3()
        .py_1p5()
        .rounded_md()
        .bg(if enabled { bg } else { cx.theme().border.opacity(0.3) })
        .text_xs()
        .font_medium()
        .text_color(if enabled { text_color } else { cx.theme().muted_foreground })
        .opacity(if enabled { 1.0 } else { 0.6 })
        .on_mouse_down(MouseButton::Left, cx.listener(handler))
        .child(label.to_string())
}

/// 日志行上的“详情”按钮：点击后居中弹出日志详情弹窗
fn detail_button(state: Entity<SyslogServerState>, cx: &mut Context<NetLiteApp>) -> impl IntoElement {
    small_button(
        cx,
        "详情",
        cx.theme().primary,
        cx.theme().background,
        true,
        move |_, _, _, cx| {
            let st = state.clone();
            st.update(cx, |s, _| {
                s.show_detail = true;
            });
            // 阻止事件泡泡到整行，避免重复触发行选择
            cx.stop_propagation();
            cx.notify();
        },
    )
}

/// 详情弹窗右上角的关闭按钮（×）
fn close_button(state: Entity<SyslogServerState>, cx: &mut Context<NetLiteApp>) -> impl IntoElement {
    let theme = cx.theme();
    div()
        .flex()
        .items_center()
        .justify_center()
        .w_6()
        .h_6()
        .rounded_md()
        .bg(theme.border.opacity(0.3))
        .cursor_pointer()
        .text_sm()
        .font_bold()
        .text_color(theme.foreground)
        .child("×")
        .on_mouse_down(MouseButton::Left, cx.listener(move |_, _, _, cx| {
            state.update(cx, |s, _| {
                s.show_detail = false;
            });
            cx.notify();
        }))
}

/// 渲染日志表格行。
/// 独立成函数，使 cx 仅作参数借用而不被闭包捕获，避免与外层 state.read(cx) 的借用冲突。
fn render_rows(
    state: Entity<SyslogServerState>,
    cx: &mut Context<NetLiteApp>,
    rows: Vec<SyslogEntry>,
    selected_index: Option<usize>,
    show_detail: bool,
) -> Vec<AnyElement> {
    let theme = cx.theme().clone();
    rows.iter()
        .enumerate()
        .map(|(i, entry)| {
            let selected = selected_index == Some(i);
            let state_for_row = state.clone();
            let row = div()
                .flex()
                .items_center()
                .gap_2()
                .w_full()
                .h_6()
                .px_4()
                .bg(if selected {
                    theme.primary.opacity(0.12)
                } else {
                    theme.background
                })
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |_, _, _, cx| {
                        state_for_row.update(cx, |s, _| {
                            s.selected_index = Some(i);
                        });
                        cx.notify();
                    }),
                )
                .child(
                    div()
                        .w(px(150.0))
                        .min_w(px(116.0))
                        .text_xs()
                        .text_color(theme.muted_foreground.opacity(0.85))
                        .child(entry.received_at.clone()),
                )
                .child(
                    div()
                        .w(px(110.0))
                        .min_w(px(64.0))
                        .text_xs()
                        .text_color(theme.foreground)
                        .child(entry.source_ip.clone()),
                )
                .child(
                    div()
                        .w(px(52.0))
                        .min_w(px(40.0))
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(entry.source_port.clone()),
                )
                .child(
                    div()
                        .w(px(48.0))
                        .min_w(px(40.0))
                        .text_xs()
                        .font_medium()
                        .text_color(theme.muted_foreground)
                        .child(entry.protocol.clone()),
                )
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .text_xs()
                        .text_color(theme.foreground.opacity(0.9))
                        .truncate()
                        .child(raw_preview(&entry.raw_message)),
                );
            // 选中但未查看详情时，在行尾追加”详情”按钮。
            // flex_shrink_1 + min_w：窗口变窄时按钮随行收缩（跟随窗口缩放），但始终保留、不被挤压消失。
            if selected && !show_detail {
                row.child(
                    div()
                        .flex_shrink_1()
                        .min_w(px(56.0))
                        .child(detail_button(state.clone(), cx)),
                )
                .into_any_element()
            } else {
                row.into_any_element()
            }
        })
        .collect()
}

/// 表格表头
fn header_row(cx: &mut Context<NetLiteApp>) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .gap_2()
        .w_full()
        .h_6()
        .px_4()
        .bg(cx.theme().muted_foreground.opacity(0.08))
        .border_b_1()
        .border_color(cx.theme().border)
        .child(div().w(px(160.0)).min_w(px(120.0)).text_xs().text_color(cx.theme().muted_foreground).child("时间"))
        .child(div().w(px(120.0)).min_w(px(70.0)).text_xs().text_color(cx.theme().muted_foreground).child("来源IP"))
        .child(div().w(px(70.0)).min_w(px(50.0)).text_xs().text_color(cx.theme().muted_foreground).child("来源端口"))
        .child(div().w(px(60.0)).text_xs().text_color(cx.theme().muted_foreground).child("协议"))
        .child(div().min_w_0().flex_1().text_xs().text_color(cx.theme().muted_foreground).child("原始信息"))
}

/// 详情行（标签 + 值）
fn detail_row(label: &str, value: &str, cx: &mut Context<NetLiteApp>) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap_0p5()
        .min_w_0()
        .child(div().text_xs().text_color(cx.theme().muted_foreground).child(label.to_string()))
        .child(
            div()
                .min_w_0()
                .text_sm()
                .font_medium()
                .text_color(cx.theme().foreground)
                .truncate()
                .child(value.to_string()),
        )
}

// ---------------------------------------------------------------------------
// 导出（TXT / CSV / JSON）
// ---------------------------------------------------------------------------

/// 导出格式
#[derive(Clone, Copy, PartialEq, Eq)]
enum ExportFormat {
    Txt,
    Csv,
    Json,
}

/// 导出按钮
fn export_button(label: &str, fmt: ExportFormat, cx: &mut Context<NetLiteApp>) -> impl IntoElement {
    let theme = cx.theme().clone();
    let ext = match fmt {
        ExportFormat::Txt => "txt",
        ExportFormat::Csv => "csv",
        ExportFormat::Json => "json",
    };
    small_button(cx, label, theme.border, theme.foreground, true, move |app: &mut NetLiteApp, _, _, cx| {
        // 读取当前可见（已按搜索过滤）的记录
        let entries: Vec<SyslogEntry> = {
            let s = app.syslog_server_state.read(cx);
            let query = app.syslog_search_input.read_with(cx, |i, _| i.text().to_string());
            s.visible_rows(&query).into_iter().cloned().collect()
        };
        if entries.is_empty() {
            return;
        }
        let file_name = format!("syslog.{}", ext);
        tokio::spawn(async move {
            if let Some(path) = rfd::AsyncFileDialog::new()
                .set_file_name(&file_name)
                .add_filter("纯文本文件", &["txt"])
                .add_filter("CSV 文件", &["csv"])
                .add_filter("JSON 文件", &["json"])
                .save_file()
                .await
            {
                let path = path.path().to_path_buf();
                let content = match fmt {
                    ExportFormat::Txt => export_txt(&entries),
                    ExportFormat::Csv => export_csv(&entries),
                    ExportFormat::Json => export_json(&entries),
                };
                let _ = std::fs::write(&path, content);
            }
        });
    })
}

/// 导出为 TXT
fn export_txt(entries: &[SyslogEntry]) -> String {
    let mut out = String::new();
    for e in entries {
        out.push_str(&format!(
            "[{}] {}:{} {} {}\n",
            e.received_at, e.source_ip, e.source_port, e.protocol, e.raw_message
        ));
    }
    out
}

/// 导出为 CSV（RFC4180 引号转义）
fn export_csv(entries: &[SyslogEntry]) -> String {
    let mut out = String::from("received_at,source_ip,source_port,protocol,raw_message\n");
    for e in entries {
        out.push_str(&format!(
            "{},{},{},{},{}\n",
            field(&e.received_at),
            field(&e.source_ip),
            field(&e.source_port),
            field(&e.protocol),
            field(&e.raw_message)
        ));
    }
    out
}

/// 导出为 JSON
fn export_json(entries: &[SyslogEntry]) -> String {
    let mut out = String::from("[\n");
    for (i, e) in entries.iter().enumerate() {
        out.push_str(&format!(
            "  {{\"received_at\":{},\"source_ip\":{},\"source_port\":{},\"protocol\":{},\"raw_message\":{}}}",
            jstr(&e.received_at),
            jstr(&e.source_ip),
            jstr(&e.source_port),
            jstr(&e.protocol),
            jstr(&e.raw_message)
        ));
        if i + 1 < entries.len() {
            out.push(',');
        }
        out.push('\n');
    }
    out.push_str("]\n");
    out
}

/// CSV 字段转义
fn field(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

/// JSON 字符串字面量转义
fn jstr(s: &str) -> String {
    let mut o = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

// ---------------------------------------------------------------------------
// 渲染辅助
// ---------------------------------------------------------------------------

/// 原始信息预览：压缩换行并截断，用于表格单行展示
fn raw_preview(raw: &str) -> String {
    let collapsed = raw.replace(['\n', '\r'], " ");
    if collapsed.chars().count() > RAW_PREVIEW_MAX {
        collapsed.chars().take(RAW_PREVIEW_MAX - 1).collect::<String>() + "…"
    } else {
        collapsed
    }
}

/// 构造一条 Syslog 记录（记录 NetLite 接收时间，不解析设备时间）
fn make_entry(source_ip: String, source_port: String, protocol: &'static str, raw: String) -> SyslogEntry {
    SyslogEntry {
        received_at: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        source_ip,
        source_port,
        protocol: protocol.to_string(),
        raw_message: raw,
    }
}

// ---------------------------------------------------------------------------
// 接收线程（后台运行）
// ---------------------------------------------------------------------------

/// 按协议分发到 UDP / TCP 接收逻辑
fn run_server(
    addr: SocketAddr,
    protocol: SyslogProtocol,
    recv: Arc<Mutex<Vec<SyslogEntry>>>,
    running: Arc<AtomicBool>,
    error: Arc<Mutex<Option<String>>>,
) {
    match protocol {
        SyslogProtocol::UDP => run_udp(addr, recv, running, error),
        SyslogProtocol::TCP => run_tcp(addr, recv, running, error),
    }
}

/// UDP 接收：绑定后循环 recv_from，将原始字节按条写入共享缓冲
fn run_udp(
    addr: SocketAddr,
    recv: Arc<Mutex<Vec<SyslogEntry>>>,
    running: Arc<AtomicBool>,
    error: Arc<Mutex<Option<String>>>,
) {
    let socket = match UdpSocket::bind(addr) {
        Ok(s) => s,
        Err(e) => {
            *error.lock().unwrap() = Some(format!("绑定端口失败: {}", e));
            running.store(false, Ordering::Relaxed);
            return;
        }
    };
    // 设置读超时，使循环能周期性检查 running 以响应停止
    let _ = socket.set_read_timeout(Some(Duration::from_millis(READ_TIMEOUT_MS)));

    let mut buf = [0u8; 65535];
    loop {
        if !running.load(Ordering::Relaxed) {
            break;
        }
        match socket.recv_from(&mut buf) {
            Ok((n, peer)) => {
                if !running.load(Ordering::Relaxed) {
                    break;
                }
                let raw = String::from_utf8_lossy(&buf[..n]).to_string();
                recv.lock().unwrap().push(make_entry(
                    peer.ip().to_string(),
                    peer.port().to_string(),
                    "UDP",
                    raw,
                ));
            }
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => continue,
            Err(e) => {
                *error.lock().unwrap() = Some(format!("接收错误: {}", e));
                running.store(false, Ordering::Relaxed);
                break;
            }
        }
    }
}

/// TCP 接收：监听并接受连接，每个连接一个线程按行读取原始日志
fn run_tcp(
    addr: SocketAddr,
    recv: Arc<Mutex<Vec<SyslogEntry>>>,
    running: Arc<AtomicBool>,
    error: Arc<Mutex<Option<String>>>,
) {
    let listener = match TcpListener::bind(addr) {
        Ok(l) => l,
        Err(e) => {
            *error.lock().unwrap() = Some(format!("绑定端口失败: {}", e));
            running.store(false, Ordering::Relaxed);
            return;
        }
    };

    // 设为非阻塞，使 accept() 在无连接时立即返回错误，从而周期性检查 running 标志
    let _ = listener.set_nonblocking(true);

    loop {
        if !running.load(Ordering::Relaxed) {
            break;
        }
        match listener.accept() {
            Ok((stream, peer)) => {
                if !running.load(Ordering::Relaxed) {
                    break;
                }
                let t_recv = recv.clone();
                let t_running = running.clone();
                thread::spawn(move || {
                    handle_tcp_connection(stream, peer, t_recv, t_running);
                });
            }
            // 无连接到达（超时），短暂休眠后重新检查 running
            Err(_) => {
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

/// 单个 TCP 连接的读取循环：按行拆分原始内容（去掉行尾换行，保留其余原样）
fn handle_tcp_connection(
    stream: TcpStream,
    peer: SocketAddr,
    recv: Arc<Mutex<Vec<SyslogEntry>>>,
    running: Arc<AtomicBool>,
) {
    let _ = stream
        .set_read_timeout(Some(Duration::from_millis(READ_TIMEOUT_MS)));
    let mut reader = std::io::BufReader::new(stream);
    let mut line = String::new();
    loop {
        if !running.load(Ordering::Relaxed) {
            break;
        }
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break, // 连接已关闭
            Ok(_) => {
                let raw = line.trim_end_matches(['\n', '\r']).to_string();
                if raw.is_empty() {
                    continue;
                }
                recv.lock().unwrap().push(make_entry(
                    peer.ip().to_string(),
                    peer.port().to_string(),
                    "TCP",
                    raw,
                ));
            }
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => continue,
            Err(_) => break,
        }
    }
}