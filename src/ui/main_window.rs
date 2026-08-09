use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::StyledExt;
use gpui_component::IconName;
use gpui_component::ActiveTheme;
use gpui_component::TitleBar;
use crate::app::NetLiteApp;
use crate::theme_event_handler::{ThemeEventHandler, apply_theme};
use crate::tools::CurrentView;
use crate::ui::dialog::{NewConnectionDialog, DecoderSelectionDialog, FavoriteRemarkDialog, FavoriteListPanel, AddClientDialog};

pub struct MainWindow<'a> {
    app: &'a NetLiteApp,
}

impl<'a> MainWindow<'a> {
    pub fn new(app: &'a NetLiteApp, _cx: &mut Context<NetLiteApp>) -> Self {
        Self { app }
    }

    pub fn render(
        self,
        window: &mut Window,
        cx: &mut Context<NetLiteApp>,
    ) -> impl IntoElement {
        let theme = cx.theme().clone();

        div()
            .w_full()
            .h_full()
            .flex()
            .flex_col()
            .bg(theme.background)
            .on_key_down(cx.listener(|app, event: &KeyDownEvent, _window, cx| {
                if event.keystroke.key.as_str() == "escape" {
                    if app.show_favorite_list {
                        app.show_favorite_list = false;
                        cx.notify();
                    }
                }
            }))
            .child(
                TitleBar::new()
                    .on_close_window(|_, window: &mut Window, _cx| {
                        window.remove_window();
                    })
                    .child(
                        div()
                            .text_lg()
                            .font_semibold()
                            .text_color(theme.foreground)
                            .child("NetLite"),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .w_8()
                                    .h_8()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .cursor_pointer()
                                    .rounded_md()
                                    .hover(|style| style.bg(theme.border))
                                    .child(
                                        if cx.global::<ThemeEventHandler>().is_dark_mode() {
                                            IconName::Sun
                                        } else {
                                            IconName::Moon
                                        }
                                    )
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(move |_app, _event, _window, cx| {
                                            cx.global_mut::<ThemeEventHandler>().toggle_theme();
                                            let is_dark = cx.global::<ThemeEventHandler>().is_dark_mode();
                                            apply_theme(is_dark, cx);
                                            cx.notify();
                                        }),
                                    ),
                            ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_1()
                    .overflow_hidden()
                    .child(match &self.app.current_view {
                        CurrentView::ToolHome => {
                            div()
                                .flex()
                                .flex_col()
                                .flex_1()
                                .overflow_hidden()
                                .child(crate::tools::tool_home::ToolHomePage::new(self.app).render(window, cx))
                        }
                        CurrentView::ToolDetail { tool_id } => {
                            div()
                                .flex()
                                .flex_col()
                                .flex_1()
                                .overflow_hidden()
                                .child(crate::tools::tool_detail::ToolDetailView::new(self.app, tool_id).render(window, cx))
                        }
                    }),
            )
            .when(self.app.show_new_connection, |this_div| {
                this_div.child(NewConnectionDialog::new(self.app).render(window, cx))
            })
            .when(self.app.show_decoder_selection, |this_div| {
                this_div.child(DecoderSelectionDialog::new(self.app).render(window, cx))
            })
            .when(self.app.show_add_client_dialog, |this_div| {
                if let Some(input) = self.app.add_client_dialog_input.clone() {
                    let error = self.app.add_client_dialog_error.clone();
                    this_div.child(AddClientDialog::new(self.app, input, error).render(window, cx))
                } else {
                    this_div
                }
            })
            .when(self.app.show_favorite_remark, |this_div| {
                this_div.child(FavoriteRemarkDialog::new(self.app, self.app.favorite_remark_input.clone()).render(window, cx))
            })
            .when(self.app.show_favorite_list, |this_div| {
                this_div.child(FavoriteListPanel::new(self.app, self.app.favorite_list_search_input.clone()).render(window, cx))
            })
            .when(self.app.show_context_menu, |this_div| {
                let menu_x = self.app.context_menu_position.unwrap_or_else(|| px(0.0));
                let menu_y = self.app.context_menu_position_y.unwrap_or_else(|| px(0.0));
                this_div.child(
                    div()
                        .absolute()
                        .inset_0()
                        .flex()
                        .items_start()
                        .justify_start()
                        .bg(gpui::rgba(0x80000000))
                        .child(
                            div()
                                .absolute()
                                .left(menu_x)
                                .top(menu_y)
                                .bg(theme.background)
                                .rounded_md()
                                .shadow_lg()
                                .w_48()
                                .flex()
                                .flex_col()
                                .child(
                                    div()
                                        .px_4()
                                        .py_3()
                                        .text_sm()
                                        .text_color(theme.foreground)
                                        .cursor_pointer()
                                        .hover(|style| style.bg(theme.border))
                                        .child("编辑连接")
                                        .on_mouse_down(MouseButton::Left, cx.listener(|app: &mut NetLiteApp, _event: &MouseDownEvent, window: &mut Window, cx: &mut Context<NetLiteApp>| {
                                            if let Some(connection_id) = app.context_menu_connection.clone() {
                                                app.show_context_menu = false;
                                                app.context_menu_connection = None;
                                                app.context_menu_position = None;
                                                app.context_menu_position_y = None;
                                                app.open_edit_connection(connection_id, window, cx);
                                            }
                                        })),
                                )
                                .child(
                                    div()
                                        .px_4()
                                        .py_3()
                                        .text_sm()
                                        .text_color(gpui::rgb(0xef4444))
                                        .cursor_pointer()
                                        .hover(|style| style.bg(gpui::rgb(0xfef2f2)))
                                        .child("删除连接")
                                        .on_mouse_down(MouseButton::Left, cx.listener(|app: &mut NetLiteApp, _event: &MouseDownEvent, _window: &mut Window, cx: &mut Context<NetLiteApp>| {
                                            if let Some(connection_name) = app.context_menu_connection.clone() {
                                                let is_client = app.context_menu_is_client;
                                                let tab_id = connection_name.clone();
                                                app.close_tab(tab_id, cx);
                                                if is_client {
                                                    app.storage.remove_client_connection(&connection_name);
                                                } else {
                                                    app.storage.remove_server_connection(&connection_name);
                                                }
                                            }
                                            app.show_context_menu = false;
                                            app.context_menu_connection = None;
                                            app.context_menu_position = None;
                                            app.context_menu_position_y = None;
                                            cx.notify();
                                        })),
                                ),
                        )
                        .on_mouse_down(MouseButton::Left, cx.listener(|app: &mut NetLiteApp, _event: &MouseDownEvent, _window: &mut Window, cx: &mut Context<NetLiteApp>| {
                            app.show_context_menu = false;
                            app.context_menu_connection = None;
                            app.context_menu_position = None;
                            app.context_menu_position_y = None;
                            cx.notify();
                        })),
                )
            })
    }
}
