use gpui::prelude::*;

use gpui::*;
use gpui_component::scroll::ScrollableElement;
use gpui_component::StyledExt;
use gpui_component::input::Input;
use gpui_component::ActiveTheme;
use gpui_component::Icon;
use crate::custom_icons::CustomIconName;
use ipnet::Ipv4Net;
use std::net::Ipv4Addr;
use std::time::Duration;

/// 复制按钮状态（在 Window 中存储）
#[derive(Default)]
struct CopyButtonState {
    copied: bool,
}

use crate::app::NetLiteApp;

/// IP 地址计算器单次计算最大输入长度
const MAX_INPUT_LEN: usize = 64;

/// 计算结果（所有字段 Owned，避免生命周期问题）
#[derive(Clone)]
struct CalcResult {
    ip: String,
    is_private: bool,
    network_addr: String,
    subnet_mask: String,
    prefix_len: String,
    total_ips: String,
    usable_ips: String,
    first_usable: Option<String>,
    last_usable: Option<String>,
    first_usable_range: String,
    broadcast: String,
    all_ranges_text: String,
}

/// 校验输入并解析为 Ipv4Net，返回错误信息或网络
/// 支持 CIDR 格式（如 192.168.1.10/24）和纯 IP 地址（如 192.168.1.1，视为 /32）
fn parse_input(query: &str) -> Result<Ipv4Net, String> {
    let query = query.trim();
    if query.is_empty() {
        return Err("请输入 IP/mask 或 IP/subnet，如 192.168.1.10/24".to_string());
    }
    if query.len() > MAX_INPUT_LEN {
        return Err("输入过长".to_string());
    }
    // 先尝试 CIDR 格式
    if let Ok(net) = query.parse::<Ipv4Net>() {
        return Ok(net);
    }
    // 再尝试纯 IP 地址格式（补上 /32）
    if let Ok(addr) = query.parse::<Ipv4Addr>() {
        let net = Ipv4Net::new(addr, 32).expect("valid /32 network");
        return Ok(net);
    }
    Err("IP 地址格式错误，请输入 IP/mask 或 IP/subnet，如 192.168.1.10/255.255.255.0 或 192.168.1.10/24".to_string())
}

/// 执行计算
fn calc(net: &Ipv4Net) -> CalcResult {
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
        "网络地址: {}\n子网掩码: /{}\n可用 IP 范围: {} ~ {}\n广播地址: {}",
        network_addr,
        prefix_len,
        first_usable_str.as_deref().unwrap_or("-"),
        last_usable_str.as_deref().unwrap_or("-"),
        broadcast,
    );

    CalcResult {
        ip: ip.to_string(),
        is_private,
        network_addr: network_addr.to_string(),
        subnet_mask: mask.to_string(),
        prefix_len: format!("/{}", prefix_len),
        total_ips: total_ips.to_string(),
        usable_ips: usable_ips.to_string(),
        first_usable: first_usable_str,
        last_usable: last_usable_str,
        first_usable_range,
        broadcast: broadcast.to_string(),
        all_ranges_text,
    }
}

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
        let net = match parse_input(input_text) {
            Ok(net) => net,
            Err(err) => {
                self.input_error = Some(err);
                return;
            }
        };
        self.result = Some(calc(&net));
    }
}

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

        // 实时校验：在渲染阶段读取输入并验证，自动处理输入/删除/粘贴等所有场景
        let mut input_error: Option<String> = None;
        app.ip_calculator_input.read_with(cx, |input, _| {
            let query = input.text().to_string();
            // 空输入不报错，等待用户开始输入
            if !query.trim().is_empty() {
                input_error = parse_input(&query).err();
            }
        });

        // 同步错误状态到 state（供按钮 handler 等其他地方读取）
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
                                        .child("请输入 IP/mask 或 IP/subnet")
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
                                    // 有错误时不执行计算
                                    if has_error {
                                        return;
                                    }
                                    Self::do_calculate(app, &state_clone, cx);
                                },
                            )),
                    ),
            )
            // 错误信息显示在输入区域下方，实时显示/隐藏
            .children(input_error.map(|err| {
                div()
                    .text_xs()
                    .text_color(theme.red)
                    .px_1()
                    .child(err)
            }))
    }

    /// 渲染内容区域
    fn render_content(state: &Entity<IpCalculatorState>, window: &mut Window, cx: &mut Context<NetLiteApp>) -> impl IntoElement {
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
                    Self::render_ip_header(r_clone.ip.clone(), r_clone.is_private, &theme).into_any_element(),
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
                                    Self::render_copy_button(ElementId::Name("copy-all-ranges".into()), r_clone.all_ranges_text.clone(), &theme, window, cx),
                                ),
                        )
                        .children(vec![
                            Self::detail_row("网络地址".to_string(), r_clone.network_addr, theme.muted_foreground, theme.foreground).into_any_element(),
                            Self::detail_row("可用 IP 范围".to_string(), r_clone.first_usable_range, theme.muted_foreground, theme.foreground).into_any_element(),
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
                .child(div().text_lg().font_medium().text_color(theme.muted_foreground.opacity(0.6)).child("输入 IP 网段后点击计算"))
        }
    }

    /// 渲染 IP 头部
    fn render_ip_header(ip: String, is_private: bool, theme: &gpui_component::Theme) -> impl IntoElement {
        let type_color = if is_private { gpui::rgb(0x55AA7F) } else { gpui::rgb(0xFF8E77) };
        let type_label = if is_private { "私有地址" } else { "公网地址" };

        div()
            .flex()
            .items_center()
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
                    .child("IPv4"),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(type_color)
                    .child(type_label),
            )
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
            .children(vec![
                Self::info_card("网络地址".to_string(), r.network_addr.clone(), card_bg, card_border, label_color, value_color),
                Self::info_card("子网掩码".to_string(), r.subnet_mask.clone(), card_bg, card_border, label_color, value_color),
                Self::info_card("前缀长度".to_string(), r.prefix_len.clone(), card_bg, card_border, label_color, value_color),
                Self::info_card("IP 地址总数".to_string(), r.total_ips.clone(), card_bg, card_border, label_color, value_color),
                Self::info_card("可用 IP".to_string(), r.usable_ips.clone(), card_bg, card_border, label_color, value_color),
            ])
    }

    /// 单个信息卡片
    fn info_card(
        label: String,
        value: String,
        card_bg: impl Into<Rgba>,
        card_border: impl Into<Rgba>,
        label_color: impl Into<Rgba>,
        value_color: impl Into<Rgba>,
    ) -> impl IntoElement {
        let card_bg = card_bg.into();
        let card_border = card_border.into();
        let label_color = label_color.into();
        let value_color = value_color.into();
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
    fn detail_row(label: String, value: String, label_color: impl Into<Rgba>, value_color: impl Into<Rgba>) -> impl IntoElement {
        let label_color = label_color.into();
        let value_color = value_color.into();
        div()
            .flex()
            .items_center()
            .justify_start()
            .gap_4()
            .py_1()
            .child(div().text_sm().text_color(label_color).child(label.clone()))
            .child(
                div().flex().items_center()
                    .min_w_0()
                    .flex_1()
                    .child(div().text_sm().font_medium().text_color(value_color).child(value)),
            )
    }

    /// 复制按钮：悬停显示 Copy，点击后显示 Copied
    fn render_copy_button(
        id: ElementId,
        value: String,
        theme: &gpui_component::Theme,
        window: &mut Window,
        cx: &mut Context<NetLiteApp>,
    ) -> AnyElement {
        let state = window.use_keyed_state(id.clone(), cx, |_, _| CopyButtonState::default());
        let copied = state.read(cx).copied;
        let text = if copied { "Copied" } else { "Copy" };
        let value_clone = value.clone();

        div()
            .id(id.clone())
            .cursor_pointer()
            .px_2()
            .py_1()
            .rounded_sm()
            .text_xs()
            .text_color(theme.muted_foreground)
            .hover(|s| s.bg(theme.secondary).text_color(theme.foreground))
            .on_click({
                let state = state.clone();
                move |_, window, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(value_clone.clone()));
                    state.update(cx, |state, cx| {
                        state.copied = true;
                        cx.notify();
                    });
                    let state = state.clone();
                    cx.spawn(async move |cx| {
                        cx.background_executor().timer(Duration::from_secs(1)).await;
                        let _ = state.update(cx, |state, cx| {
                            state.copied = false;
                            cx.notify();
                        });
                    })
                    .detach();
                }
            })
            .child(text)
            .into_any_element()
    }
}
