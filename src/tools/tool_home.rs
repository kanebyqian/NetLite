use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::StyledExt;
use gpui_component::{Icon, IconName};
use gpui_component::ActiveTheme as _;
use gpui_component::input::Input;

use crate::app::NetLiteApp;
use crate::tools::{AVAILABLE_TOOLS, CurrentView};

/// 工具卡片首页
pub struct ToolHomePage<'a> {
    app: &'a NetLiteApp,
}

impl<'a> ToolHomePage<'a> {
    pub fn new(app: &'a NetLiteApp) -> Self {
        Self { app }
    }

    pub fn render(
        self,
        _window: &mut Window,
        cx: &mut Context<NetLiteApp>,
    ) -> impl IntoElement {
        let theme = cx.theme().clone();

        div()
            .flex()
            .flex_col()
            .flex_1()
            .bg(theme.background)
            .child(self.render_header(&theme, cx))
            .child(self.render_search_bar(&theme, cx))
            .child(self.render_tools_grid(&theme, cx))
    }

    /// 渲染顶部标题区域
    fn render_header(&self, theme: &gpui_component::Theme, _cx: &mut Context<NetLiteApp>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .h_48()
            .child(
                div()
                    .text_3xl()
                    .font_semibold()
                    .text_color(theme.foreground)
                    .child("NetLite"),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child("小巧 · 简单 · 实用"),
            )
    }

    /// 渲染搜索栏
    fn render_search_bar(&self, theme: &gpui_component::Theme, cx: &mut Context<NetLiteApp>) -> impl IntoElement {
        let input_entity = self.app.tool_search_input.clone();
        let input_text = input_entity.read(cx).text().to_string();

        div()
            .flex()
            .items_center()
            .justify_center()
            .px_8()
            .mb_4()
            .child(
                div()
                    .w_full()
                    .max_w_96()
                    .h_10()
                    .bg(theme.secondary)
                    .border_1()
                    .border_color(theme.border)
                    .rounded_lg()
                    .flex()
                    .items_center()
                    .relative()
                    .child(
                        div().flex().flex_1().child(
                            Input::new(&input_entity)
                                .w_0()
                                .flex_1()
                                .appearance(false)
                                .h_full(),
                        ),
                    )
                    .children(
                        input_entity.read_with(cx, |input, _| input.text().len() == 0)
                            .then(|| {
                                div().absolute().left_2().top_0().h_10()
                                    .flex().items_center()
                                    .text_sm().text_color(theme.muted_foreground.opacity(0.5))
                                    .child("搜索工具名称或简介……")
                            })
                    )
                    .child(
                        div()
                            .pl_3()
                            .child(
                                Icon::new(IconName::Search)
                                    .size(px(16.0))
                                    .text_color(theme.muted_foreground),
                            )
                    )
                    .when(!input_text.is_empty(), |content| {
                        content.child(
                            div()
                                .cursor_pointer()
                                .px_1()
                                .rounded_full()
                                .hover(|style| style.bg(theme.border))
                                .child(Icon::new(IconName::Close).size(px(12.0)))
                                .on_mouse_down(MouseButton::Left, cx.listener(
                                    |app: &mut NetLiteApp, _event: &MouseDownEvent, _window: &mut Window, cx: &mut Context<NetLiteApp>| {
                                        app.tool_search_query.clear();
                                        cx.notify();
                                    },
                                )),
                        )
                    }),
            )
    }

    /// 根据搜索关键词过滤工具列表
    fn filter_tools(&self, query: &str) -> Vec<&'static crate::tools::Tool> {
        let q = query.to_lowercase();
        AVAILABLE_TOOLS.iter().filter(|tool| {
            query.is_empty()
                || tool.name.to_lowercase().contains(&q)
                || tool.description.to_lowercase().contains(&q)
        }).collect()
    }

    /// 渲染工具卡片网格
    fn render_tools_grid(&self, theme: &gpui_component::Theme, cx: &mut Context<NetLiteApp>) -> impl IntoElement {
        let input_text = self.app.tool_search_input.read(cx).text().to_string();
        let tools = self.filter_tools(&input_text);

        div()
            .flex()
            .flex_col()
            .items_center()
            .px_8()
            .child(
                div()
                    .text_sm()
                    .font_medium()
                    .text_color(theme.foreground)
                    .mb_3()
                    .child("全部工具"),
            )
            .child(
                div()
                    .grid()
                    .grid_cols(3)
                    .gap_4()
                    .max_w(px(576.))
                    .children(
                        tools.iter().map(|tool| {
                            let tool_id = tool.id;
                            let tool_name = tool.name;
                            let tool_desc = tool.description;
                            let icon = tool.icon.clone();

                            div()
                                .w_full()
                                .h_32()
                                .bg(theme.secondary)
                                .border_1()
                                .border_color(theme.border)
                                .rounded_lg()
                                .flex()
                                .flex_col()
                                .items_center()
                                .justify_center()
                                .gap_2()
                                .cursor_pointer()
                                .hover(|style| style.bg(theme.border))
                                .child(
                                    Icon::new(icon)
                                        .size(px(24.0))
                                        .text_color(theme.foreground),
                                )
                                .child(
                                    div()
                                        .text_sm()
                                        .font_medium()
                                        .text_color(theme.foreground)
                                        .child(tool_name),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .child(tool_desc),
                                )
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |app: &mut NetLiteApp, _event: &MouseDownEvent, _window: &mut Window, cx: &mut Context<NetLiteApp>| {
                                        app.current_view = CurrentView::ToolDetail {
                                            tool_id: tool_id.to_string(),
                                        };
                                        cx.notify();
                                    }),
                                )
                        }),
                    ),
            )
            .when(tools.is_empty(), |content| {
                content.child(
                    div()
                        .text_sm()
                        .text_color(gpui::rgb(0x9ca3af))
                        .mt_4()
                        .child("没有找到匹配的工具"),
                )
            })
    }
}
