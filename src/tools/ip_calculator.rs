use gpui::prelude::*;

use gpui::*;
use gpui_component::scroll::ScrollableElement;
use gpui_component::StyledExt;
use gpui_component::input::Input;
use gpui_component::{ActiveTheme, Icon};
use gpui_component::clipboard::Clipboard;
use crate::custom_icons::CustomIconName;
use ipnet::{Ipv4Net, Ipv6Net};
use std::net::{Ipv4Addr, Ipv6Addr};

use crate::app::NetLiteApp;

/// IP 地址计算器单次计算最大输入长度
const MAX_INPUT_LEN: usize = 128;

/// IP 版本
#[derive(Clone, Copy, PartialEq, Eq)]
enum IpVersion {
    IPv4,
    IPv6,
}

/// IPv6 地址类型
#[derive(Clone)]
enum Ipv6AddrType {
    /// 全球单播地址 2000::/3
    GlobalUnicast,
    /// 唯一本地地址 fc00::/7 (ULA)
    Ula,
    /// 链路本地地址 fe80::/10
    LinkLocal,
    /// 环回地址 ::1/128
    Loopback,
    /// 组播地址 ff00::/8
    Multicast,
    /// 未指定地址 ::/128
    Unspecified,
    /// 其他
    Other,
}

impl Ipv6AddrType {
    fn label(&self) -> &'static str {
        match self {
            Ipv6AddrType::GlobalUnicast => "全球单播 (GUA)",
            Ipv6AddrType::Ula => "唯一本地 (ULA)",
            Ipv6AddrType::LinkLocal => "链路本地 (LLA)",
            Ipv6AddrType::Loopback => "环回地址",
            Ipv6AddrType::Multicast => "组播 (Mcast)",
            Ipv6AddrType::Unspecified => "未指定地址",
            Ipv6AddrType::Other => "其他",
        }
    }

    fn is_private(&self) -> bool {
        matches!(self, Ipv6AddrType::Ula | Ipv6AddrType::LinkLocal | Ipv6AddrType::Loopback)
    }

}

/// 判断 IPv6 地址类型
fn classify_ipv6(addr: &Ipv6Addr) -> Ipv6AddrType {
    let bytes = addr.octets();
    // 检查未指定地址
    if addr.is_unspecified() {
        return Ipv6AddrType::Unspecified;
    }
    // 检查环回地址
    if addr.is_loopback() {
        return Ipv6AddrType::Loopback;
    }
    // 检查组播地址 ff00::/8
    if bytes[0] == 0xFF {
        return Ipv6AddrType::Multicast;
    }
    // 检查链路本地 fe80::/10
    if (bytes[0] == 0xFE) && ((bytes[1] & 0xC0) == 0x80) {
        return Ipv6AddrType::LinkLocal;
    }
    // 检查唯一本地地址 fc00::/7
    if (bytes[0] & 0xFE) == 0xFC {
        return Ipv6AddrType::Ula;
    }
    // 检查全球单播 2000::/3
    if (bytes[0] & 0xE0) == 0x20 {
        return Ipv6AddrType::GlobalUnicast;
    }
    Ipv6AddrType::Other
}

/// 计算结果（所有字段 Owned，避免生命周期问题）
#[derive(Clone)]
struct CalcResult {
    version: IpVersion,
    /// 网络地址（输入地址与掩码运算后的网络地址）
    ip: String,
    is_private: bool,
    network_addr: String,
    subnet_mask: String,
    prefix_len: String,
    total_ips: String,
    usable_ips: String,
    first_usable_range: String,
    broadcast: String,
    /// 所有信息用于复制的纯文本
    all_ranges_text: String,
    /// IPv6 地址类型标签（IPv4 为空）
    ipv6_addr_type: Option<String>,
    /// IPv6 网络位/接口ID分界
    ipv6_network_bits: Option<String>,
}

// ============================================================
// IPv4 解析与计算
// ============================================================

/// 校验输入并解析为 Ipv4Net
fn parse_ipv4(query: &str) -> Option<Ipv4Net> {
    if let Ok(net) = query.parse::<Ipv4Net>() {
        return Some(net);
    }
    if let Ok(addr) = query.parse::<Ipv4Addr>() {
        let net = Ipv4Net::new(addr, 32).expect("valid /32 network");
        return Some(net);
    }
    None
}

/// IPv4 计算
fn calc_ipv4(net: &Ipv4Net) -> CalcResult {
    let ip = net.network();
    let mask = net.netmask();
    let prefix_len = net.prefix_len();

    let total_ips = 1u64 << (32 - prefix_len);
    let usable_ips = if total_ips > 2 { total_ips - 2 } else { 0 };

    let ip_u32 = u32::from(ip);
    let mask_u32 = u32::from(mask);
    let broadcast_u32 = ip_u32 | (!mask_u32);
    let network_addr = Ipv4Addr::from(ip_u32);
    let broadcast = Ipv4Addr::from(broadcast_u32);

    let first_usable = if usable_ips > 0 {
        Some(Ipv4Addr::from(ip_u32 + 1))
    } else {
        None
    };
    let last_usable = if usable_ips > 0 {
        Some(Ipv4Addr::from(broadcast_u32 - 1))
    } else {
        None
    };

    let is_private = ip.is_private();

    let first_usable_str = first_usable.as_ref().map(|i| i.to_string());
    let last_usable_str = last_usable.as_ref().map(|i| i.to_string());
    let first_usable_range = format!(
        "{} ~ {}",
        first_usable_str.as_deref().unwrap_or("-"),
        last_usable_str.as_deref().unwrap_or("-")
    );

    let all_ranges_text = format!(
        "网络地址: {}\n子网掩码: {}\n前缀长度: {}\n可用 IP 范围: {} ~ {}\n广播地址: {}\nIP 总数: {}",
        network_addr,
        mask,
        prefix_len,
        first_usable_str.as_deref().unwrap_or("-"),
        last_usable_str.as_deref().unwrap_or("-"),
        broadcast,
        total_ips,
    );

    CalcResult {
        version: IpVersion::IPv4,
        ip: ip.to_string(),
        is_private,
        network_addr: network_addr.to_string(),
        subnet_mask: mask.to_string(),
        prefix_len: format!("/{}", prefix_len),
        total_ips: total_ips.to_string(),
        usable_ips: usable_ips.to_string(),
        first_usable_range,
        broadcast: broadcast.to_string(),
        all_ranges_text,
        ipv6_addr_type: None,
        ipv6_network_bits: None,
    }
}

// ============================================================
// IPv6 解析与计算
// ============================================================

/// 校验输入并解析为 Ipv6Net
fn parse_ipv6(query: &str) -> Option<Ipv6Net> {
    // 尝试 CIDR 格式：2001:db8::1/64
    if let Ok(net) = query.parse::<Ipv6Net>() {
        return Some(net);
    }
    // 尝试纯 IPv6 地址：补上 /128
    if let Ok(addr) = query.parse::<Ipv6Addr>() {
        let net = Ipv6Net::new(addr, 128).expect("valid /128 network");
        return Some(net);
    }
    None
}

/// 计算 2^exp 的大数字符串表示
fn pow2_string(exp: u32) -> String {
    if exp == 0 {
        return "1".to_string();
    }
    let mut result: u128 = 1;
    for _ in 0..exp {
        result *= 2;
    }
    result.to_string()
}

/// 计算 2^exp 的人类可读格式（科学计数）
fn pow2_human(exp: u32) -> String {
    if exp == 0 {
        return "1".to_string();
    }
    // 2^exp ≈ 10^(exp * log10(2))
    // log10(2) ≈ 0.30103
    let log10_val = exp as f64 * 0.30103;
    let exponent = log10_val.floor() as i64;
    let mantissa = 10.0f64.powf(log10_val - exponent as f64);

    if exponent < 0 {
        pow2_string(exp)
    } else if exponent >= 18 {
        // 超大数，用科学计数
        format!("{:.2} × 10^{}", mantissa, exponent)
    } else {
        // 中等大小：如 2^48 ≈ 281万亿
        let full = pow2_string(exp);
        if exponent >= 12 {
            // 万亿级别
            let trillion = 1_000_000_000_000u128;
            let unit = format!("万亿");
            let val_f64 = full.parse::<f64>().unwrap_or(0.0);
            format!("{:.2}{}", val_f64 / (trillion as f64), unit)
        } else if exponent >= 8 {
            let million = 1_000_000u128;
            let unit = format!("亿");
            let val_f64 = full.parse::<f64>().unwrap_or(0.0);
            format!("{:.2}{}", val_f64 / (million as f64), unit)
        } else if exponent >= 4 {
            let thousand = 1_000u128;
            let unit = format!("千");
            let val_f64 = full.parse::<f64>().unwrap_or(0.0);
            format!("{:.1}{}", val_f64 / (thousand as f64), unit)
        } else {
            full
        }
    }
}

/// IPv6 计算
fn calc_ipv6(net: &Ipv6Net) -> CalcResult {
    let ip = net.network();
    let prefix_len = net.prefix_len();
    let addr_type = classify_ipv6(&ip);
    let is_private = addr_type.is_private();

    // 网络位/接口ID分界
    let network_bits = prefix_len;
    let interface_bits = 128u32 - prefix_len as u32;

    // IPv6 地址空间
    let total_ips = pow2_string(interface_bits);
    let total_human = pow2_human(interface_bits);

    // 可用地址数
    let usable_ips = if interface_bits > 0 {
        let mut total: u128 = 1;
        for _ in 0..interface_bits {
            total *= 2;
        }
        if total > 2 {
            (total - 2).to_string()
        } else {
            "0".to_string()
        }
    } else {
        "0".to_string()
    };

    // 无子网掩码概念，无广播地址
    let first_usable = if interface_bits > 0 {
        let network_u128 = u128::from(ip);
        Some(Ipv6Addr::from(network_u128 + 1).to_string())
    } else {
        None
    };
    let last_usable = if interface_bits > 0 {
        let mut total: u128 = 1;
        for _ in 0..interface_bits {
            total *= 2;
        }
        if total > 2 {
            let network_u128 = u128::from(ip);
            Some(Ipv6Addr::from(network_u128 + total - 2).to_string())
        } else {
            None
        }
    } else {
        None
    };

    let first_usable_str = first_usable.clone();
    let last_usable_str = last_usable.clone();
    let first_usable_range = format!(
        "{} ~ {}",
        first_usable_str.as_deref().unwrap_or("-"),
        last_usable_str.as_deref().unwrap_or("-")
    );

    let network_addr_str = expand_ipv6(&ip);
    let first_us_str = first_usable.as_ref().map(|s| s.as_str()).unwrap_or("-");
    let last_us_str = last_usable.as_ref().map(|s| s.as_str()).unwrap_or("-");

    let all_ranges_text = format!(
        "网络地址: {}\n前缀长度: /{}\n网络位: {} 位 (接口ID: {} 位)\n地址类型: {}\n可用地址范围: {} ~ {}\n地址总数: {} ({})",
        network_addr_str,
        prefix_len,
        network_bits,
        interface_bits,
        addr_type.label(),
        first_us_str,
        last_us_str,
        total_human,
        total_ips,
    );

    CalcResult {
        version: IpVersion::IPv6,
        ip: network_addr_str.clone(),
        is_private,
        network_addr: network_addr_str,
        subnet_mask: format!("/{}", prefix_len),
        prefix_len: format!("/{}", prefix_len),
        total_ips: total_human,
        usable_ips,
        first_usable_range,
        broadcast: "N/A (IPv6 无广播)".to_string(),
        all_ranges_text,
        ipv6_addr_type: Some(addr_type.label().to_string()),
        ipv6_network_bits: Some(format!("{} / {}", prefix_len, interface_bits)),
    }
}

/// 扩展 IPv6 地址为标准压缩格式
fn expand_ipv6(addr: &Ipv6Addr) -> String {
    addr.to_string()
}

// ============================================================
// 统一解析与入口
// ============================================================

/// 校验输入并解析为 IP 网络
fn parse_input(query: &str) -> Result<(IpVersion, IpNetwork), String> {
    let query = query.trim();
    if query.is_empty() {
        return Err("请输入 IPv4/IPv6 地址或前缀，如 192.168.1.10/24 或 2001:db8::1/64".to_string());
    }
    if query.len() > MAX_INPUT_LEN {
        return Err("输入过长".to_string());
    }

    // 尝试 IPv4
    if let Some(net) = parse_ipv4(query) {
        return Ok((IpVersion::IPv4, IpNetwork::V4(net)));
    }

    // 尝试 IPv6
    if let Some(net) = parse_ipv6(query) {
        return Ok((IpVersion::IPv6, IpNetwork::V6(net)));
    }

    Err("IP 地址格式错误，请输入 IPv4 或 IPv6 地址及前缀长度，\n如 192.168.1.10/24 或 2001:db8::1/64".to_string())
}

/// IP 网络联合体
#[derive(Clone)]
enum IpNetwork {
    V4(Ipv4Net),
    V6(Ipv6Net),
}

/// 执行计算（统一入口）
fn calc(query: &str) -> Result<CalcResult, String> {
    let (_ip_version, net) = parse_input(query)?;
    match net {
        IpNetwork::V4(net) => Ok(calc_ipv4(&net)),
        IpNetwork::V6(net) => Ok(calc_ipv6(&net)),
    }
}

// ============================================================
// 状态管理
// ============================================================

/// IP 地址计算器状态
#[derive(Clone)]
pub struct IpCalculatorState {
    /// 输入错误
    input_error: Option<String>,
    /// 计算结果
    result: Option<CalcResult>,
}

impl IpCalculatorState {
    pub fn new(_cx: &mut Context<Self>) -> Self {
        Self {
            input_error: None,
            result: None,
        }
    }

    /// 校验并计算（点击按钮时调用）
    pub fn do_calculate(&mut self, input_text: &str) {
        self.input_error = None;
        self.result = None;
        match calc(input_text) {
            Ok(result) => self.result = Some(result),
            Err(err) => self.input_error = Some(err),
        }
    }
}

// ============================================================
// UI
// ============================================================

/// IP 地址计算器工具
pub struct IpCalculatorTool<'a> {
    _app: &'a NetLiteApp,
    state: Entity<IpCalculatorState>,
}

impl<'a> IpCalculatorTool<'a> {
    pub fn new(app: &'a NetLiteApp) -> Self {
        Self {
            _app: app,
            state: app.ip_calculator_state.clone(),
        }
    }

    pub fn render(self, window: &mut Window, cx: &mut Context<NetLiteApp>) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .flex_1()
            .overflow_hidden()
            .bg(cx.theme().background)
            .child(Self::render_input_panel(&self.state, self._app, cx))
            .child(Self::render_content(&self.state, window, cx))
            .into_any_element()
    }

    /// 执行计算并更新状态
    fn do_calculate(app: &NetLiteApp, state: &Entity<IpCalculatorState>, cx: &mut Context<NetLiteApp>) {
        let text = {
            let input = app.ip_calculator_input.clone();
            input.read_with(cx, |input_state, _| input_state.text().to_string())
        };
        state.update(cx, |state, _| {
            state.do_calculate(&text);
        });
    }

    /// 渲染输入面板
    fn render_input_panel(
        state: &Entity<IpCalculatorState>,
        app: &NetLiteApp,
        cx: &mut Context<NetLiteApp>,
    ) -> impl IntoElement {
        let theme = cx.theme().clone();
        let state_clone = state.clone();

        // 实时校验
        let mut input_error: Option<String> = None;
        app.ip_calculator_input.read_with(cx, |input, _| {
            let query = input.text().to_string();
            if !query.trim().is_empty() {
                input_error = match calc(&query) {
                    Ok(_) => None,
                    Err(err) => Some(err),
                };
            }
        });

        // 同步错误状态到 state
        if let Some(ref err) = input_error {
            if state.read(cx).input_error.as_ref() != Some(err) {
                state.update(cx, |s, _| s.input_error = Some(err.clone()));
            }
        } else if state.read(cx).input_error.is_some() {
            state.update(cx, |s, _| s.input_error = None);
        }

        let has_error = input_error.is_some();

        div()
            .flex()
            .flex_col()
            .gap_2()
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
                            .border_color(has_error.then_some(theme.red).unwrap_or(theme.border))
                            .rounded_md()
                            .relative()
                            .child(Input::new(&app.ip_calculator_input).w_0().flex_1().appearance(false).h_9())
                            .children(
                                app.ip_calculator_input.read_with(cx, |input, _| input.text().len() == 0).then(|| {
                                    div()
                                        .absolute()
                                        .left_2()
                                        .top_0()
                                        .flex()
                                        .items_center()
                                        .h_9()
                                        .text_sm()
                                        .text_color(theme.muted_foreground.opacity(0.5))
                                        .child("请输入 IPv4/IPv6 地址及前缀，如 192.168.1.10/24")
                                }),
                            ),
                    )
                    .child(
                        div()
                            .px_4()
                            .py_1p5()
                            .rounded_md()
                            .bg(if has_error { theme.border } else { theme.primary })
                            .text_sm()
                            .font_medium()
                            .opacity(if has_error { 0.5 } else { 1.0 })
                            .cursor_pointer()
                            .text_color(theme.background)
                            .child("计算")
                            .on_mouse_down(MouseButton::Left, cx.listener(
                                move |app: &mut NetLiteApp, _event: &MouseDownEvent, _window, cx| {
                                    if has_error {
                                        return;
                                    }
                                    Self::do_calculate(app, &state_clone, cx);
                                },
                            )),
                    ),
            )
            .children(input_error.map(|err| {
                div()
                    .text_xs()
                    .text_color(theme.red)
                    .px_1()
                    .child(err)
            }))
    }

    /// 渲染内容区域
    fn render_content(state: &Entity<IpCalculatorState>, _window: &mut Window, cx: &mut Context<NetLiteApp>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let result = state.read(cx).result.clone();

        if let Some(ref r) = result {
            let r_clone = r.clone();
            div()
                .flex()
                .flex_col()
                .flex_1()
                .overflow_y_scrollbar()
                .px_6()
                .py_4()
                .children(vec![
                    Self::render_ip_header(r_clone.ip.clone(), r_clone.version.clone(), &r_clone, &theme).into_any_element(),
                    Self::render_info_cards(&r_clone, &theme).into_any_element(),
                    div()
                        .flex()
                        .flex_col()
                        .gap_4()
                        .w_0()
                        .flex_1()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_start()
                                .gap_3()
                                .child(
                                    div()
                                        .text_base()
                                        .font_semibold()
                                        .text_color(theme.foreground)
                                        .child("地址范围"),
                                )
                                .child(
                                    Self::render_copy_button(r_clone.all_ranges_text.clone()),
                                ),
                        )
                        .children(vec![
                            Self::detail_row("网络地址".to_string(), r_clone.network_addr, theme.muted_foreground, theme.foreground).into_any_element(),
                            Self::detail_row("可用地址范围".to_string(), r_clone.first_usable_range, theme.muted_foreground, theme.foreground).into_any_element(),
                            Self::detail_row("广播地址".to_string(), r_clone.broadcast, theme.muted_foreground, theme.foreground).into_any_element(),
                        ])
                        .into_any_element(),
                ])
        } else {
            div()
                .flex()
                .flex_col()
                .flex_1()
                .overflow_y_scrollbar()
                .justify_center()
                .items_center()
                .child(Icon::new(CustomIconName::Calculator).size(px(48.0)).text_color(theme.muted_foreground.opacity(0.5)))
                .child(div().text_lg().font_medium().text_color(theme.muted_foreground.opacity(0.6)).child("输入 IPv4/IPv6 地址后点击计算"))
        }
    }

    /// 渲染 IP 头部
    fn render_ip_header(ip: String, version: IpVersion, r: &CalcResult, theme: &gpui_component::Theme) -> impl IntoElement {
        div()
            .flex()
            .items_center()
            .flex_wrap()
            .gap_3()
            .pb_4()
            .child(
                div()
                    .text_2xl()
                    .font_semibold()
                    .text_color(theme.foreground)
                    .child(ip),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(match version {
                        IpVersion::IPv4 => "IPv4",
                        IpVersion::IPv6 => "IPv6",
                    }),
            )
            .children(match version {
                IpVersion::IPv4 => {
                    let type_color = if r.is_private { gpui::rgb(0x55AA7F) } else { gpui::rgb(0xFF8E77) };
                    let type_label = if r.is_private { "私有地址" } else { "公网地址" };
                    Some(
                        div()
                            .text_sm()
                            .text_color(type_color)
                            .child(type_label)
                            .into_any_element(),
                    )
                }
                IpVersion::IPv6 => {
                    if let Some(ref type_label) = r.ipv6_addr_type {
                        let color = match type_label.as_str() {
                            "全球单播 (GUA)" => gpui::rgb(0x7799FF),
                            "唯一本地 (ULA)" => gpui::rgb(0x55AA7F),
                            "链路本地 (LLA)" => gpui::rgb(0xFFAA55),
                            "环回地址" => gpui::rgb(0x55AA7F),
                            "组播 (Mcast)" => gpui::rgb(0x7799FF),
                            _ => gpui::rgb(0xFF8E77),
                        };
                        Some(
                            div()
                                .text_sm()
                                .text_color(color)
                                .child(type_label.clone())
                                .into_any_element(),
                        )
                    } else {
                        None
                    }
                }
            })
    }

    /// 渲染信息卡片
    fn render_info_cards(r: &CalcResult, theme: &gpui_component::Theme) -> impl IntoElement {
        let card_bg = theme.background;
        let card_border = theme.border;
        let label_color = theme.muted_foreground;
        let value_color = theme.foreground;

        div()
            .flex()
            .flex_wrap()
            .gap_3()
            .pb_4()
            .children(match r.version {
                IpVersion::IPv4 => {
                    vec![
                        Self::card_inner("网络地址".to_string(), r.network_addr.clone(), card_bg, card_border, label_color, value_color),
                        Self::card_inner("子网掩码".to_string(), r.subnet_mask.clone(), card_bg, card_border, label_color, value_color),
                        Self::card_inner("前缀长度".to_string(), r.prefix_len.clone(), card_bg, card_border, label_color, value_color),
                        Self::card_inner("地址总数".to_string(), r.total_ips.clone(), card_bg, card_border, label_color, value_color),
                        Self::card_inner("可用地址".to_string(), r.usable_ips.clone(), card_bg, card_border, label_color, value_color),
                    ]
                }
                IpVersion::IPv6 => {
                    vec![
                        Self::card_inner("前缀长度".to_string(), r.prefix_len.clone(), card_bg, card_border, label_color, value_color),
                        Self::card_inner("地址类型".to_string(), r.ipv6_addr_type.as_deref().unwrap_or("-").to_string(), card_bg, card_border, label_color, value_color),
                        Self::card_inner("网络/接口位".to_string(), r.ipv6_network_bits.as_deref().unwrap_or("-").to_string(), card_bg, card_border, label_color, value_color),
                        Self::card_inner("地址空间".to_string(), r.total_ips.clone(), card_bg, card_border, label_color, value_color),
                        Self::card_inner("可用地址".to_string(), r.usable_ips.clone(), card_bg, card_border, label_color, value_color),
                    ]
                }
            })
    }

    /// 单个信息卡片内部 div
    fn card_inner(
        label: String,
        value: String,
        card_bg: gpui::Hsla,
        card_border: gpui::Hsla,
        label_color: gpui::Hsla,
        value_color: gpui::Hsla,
    ) -> impl IntoElement {
        div()
            .flex_1()
            .min_w_0()
            .w_32()
            .bg(card_bg)
            .border_1()
            .border_color(card_border)
            .rounded_md()
            .px_3()
            .py_2()
            .flex()
            .flex_col()
            .gap_1()
            .child(div().text_xs().text_color(label_color).child(label))
            .child(div().text_sm().font_semibold().text_color(value_color).child(value))
    }

    /// 单行详情
    fn detail_row(label: String, value: String, label_color: gpui::Hsla, value_color: gpui::Hsla) -> impl IntoElement {
        div()
            .flex()
            .items_center()
            .justify_start()
            .gap_4()
            .py_1()
            .child(div().text_sm().text_color(label_color).child(label))
            .child(
                div().flex().items_center()
                    .min_w_0()
                    .flex_1()
                    .child(div().text_sm().font_medium().text_color(value_color).child(value)),
            )
    }

    /// 复制按钮
    fn render_copy_button(value: String) -> impl IntoElement {
        Clipboard::new(ElementId::Name("copy-btn".into()))
            .value(value)
    }
}
