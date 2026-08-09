use gpui::prelude::FluentBuilder;
use gpui::*;

use crate::app::NetLiteApp;
use gpui_component::ActiveTheme as _;
use gpui_component::StyledExt;
use gpui_component::scroll::ScrollableElement;

/// TCP/UDP 测试工具组件
/// 整合了 ConnectionPanel + TabContainer + ConnectionTab 的渲染逻辑
pub struct TcpUdpTestTool<'a> {
    app: &'a NetLiteApp,
}

impl<'a> TcpUdpTestTool<'a> {
    pub fn new(app: &'a NetLiteApp) -> Self {
        Self { app }
    }

    pub fn render(
        self,
        window: &mut Window,
        cx: &mut Context<NetLiteApp>,
    ) -> AnyElement {
        let theme = cx.theme().clone();

        div()
            .flex()
            .flex_1()
            .overflow_hidden()
            .bg(theme.background)
            .child(
                div()
                    .w(px(200.))
                    .h_full()
                    .overflow_y_scrollbar()
                    .child(self.render_left_panel(window, cx)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .overflow_x_hidden()
                    .child(self.render_right_panel(window, cx)),
            )
            .into_any_element()
    }

    fn render_left_panel(&self, window: &mut Window, cx: &mut Context<NetLiteApp>) -> AnyElement {
        crate::ui::connection_panel::ConnectionPanel::new(self.app).render(window, cx).into_any_element()
    }

    fn render_right_panel(&self, window: &mut Window, cx: &mut Context<NetLiteApp>) -> AnyElement {
        let tabs = self.get_tabs();
        let theme = cx.theme().clone();

        div()
            .flex()
            .flex_col()
            .flex_1()
            .bg(theme.background)
            .child(self.render_tab_header(&tabs, cx))
            .child(self.render_tab_content(window, cx))
            .into_any_element()
    }

    fn get_tabs(&self) -> Vec<TabInfo> {
        let mut tabs = Vec::new();
        for (tab_id, tab_state) in &self.app.connection_tabs {
            let address = tab_state.address();
            let protocol = tab_state.protocol();
            let connection_type = if tab_state.connection_config.is_client() { "C" } else { "S" };
            let name = format!("{} [{}-{}]", address, connection_type, protocol);
            tabs.push(TabInfo {
                id: (*tab_id).to_string(),
                name,
                is_active: self.app.active_tab == *tab_id,
            });
        }
        tabs
    }

    fn render_tab_header(&self, tabs: &[TabInfo], cx: &mut Context<NetLiteApp>) -> AnyElement {
        let theme = cx.theme().clone();
        let is_tab_multiline = self.app.tab_multiline;

        div()
            .flex()
            .gap_1()
            .p_1()
            .bg(theme.secondary)
            .border_b_1()
            .border_color(theme.border)
            .min_h(px(32.))
            .w_full()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .flex_1()
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .when(is_tab_multiline, |div| div.flex_wrap())
                    .children(
                        tabs.iter().map(|tab| {
                            let tab_id = tab.id.clone();
                            let is_active = tab.is_active;
                            let tab_name = tab.name.clone();

                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .px_3()
                                .py_1()
                                .cursor_pointer()
                                .hover(|style| style.bg(theme.border))
                                .when(is_active, |div| {
                                    div.bg(theme.primary)
                                        .text_color(theme.background)
                                        .border_1()
                                        .border_color(theme.primary)
                                        .border_b_0()
                                })
                                .when(!is_active, |div| {
                                    div.bg(theme.secondary)
                                        .text_color(theme.muted_foreground)
                                })
                                .on_mouse_down(MouseButton::Left, {
                                    let tab_id_clone = tab_id.clone();
                                    cx.listener(move |app: &mut NetLiteApp, _event: &MouseDownEvent, _window: &mut Window, cx: &mut Context<NetLiteApp>| {
                                        app.active_tab = tab_id_clone.clone();
                                        cx.notify();
                                    })
                                })
                                .child(
                                    div()
                                        .text_xs()
                                        .font_medium()
                                        .max_w(px(150.))
                                        .overflow_hidden()
                                        .text_ellipsis()
                                        .whitespace_nowrap()
                                        .child(tab_name),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .when(is_active, |div| div.text_color(theme.background))
                                        .when(!is_active, |div| div.text_color(gpui::rgb(0x9ca3af)))
                                        .hover(|style| style.text_color(gpui::rgb(0xef4444)))
                                        .cursor_pointer()
                                        .child("×")
                                        .on_mouse_down(MouseButton::Left, {
                                            let tab_id_clone = tab_id.clone();
                                            cx.listener(move |app: &mut NetLiteApp, _event: &MouseDownEvent, _window: &mut Window, cx: &mut Context<NetLiteApp>| {
                                                app.close_tab(tab_id_clone.clone(), cx);
                                                if app.active_tab == tab_id_clone {
                                                    if let Some(first_tab_id) = app.connection_tabs.keys().next() {
                                                        app.active_tab = (*first_tab_id).to_string();
                                                    } else {
                                                        app.active_tab = String::new();
                                                    }
                                                }
                                                cx.notify();
                                            })
                                        }),
                                )
                        }),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .flex_shrink_0()
                    .h_8()
                    .px_2()
                    .py_1()
                    .cursor_pointer()
                    .bg(theme.secondary)
                    .border_1()
                    .border_color(theme.border)
                    .hover(|style| style.bg(theme.border))
                    .on_mouse_down(MouseButton::Left, {
                        cx.listener(move |app: &mut NetLiteApp, _event: &MouseDownEvent, _window: &mut Window, cx: &mut Context<NetLiteApp>| {
                            app.tab_multiline = !app.tab_multiline;
                            cx.notify();
                        })
                    })
                    .child(if is_tab_multiline {
                        gpui_component::IconName::ChevronUp
                    } else {
                        gpui_component::IconName::ChevronDown
                    }),
            )
            .into_any_element()
    }

    fn render_tab_content(&self, window: &mut Window, cx: &mut Context<NetLiteApp>) -> AnyElement {
        if let Some((tab_id, tab_state)) = self.app.connection_tabs.get_key_value(&self.app.active_tab) {
            crate::ui::connection_tab::ConnectionTab::new(self.app, (*tab_id).clone(), tab_state).render(window, cx).into_any_element()
        } else {
            div().flex().flex_col().flex_1().child(
                div().flex().items_center().justify_center().flex_1().child(
                    div()
                        .text_sm()
                        .text_color(gpui::rgb(0x9ca3af))
                        .child("请先创建连接"),
                ),
            ).into_any_element()
        }
    }
}

/// 标签页信息（TabContainer 中的定义）
#[derive(Debug, Clone)]
struct TabInfo {
    id: String,
    name: String,
    is_active: bool,
}
