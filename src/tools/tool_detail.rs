use gpui::*;

use crate::app::NetLiteApp;
use crate::tools::Tool;
use gpui_component::ActiveTheme as _;
use gpui_component::StyledExt;
use gpui_component::{Icon, IconName};

/// 工具详情页
pub struct ToolDetailView<'a> {
    app: &'a NetLiteApp,
    tool: &'a Tool,
}

impl<'a> ToolDetailView<'a> {
    pub fn new(app: &'a NetLiteApp, tool_id: &'a str) -> Self {
        Self {
            app,
            tool: Tool::from_id(tool_id).expect("unknown tool"),
        }
    }

    pub fn render(
        self,
        window: &mut Window,
        cx: &mut Context<NetLiteApp>,
    ) -> impl IntoElement {
        let theme = cx.theme().clone();

        div()
            .flex()
            .flex_col()
            .flex_1()
            .overflow_hidden()
            .bg(theme.background)
            .child(self.render_toolbar(window, cx))
            .child(self.render_content(window, cx))
    }

    fn render_toolbar(&self, _window: &mut Window, cx: &mut Context<NetLiteApp>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let tool_name = self.tool.name;
        let icon = self.tool.icon.clone();

        div()
            .flex()
            .items_center()
            .gap_3()
            .px_4()
            .py_2()
            .bg(theme.secondary)
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .px_3()
                    .py_1()
                    .cursor_pointer()
                    .rounded_md()
                    .hover(|style| style.bg(theme.border))
                    .child(Icon::new(IconName::ArrowLeft).size(px(16.0)))
                    .on_mouse_down(MouseButton::Left, cx.listener(
                        |app: &mut NetLiteApp, _event: &MouseDownEvent, _window: &mut Window, cx: &mut Context<NetLiteApp>| {
                            app.current_view = crate::tools::CurrentView::ToolHome;
                            cx.notify();
                        },
                    )),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(Icon::new(icon).size(px(18.0)))
                    .child(
                        div()
                            .text_base()
                            .font_semibold()
                            .text_color(theme.foreground)
                            .child(tool_name),
                    ),
            )
    }

    fn render_content(&self, window: &mut Window, cx: &mut Context<NetLiteApp>) -> impl IntoElement {
        match self.tool.id {
            "tcp_udp_test" => div().flex().flex_col().flex_1().child(
                crate::tools::tcp_udp_test::TcpUdpTestTool::new(self.app).render(window, cx),
            ),
            "ip_scanner" => div().flex().flex_col().flex_1().child(
                crate::tools::ip_scanner::IpScannerTool::new(self.app).render(window, cx),
            ),
            "ip_calculator" => div().flex().flex_col().flex_1().child(
                crate::tools::ip_calculator::IpCalculatorTool::new(self.app).render(window, cx),
            ),
            _ => div().flex().flex_col().flex_1().child(
                self.render_placeholder(cx),
            ),
        }
    }

    fn render_placeholder(&self, cx: &Context<NetLiteApp>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let icon = self.tool.icon.clone();

        div()
            .flex()
            .items_center()
            .justify_center()
            .flex_1()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_3()
                    .child(Icon::new(icon).size(px(48.0)))
                    .child(
                        div()
                            .text_lg()
                            .font_semibold()
                            .text_color(theme.foreground)
                            .child(self.tool.name),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(self.tool.description),
                    )
                    .child(
                        div()
                            .text_sm()
                            .font_medium()
                            .text_color(gpui::rgb(0x9ca3af))
                            .child("暂未实现，敬请期待"),
                    ),
            )
    }
}
