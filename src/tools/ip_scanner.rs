use gpui::prelude::*;

/// 单次扫描最大 IP 数量限制
const MAX_SCAN_COUNT: usize = 256;
use gpui::*;
use gpui_component::scroll::ScrollableElement;
use gpui_component::StyledExt;
use gpui_component::input::Input;
use gpui_component::{ActiveTheme, Icon, IconName};
use gpui_component::tooltip::Tooltip;
use ipnet::Ipv4Net;
use log::debug;
use std::net::Ipv4Addr;
use std::thread;
use std::time::{Duration, Instant};

use crate::app::NetLiteApp;

/// IP 地址扫描工具
pub struct IpScannerTool<'a> {
    _app: &'a NetLiteApp,
    state: Entity<IpScannerState>,
}

/// IP 探测结果状态
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum IpStatus {
    /// 未检测
    Unknown,
    /// 扫描中
    Scanning,
    /// 正常
    Up,
    /// 不可达
    Down,
}

impl IpStatus {
    fn bg_color(self) -> Rgba {
        match self {
            IpStatus::Up => gpui::rgb(0x55AA7F),
            IpStatus::Down => gpui::rgb(0xFF8E77),
            IpStatus::Scanning => gpui::rgb(0xF5C542),
            IpStatus::Unknown => gpui::rgb(0xCBCBCB),
        }
    }
}

impl<'a> IpScannerTool<'a> {
    pub fn new(app: &'a NetLiteApp) -> Self {
        Self { _app: app, state: app.ip_scanner_state.clone() }
    }

    pub fn render(self, _window: &mut Window, cx: &mut Context<NetLiteApp>) -> AnyElement {
        let state = self.state.clone();

        div()
            .flex()
            .flex_col()
            .flex_1()
            .overflow_hidden()
            .bg(cx.theme().background)
            .child(Self::render_input_panel(state.clone(), self._app, cx))
                    .child(
                if !state.read(cx).results.is_empty() {
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .overflow_hidden()
                        .children(vec![
                            Self::render_stats_bar(state.clone(), cx).into_any_element(),
                            Self::render_ip_grid(state.clone(), _window, cx).into_any_element(),
                        ])
                } else {
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .justify_center()
                        .items_center()
                        .child(Icon::new(IconName::Search).size(px(48.0)).text_color(cx.theme().muted_foreground.opacity(0.5)))
                        .child(div().text_lg().font_medium().text_color(cx.theme().muted_foreground.opacity(0.6)).child("输入 IP 网段后点击扫描"))
                },
            )
            .into_any_element()
    }

    /// 校验输入格式，返回错误信息
    fn validate_input(query: &str) -> Option<String> {
        let query = query.trim();
        if query.is_empty() {
            return None; // 空输入不报错
        }
        // 检查是否合法：CIDR / 单 IP / IP 范围
        if query.contains('/') {
            // CIDR 格式
            if let Ok(net) = query.parse::<Ipv4Net>() {
                let network = u32::from(net.network());
                let broadcast = u32::from(net.broadcast());
                let count = (broadcast - network + 1) as usize - 2; // 减去网络地址和广播地址
                if count < 1 || count > MAX_SCAN_COUNT {
                    return Some(if count > MAX_SCAN_COUNT {
                        format!("扫描范围过大，最多支持 {} 个 IP", MAX_SCAN_COUNT)
                    } else {
                        "该网段无可用主机地址".to_string()
                    });
                }
            } else {
                return Some("IP 网段格式错误，如 192.168.1.0/24".to_string());
            }
        } else if query.contains('-') {
            // IP 范围格式
            if let Some((left, right)) = query.split_once('-') {
                if let (Ok(start), Ok(end)) = (left.trim().parse::<Ipv4Addr>(), right.trim().parse::<Ipv4Addr>()) {
                    if start.octets()[0] != end.octets()[0]
                        || start.octets()[1] != end.octets()[1]
                        || start.octets()[2] != end.octets()[2]
                    {
                        return Some("IP 范围必须在同一 /24 网段内".to_string());
                    }
                    if start.octets()[3] == 0 || start.octets()[3] == 255
                        || end.octets()[3] == 0 || end.octets()[3] == 255
                    {
                        return Some("网络地址（.0）和广播地址（.255）不可扫描".to_string());
                    }
                    if start > end {
                        return Some("起始 IP 不能大于结束 IP".to_string());
                    }
                    let count = u32::from(end) - u32::from(start) + 1;
                    if count as usize > MAX_SCAN_COUNT {
                        return Some(format!("扫描范围过大，最多支持 {} 个 IP", MAX_SCAN_COUNT));
                    }
                } else {
                    return Some("IP 范围格式错误，如 192.168.1.1-100".to_string());
                }
            } else {
                return Some("IP 范围格式错误，如 192.168.1.1-100".to_string());
            }
        } else {
            // 单 IP 格式
            if let Ok(addr) = query.parse::<Ipv4Addr>() {
                if addr.octets()[3] == 0 || addr.octets()[3] == 255 {
                    return Some("网络地址（.0）和广播地址（.255）不可扫描".to_string());
                }
            } else {
                return Some("IP 地址格式错误，如 192.168.1.1".to_string());
            }
        }
        None // 格式正确
    }

    /// 渲染输入面板
    fn render_input_panel(state: Entity<IpScannerState>, app: &NetLiteApp, cx: &mut Context<NetLiteApp>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let is_scanning = state.read(cx).is_scanning;

        // 在渲染阶段实时读取输入并校验（自动处理输入/删除/粘贴等所有场景）
        let mut input_error: Option<String> = None;
        app.ip_scanner_input.read_with(cx, |input, _| {
            let query = input.text().to_string();
            input_error = Self::validate_input(&query);
        });
        // 持久化校验结果，供扫描按钮 handler 等其他地方读取
        if let Some(ref err) = input_error {
            if state.read(cx).input_error.as_ref() != Some(err) {
                state.update(cx, |s, _| s.input_error = Some(err.clone()));
            }
        } else if state.read(cx).input_error.is_some() {
            state.update(cx, |s, _| s.input_error = None);
        }

        let input_error = input_error;

        div()
            .flex()
            .flex_col()
            .px_6()
            .py_3()
            .border_b_1()
            .border_color(theme.border)
            .bg(theme.secondary)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .flex_1()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .flex_1()
                            .h_9()
                            .bg(theme.background)
                            .border_1()
                            .border_color(input_error.as_ref().map(|_| theme.red).unwrap_or(theme.border))
                            .rounded_md()
                            .relative()
                            .child(Input::new(&app.ip_scanner_input).w_0().flex_1().appearance(false).h_9())
                            .children(
                                app.ip_scanner_input.read_with(cx, |input, _| input.text().len() == 0)
                                    .then(|| {
                                        div().absolute().left_2().top_0().h_9()
                                            .flex().items_center()
                                            .text_sm().text_color(theme.muted_foreground.opacity(0.5))
                                            .child("输入 IP 网段，如 192.168.1.0/24、192.168.1.1-100 或 192.168.1.1")
                                    })
                            ),
                    )
                    .child(
                        div()
                            .px_4()
                            .py_1p5()
                            .rounded_md()
                            .bg(if is_scanning { theme.border } else { theme.primary })
                            .text_sm()
                            .font_medium()
                            .opacity(if input_error.is_some() || is_scanning { 0.5 } else { 1.0 })
                            .text_color(if is_scanning {
                                theme.muted_foreground
                            } else {
                                theme.background
                            })
                            .child(
                                div()
                                    .text_sm()
                                    .font_medium()
                                    .text_color(theme.background)
                                    .child(if is_scanning { "扫描中…" } else { "扫描" }),
                            )
                            .on_mouse_down(MouseButton::Left, cx.listener(
                                move |app: &mut NetLiteApp, _event: &MouseDownEvent, _window: &mut Window, cx: &mut Context<NetLiteApp>| {
                                    let already_scanning = state.read(cx).is_scanning;
                                    let has_error = state.read(cx).input_error.is_some();
                                    if already_scanning || has_error {
                                        return;
                                    }

                                    let input = app.ip_scanner_input.clone();
                                    let state = state.clone();

                                    let mut query = String::new();
                                    input.read_with(cx, |input, _app| {
                                        query = input.text().to_string();
                                    });

                                    // 解析查询（支持 CIDR / 单 IP / IP 范围）
                                    let scan_query = match parse_scan_query(&query) {
                                        Some(q) => q,
                                        None => return,
                                    };

                                    // 先批量渲染小方块（Unknown 状态）
                                    state.update(cx, |state, cx| {
                                        state.network_prefix = scan_query.prefix.clone();
                                        state.results = vec![IpStatus::Unknown; scan_query.count];
                                        state.latencies = vec![None; scan_query.count];
                                        state.has_scanned = false;
                                        cx.notify();
                                    });
                                    cx.notify();

                                    // 再标记为扫描中，触发重绘显示黄色
                                    state.update(cx, |state, cx| {
                                        for i in 0..scan_query.count {
                                            state.results[i] = IpStatus::Scanning;
                                        }
                                        state.is_scanning = true;
                                        cx.notify();
                                    });
                                    cx.notify();

                                    // 在后台线程扫描，通过共享状态实时发送每个 IP 的探测结果
                                    let scan_query_clone = scan_query.clone();
                                    let count = scan_query.count;

                                    // 使用 Arc<Mutex<Vec>> 作为共享结果缓冲区
                                    let results_shared = std::sync::Arc::new(std::sync::Mutex::new(
                                        vec![(None, None); count]
                                    ));
                                    let latencies_shared = std::sync::Arc::new(std::sync::Mutex::new(
                                        vec![None; count]
                                    ));
                                    let completed = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
                                    let total = count;

                                    // 启动扫描线程
                                    let results_shared_clone = results_shared.clone();
                                    let latencies_shared_clone = latencies_shared.clone();
                                    let completed_clone = completed.clone();
                                    thread::spawn(move || {
                                        scan_network_realtime_into(
                                            scan_query_clone,
                                            results_shared_clone,
                                            latencies_shared_clone,
                                            completed_clone,
                                            total,
                                        );
                                    });

                                    // 轮询共享结果并更新 UI（小方块实时变色）
                                    let state = state.clone();
                                    cx.spawn(async move |_this, cx| {
                                        loop {
                                            let done = completed.load(std::sync::atomic::Ordering::SeqCst);
                                            if done >= total {
                                                break;
                                            }
                                            // 读取已完成的结果，逐个更新方块颜色
                                            {
                                                let r_lock = results_shared.lock().unwrap();
                                                let l_lock = latencies_shared.lock().unwrap();
                                                for i in 0..total {
                                                    let (status, _) = &r_lock[i];
                                                    let lat = &l_lock[i];
                                                    if let Some(status) = status {
                                                        state.update(cx, |state, cx| {
                                                            if i < state.results.len() {
                                                                state.results[i] = *status;
                                                                state.latencies[i] = lat.clone();
                                                                cx.notify();
                                                            }
                                                        });
                                                    }
                                                }
                                            }
                                            smol::Timer::after(std::time::Duration::from_millis(100)).await;
                                        }
                                        // 读取最终结果（确保没有遗漏）
                                        {
                                            let r_lock = results_shared.lock().unwrap();
                                            let l_lock = latencies_shared.lock().unwrap();
                                            for i in 0..total {
                                                if let Some(status) = r_lock[i].0 {
                                                    state.update(cx, |state, cx| {
                                                        if i < state.results.len() {
                                                            state.results[i] = status;
                                                            state.latencies[i] = l_lock[i];
                                                            cx.notify();
                                                        }
                                                    });
                                                }
                                            }
                                        }
                                        state.update(cx, |state, cx| {
                                            state.is_scanning = false;
                                            state.has_scanned = true;
                                            cx.notify();
                                        });
                                    })
                                    .detach();
                                },
                            ))
                    )
            )
            .children(input_error.as_ref().map(|err| {
                let err = err.clone();
                div()
                    .text_xs()
                    .text_color(theme.red)
                    .px_6()
                    .pb_2()
                    .child(err)
            }))
    }

    /// 渲染统计栏
    fn render_stats_bar(
        state: Entity<IpScannerState>,
        cx: &mut Context<NetLiteApp>,
    ) -> impl IntoElement {
        let s = state.read(cx);
        let counts = s.compute_counts();

        div()
            .flex()
            .items_center()
            .gap_4()
            .px_6()
            .py_2()
            .border_b_1()
            .border_color(gpui::rgb(0x333333))
            .child(
                div().flex().gap_1().child(div().w_2p5().h_2p5().rounded_full().bg(gpui::rgb(0x55AA7F))).child(div().text_xs().text_color(gpui::rgb(0x55AA7F)).child(format!("{} 正常", counts.0)))
            )
            .child(
                div().flex().gap_1().child(div().w_2p5().h_2p5().rounded_full().bg(gpui::rgb(0xFF8E77))).child(div().text_xs().text_color(gpui::rgb(0xFF8E77)).child(format!("{} 不可达", counts.1)))
            )
            .child(
                div().flex().gap_1().child(div().w_2p5().h_2p5().rounded_full().bg(gpui::rgb(0xCBCBCB))).child(div().text_xs().text_color(gpui::rgb(0x999999)).child(format!("{} 未检测", counts.2)))
            )
    }

    /// 渲染 IP 网格
    fn render_ip_grid(
        state: Entity<IpScannerState>,
        _window: &mut Window,
        cx: &mut Context<NetLiteApp>,
    ) -> impl IntoElement {
        let s = state.read(cx);
        let network_prefix = s.network_prefix.clone();
        let count = s.results.len();
        let latencies = s.latencies.clone();

        let mut children = Vec::with_capacity(count);
        for idx in 0..count {
            let prefix = network_prefix.clone();
            let status = s.results[idx];
            let bg = status.bg_color();
            let ip_suffix = idx + 1;
            let lat = latencies.clone();

            children.push(
                div()
                    .id(gpui::ElementId::Integer(idx as u64))
                    .w_12()
                    .h_12()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_sm()
                    .bg(bg)
                    .child(
                        div()
                            .text_xs()
                            .font_medium()
                            .text_color(if status == IpStatus::Scanning {
                                gpui::rgb(0x333333)
                            } else {
                                gpui::rgb(0xffffff).opacity(0.9)
                            })
                            .child(format!("{}", ip_suffix)),
                    )
                    .hover(|style| {
                        style
                            .border_color(gpui::rgb(0xffffff).opacity(0.3))
                            .border_1()
                            .rounded_sm()
                    })
                    .tooltip(move |window, cx| {
                        let full_ip = format!("{}.{}", prefix, ip_suffix);
                        let status_text = match status {
                            IpStatus::Up => "● 正常",
                            IpStatus::Down => "● 不可达",
                            IpStatus::Scanning => "● 扫描中",
                            IpStatus::Unknown => "● 未检测",
                        };
                        let latency = lat.get(idx).and_then(|d| {
                            d.map(|d| format!(" 响应时间 {} ms", d.as_millis()))
                        }).unwrap_or_default();
                        Tooltip::new(format!("{}\n{}{}", full_ip, status_text, latency)).build(window, cx)
                    }),
            );
        }

        div()
            .overflow_y_scrollbar()
            .flex()
            .flex_col()
            .h_0()
            .flex_1()
            .px_6()
            .py_4()
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .justify_start()
                    .min_h_0()
                    .children(children),
            )
    }
}

/// 解析后的扫描查询结果
#[derive(Clone)]
struct ScanQuery {
    /// 网段前缀，如 "192.168.1"
    prefix: String,
    /// IP 范围（包含两端）
    first_ip: Ipv4Addr,
    last_ip: Ipv4Addr,
    /// 总数
    count: usize,
}

/// 从查询字符串解析为扫描目标（支持 CIDR、单 IP、IP 范围）
/// - CIDR: `192.168.1.0/24` → 解析为 /24 网段
/// - 单 IP: `192.168.1.1` → 自动补全为 /24 网段
/// - IP 范围: `192.168.1.1-192.168.1.100` → 解析为指定范围
fn parse_scan_query(query: &str) -> Option<ScanQuery> {
    let query = query.trim();
    if query.is_empty() {
        return None;
    }

    // 1. CIDR 格式: 192.168.1.0/24
    if let Ok(net) = query.parse::<Ipv4Net>() {
        let network = net.network();
        let broadcast = net.broadcast();
        let first = Ipv4Addr::new(network.octets()[0], network.octets()[1], network.octets()[2], network.octets()[3].saturating_add(1));
        let last = Ipv4Addr::new(broadcast.octets()[0], broadcast.octets()[1], broadcast.octets()[2], broadcast.octets()[3].saturating_sub(1));
        if first <= last {
            let count = (u32::from(last) - u32::from(first) + 1) as usize;
            if count > MAX_SCAN_COUNT {
                return None;
            }
            return Some(ScanQuery {
                prefix: format!("{}.{}.{}", network.octets()[0], network.octets()[1], network.octets()[2]),
                first_ip: first,
                last_ip: last,
                count,
            });
        }
        return None;
    }

    // 2. IP 范围格式: 192.168.1.1-192.168.1.100
    if let Some((left, right)) = query.split_once('-') {
        let left = left.trim();
        let right = right.trim();
        if let (Ok(start), Ok(end)) = (left.parse::<Ipv4Addr>(), right.parse::<Ipv4Addr>()) {
            // 只支持同三段（/24 内）的范围
            if start.octets()[0] == end.octets()[0]
                && start.octets()[1] == end.octets()[1]
                && start.octets()[2] == end.octets()[2]
                && start <= end
                && start.octets()[3] != 0 && start.octets()[3] != 255
                && end.octets()[3] != 0 && end.octets()[3] != 255
            {
                let count = (u32::from(end) - u32::from(start) + 1) as usize;
                if count > MAX_SCAN_COUNT {
                    return None;
                }
                let prefix = format!("{}.{}.{}", start.octets()[0], start.octets()[1], start.octets()[2]);
                return Some(ScanQuery {
                    prefix,
                    first_ip: start,
                    last_ip: end,
                    count,
                });
            }
        }
    }

    // 4. 单 IP 格式: 192.168.1.1 → 只扫描该地址（排除 .0 和 .255）
    if let Ok(addr) = query.parse::<Ipv4Addr>() {
        if addr.octets()[3] != 0 && addr.octets()[3] != 255 {
            return Some(ScanQuery {
                prefix: format!("{}.{}.{}", addr.octets()[0], addr.octets()[1], addr.octets()[2]),
                first_ip: addr,
                last_ip: addr,
                count: 1,
            });
        }
    }

    None
}

/// 扫描状态（纯数据，不含生命周期引用）
pub struct IpScannerState {
    /// 网络前缀 (如 "192.168.1")，用于显示完整 IP
    network_prefix: String,
    /// 是否正在扫描
    is_scanning: bool,
    /// 是否已完成过扫描
    has_scanned: bool,
    /// IP 状态列表 (索引 = 最后一段 octet)
    results: Vec<IpStatus>,
    /// IP 响应时间列表，仅正常时记录
    latencies: Vec<Option<Duration>>,
    /// 输入格式错误提示，Some(错误信息) 时输入框变红
    input_error: Option<String>,
}

impl IpScannerState {
    pub fn new(_cx: &mut Context<Self>) -> Self {
        Self {
            network_prefix: String::new(),
            is_scanning: false,
            has_scanned: false,
            results: vec![],
            latencies: vec![],
            input_error: None,
        }
    }

    fn compute_counts(&self) -> (usize, usize, usize) {
        let mut up = 0usize;
        let mut down = 0usize;
        let mut unknown = 0usize;
        for &status in &self.results {
            match status {
                IpStatus::Up => up += 1,
                IpStatus::Down => down += 1,
                IpStatus::Unknown => unknown += 1,
                IpStatus::Scanning => {}
            }
        }
        (up, down, unknown)
    }
}

// ---- 探测方法选择 ----

/// 检查目标 IP 是否与本机同网段（同一子网）
/// 如果掩码无效（如 0.0.0.0），统一返回 false，回退使用 ping
fn is_same_subnet(local_ip: Ipv4Addr, local_mask: Ipv4Addr, target: Ipv4Addr) -> bool {
    // 掩码无效时所有地址都被误判为同网段，回退到 ping
    if local_mask == Ipv4Addr::UNSPECIFIED || local_mask == Ipv4Addr::BROADCAST {
        return false;
    }
    let local_net = (u32::from(local_ip) & u32::from(local_mask)) as u32;
    let target_net = (u32::from(target) & u32::from(local_mask)) as u32;
    local_net == target_net
}

/// 从网络接口列表中找到最合适的本机 IP 和子网掩码
/// Windows: 使用 GetAdaptersInfo（内置 API，无需 WinPcap/Npcap）
/// 其他平台: 回退到简单实现
fn get_local_ip_and_mask() -> Option<(Ipv4Addr, Ipv4Addr)> {
    #[cfg(target_os = "windows")]
    {
        use std::ptr;

        // GetAdaptersInfo 先获取所需缓冲区大小
        let mut buf_size: u32 = 0;
        unsafe {
            winapi::um::iphlpapi::GetAdaptersInfo(ptr::null_mut(), &mut buf_size);
        }

        let mut buf: Vec<u8> = vec![0; buf_size as usize];
        let result = unsafe {
            winapi::um::iphlpapi::GetAdaptersInfo(
                buf.as_mut_ptr() as *mut _,
                &mut buf_size,
            )
        };

        if result != winapi::shared::winerror::ERROR_SUCCESS {
            debug!("IP扫描: GetAdaptersInfo 失败，错误码 {}", result);
            return None;
        }

        let mut best: Option<(Ipv4Addr, u32)> = None;

        // SAFETY: GetAdaptersInfo succeeded, buffer is valid
        unsafe {
            let mut curr: *mut winapi::um::iptypes::IP_ADAPTER_INFO = buf.as_ptr() as *mut _;
            while !curr.is_null() {
                let adapter = &*curr;

                // 跳过 loopback 适配器（Type = 24 = MIB_IF_TYPE_LOOPBACK）
                if adapter.Type == 24 {
                    curr = adapter.Next;
                    continue;
                }

                // 遍历 IP 地址链表
                let mut addr_curr: *mut winapi::um::iptypes::IP_ADDR_STRING = ptr::addr_of!(adapter.IpAddressList) as *mut _;
                while !addr_curr.is_null() {
                    let entry = &*addr_curr;
                    // IpAddress.String 是 CHAR[16]，如 "192.168.1.100"
                    let ip_str = cstr_to_str(&entry.IpAddress.String);
                    let mask_str = cstr_to_str(&entry.IpMask.String);

                    if let (Ok(ip), Ok(mask)) = (ip_str.parse::<Ipv4Addr>(), mask_str.parse::<Ipv4Addr>()) {
                                if !ip.is_loopback() && !ip.is_unspecified() {
                                    let pfx = mask_prefix_to_u8(mask);
                                    if pfx > 0 && pfx < 32 {
                                        let current_best = best.as_ref().map(|(_, p)| *p).unwrap_or(0);
                                        if u32::from(pfx) > current_best {
                                            best = Some((ip, u32::from(pfx)));
                                        }
                                    }
                                }
                            }

                    addr_curr = entry.Next;
                }

                curr = adapter.Next;
            }
        }

        best.map(|(ip, prefix)| {
            let mask = match prefix {
                0 => Ipv4Addr::UNSPECIFIED,
                32 => Ipv4Addr::new(255, 255, 255, 255),
                _ => Ipv4Addr::new(
                    ((0xFFFFFFFFu32 << (32 - prefix)) >> 24) as u8,
                    ((0xFFFFFFFFu32 << (32 - prefix)) >> 16 & 0xFF) as u8,
                    ((0xFFFFFFFFu32 << (32 - prefix)) >> 8 & 0xFF) as u8,
                    ((0xFFFFFFFFu32 << (32 - prefix)) & 0xFF) as u8,
                ),
            };
            debug!("IP扫描: 本机 IP={}, 掩码={:?}", ip, mask);
            (ip, mask)
        })
    }

    #[cfg(not(target_os = "windows"))]
    {
        // 非 Windows 平台：回退到简单实现
        use std::net::UdpSocket;
        let udp = UdpSocket::bind("0.0.0.0:0").ok()?;
        let _ = udp.connect(("8.8.8.8", 53));
        let local_addr = udp.local_addr().ok()?;
        let local_ip = match local_addr {
            std::net::SocketAddr::V4(addr) => addr.ip(),
            _ => return None,
        };
        let mask = Ipv4Addr::new(255, 255, 255, 0);
        debug!("IP扫描: 本机 IP={}（假设 /24）", local_ip);
        Some((local_ip, mask))
    }
}

/// 将 C 字符串（CHAR 数组）转为 Rust &str
#[cfg(target_os = "windows")]
fn cstr_to_str(buf: &[std::os::raw::c_char; 16]) -> &str {
    let len = buf.iter().position(|&c| c == 0).unwrap_or(16);
    // SAFETY: Windows API fills these buffers with valid UTF-8 ASCII strings like "192.168.1.100"
    unsafe { std::str::from_utf8_unchecked(std::slice::from_raw_parts(buf.as_ptr() as *const u8, len)) }
}

/// 将子网掩码 Ipv4Addr 转为前缀长度 u8
fn mask_prefix_to_u8(mask: Ipv4Addr) -> u8 {
    let val = u32::from(mask);
    val.leading_zeros() as u8
}

/// 探测单个 IP（智能选择 ARP 或 ping）
fn probe_ip(ip: Ipv4Addr, local_ip: Option<Ipv4Addr>, local_mask: Option<Ipv4Addr>) -> Result<Duration, anyhow::Error> {
    // 如果提供了本机 IP 和掩码，判断是否同网段
    let (local_ip, local_mask) = match (local_ip, local_mask) {
        (Some(lip), Some(lmsk)) => (lip, lmsk),
        _ => {
            // 没有本地接口信息，直接用 ping
            return probe_ip_ping(ip);
        }
    };

    if is_same_subnet(local_ip, local_mask, ip) {
        // 同网段 → 使用 ARP
        probe_ip_arp(ip)
    } else {
        // 跨网段 → 使用 ping
        probe_ip_ping(ip)
    }
}

// ---- 同网段 ARP 探测（SendARP，无需管理员权限） ----

/// 使用 SendARP 探测单个 IP（Windows 专用，仅同网段）
/// 返回 Ok(duration) 表示可达，Err 表示不可达
#[cfg(target_os = "windows")]
fn probe_ip_arp(ip: Ipv4Addr) -> Result<Duration, anyhow::Error> {
    let start = Instant::now();

    // SendARP 签名:
    // DWORD SendARP(IPAddr DestIP, IPAddr SrcIP, PVOID pMacAddr, PULONG PhyAddrLen)
    // DestIP: 目标 IP (网络字节序 u32)
    // SrcIP:  源 IP，0 = 自动选择出口接口 (网络字节序 u32)
    // pMacAddr: 输出缓冲区，至少 8 字节，存放 MAC 地址
    // PhyAddrLen: 输入时指定缓冲区大小，输出时写入实际长度
    const MAC_BUF_SIZE: usize = 8;
    let mut mac_buf: [u8; MAC_BUF_SIZE] = [0; MAC_BUF_SIZE];
    let mut mac_len = MAC_BUF_SIZE as u32;

    // 将 IPv4 地址转为网络字节序 u32（大端序）
    let dest_ip_u32 = ip.octets()[0] as u32
        | (ip.octets()[1] as u32) << 8
        | (ip.octets()[2] as u32) << 16
        | (ip.octets()[3] as u32) << 24;
    // SrcIP = 0 表示让系统自动选择出口接口
    let src_ip_u32 = 0;

    let result = unsafe {
        winapi::um::iphlpapi::SendARP(
            dest_ip_u32,
            src_ip_u32,
            mac_buf.as_mut_ptr() as *mut winapi::ctypes::c_void,
            &mut mac_len,
        )
    };

    let elapsed = start.elapsed();

    // ERROR_SUCCESS = 0
    if result == 0 {
        debug!("IP扫描: {} 可达 (ARP, MAC={:02X}-{:02X}-{:02X}-{:02X}-{:02X}-{:02X})",
            ip, mac_buf[0], mac_buf[1], mac_buf[2], mac_buf[3], mac_buf[4], mac_buf[5]);
        Ok(elapsed)
    } else {
        debug!("IP扫描: {} 不可达 (SendARP 返回 {})", ip, result);
        Err(anyhow::anyhow!("SendARP failed with code {}", result))
    }
}

/// Linux/macOS 下的 ARP 探测（ping 回退）
#[cfg(not(target_os = "windows"))]
fn probe_ip_arp(ip: Ipv4Addr) -> Result<Duration, anyhow::Error> {
    probe_ip_ping(ip)
}

/// 实时扫描（共享状态版本）：每个 IP 探测完成后写入共享 Vec
fn scan_network_realtime_into(
    scan_query: ScanQuery,
    results_shared: std::sync::Arc<std::sync::Mutex<Vec<(Option<IpStatus>, Option<Duration>)>>>,
    latencies_shared: std::sync::Arc<std::sync::Mutex<Vec<Option<Duration>>>>,
    completed: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    _total: usize,
) {
    let ScanQuery {
        prefix,
        first_ip,
        last_ip,
        count: _count,
    } = scan_query;

    let parts: Vec<u8> = prefix
        .split('.')
        .filter_map(|s| s.parse().ok())
        .collect();
    if parts.len() != 3 {
        return;
    }

    let first = first_ip.octets()[3];
    let last = last_ip.octets()[3];

    let (local_ip, local_mask) = get_local_ip_and_mask().unwrap_or((Ipv4Addr::UNSPECIFIED, Ipv4Addr::UNSPECIFIED));

    let concurrency = 64;
    let mut handles = Vec::new();

    for octet in first..=last {
        let target_ip = Ipv4Addr::new(parts[0], parts[1], parts[2], octet);
        let idx = (octet - first) as usize;

        let probe_method = if local_ip != Ipv4Addr::UNSPECIFIED
            && is_same_subnet(local_ip, local_mask, target_ip)
        {
            "arp"
        } else {
            "ping"
        };

        let local_ip = local_ip;
        let local_mask = local_mask;
        let results_shared = results_shared.clone();
        let latencies_shared = latencies_shared.clone();
        let completed = completed.clone();

        handles.push(thread::spawn(move || {
            debug!("IP扫描: {} ({}) 开始探测", target_ip, probe_method);
            let result = probe_ip(target_ip, Some(local_ip), Some(local_mask));
            let (status, latency) = match result {
                Ok(dur) => {
                    debug!("IP扫描: {} 可达 (ARP, MAC={:02X}-{:02X}-{:02X}-{:02X}-{:02X}-{:02X})",
                        target_ip, 0, 0, 0, 0, 0, 0);
                    (IpStatus::Up, Some(dur))
                }
                Err(e) => {
                    debug!("IP扫描: {} 不可达 ({})", target_ip, e);
                    (IpStatus::Down, None)
                }
            };
            // 写入共享结果
            {
                let mut r = results_shared.lock().unwrap();
                r[idx] = (Some(status), latency);
            }
            {
                let mut l = latencies_shared.lock().unwrap();
                l[idx] = latency;
            }
            completed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            (idx, status, latency)
        }));

        if handles.len() >= concurrency {
            for handle in handles.drain(..) {
                let _ = handle.join();
            }
        }
    }

    for handle in handles {
        let _ = handle.join();
    }
}

/// 探测单个 IP 是否可达（阻塞式，超时 3000ms）
/// 使用系统 ping 命令，通过解析输出来判断
fn probe_ip_ping(ip: Ipv4Addr) -> Result<Duration, anyhow::Error> {
    let start = std::time::Instant::now();

    // 判断当前系统，执行对应的 ping 命令
    let output = if cfg!(target_os = "windows") {
        std::process::Command::new("ping")
            .args(["-n", "1", "-w", "3000"])
            .arg(ip.to_string())
            .output()
    } else {
        std::process::Command::new("ping")
            .args(["-c", "1", "-W", "3"])
            .arg(ip.to_string())
            .output()
    };

    match output {
        Ok(out) => {
            // 解析 ping 输出，查找 "TTL=" 或 "time=" 关键字
            let stdout = String::from_utf8_lossy(&out.stdout);
            let has_response = if cfg!(target_os = "windows") {
                // Windows: "TTL=" 表示成功响应
                stdout.contains("TTL=")
            } else {
                // Linux: "time=" 或 "rtt" 表示成功响应
                stdout.contains("time=") || stdout.contains("rtt")
            };

            if has_response {
                Ok(start.elapsed())
            } else {
                Err(anyhow::anyhow!("{}: unreachable", ip))
            }
        }
        Err(e) => Err(anyhow::anyhow!("{}: {} (ping command failed)", ip, e)),
    }
}
