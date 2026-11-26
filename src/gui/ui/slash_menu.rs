use gpui::{App, ClickEvent, FontWeight, Pixels, ScrollHandle, Window, div, prelude::*, px, rgb};

use crate::{keymap, theme};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Command {
    New,
    Close,
    Stop,
    Models,
    Sidebar,
    RightPanel,
}

const COMMANDS: &[Command] = &[
    Command::New,
    Command::Close,
    Command::Stop,
    Command::Models,
    Command::Sidebar,
    Command::RightPanel,
];

impl Command {
    fn info(self) -> (&'static str, &'static str, &'static str) {
        match self {
            Self::New => (
                "new",
                "Open a new chat",
                if cfg!(target_os = "macos") {
                    "⌘T"
                } else {
                    "Ctrl+T"
                },
            ),
            Self::Close => (
                "close",
                "Close this chat tab",
                if cfg!(target_os = "macos") {
                    "⌘W"
                } else {
                    "Ctrl+W"
                },
            ),
            Self::Stop => ("stop", "Interrupt the current turn", "Esc"),
            Self::Models => ("models", "Browse and choose a model for this chat", ""),
            Self::Sidebar => ("sidebar", "Toggle the left panel", "Ctrl+B"),
            Self::RightPanel => ("right-panel", "Toggle the session panel", "Ctrl+P"),
        }
    }

    pub(crate) fn exact(text: &str) -> Option<Self> {
        let name = text.trim().strip_prefix('/')?;
        COMMANDS
            .iter()
            .copied()
            .find(|command| command.info().0 == name)
    }

    pub(crate) fn dispatch(self, window: &mut Window, cx: &mut App) {
        match self {
            Self::New => window.dispatch_action(Box::new(keymap::NewTab), cx),
            Self::Close => window.dispatch_action(Box::new(keymap::CloseTab), cx),
            Self::Stop => window.dispatch_action(Box::new(keymap::StopTurn), cx),
            Self::Sidebar => window.dispatch_action(Box::new(keymap::ToggleSidebar), cx),
            Self::RightPanel => window.dispatch_action(Box::new(keymap::ToggleRightPanel), cx),
            Self::Models => window.dispatch_action(Box::new(keymap::ChooseModel), cx),
        }
    }
}

#[derive(Default)]
pub(crate) struct Menu {
    forced: bool,
    dismissed: bool,
    selected: usize,
    scroll: ScrollHandle,
}

impl Menu {
    pub(crate) fn input_changed(&mut self) {
        self.forced = false;
        self.dismissed = false;
        self.selected = 0;
        self.scroll.scroll_to_item(0);
    }

    pub(crate) fn toggle(&mut self, text: &str) {
        if !self.dismiss(text) {
            self.input_changed();
            self.forced = true;
        }
    }

    pub(crate) fn dismiss(&mut self, text: &str) -> bool {
        let open = !self.options(text).is_empty();
        if open {
            self.dismissed = true;
            self.forced = false;
        }
        open
    }

    pub(crate) fn options(&self, text: &str) -> Vec<Command> {
        if self.dismissed {
            return Vec::new();
        }
        let query = if self.forced {
            ""
        } else if let Some(query) = text.trim_start().strip_prefix('/') {
            query
        } else {
            return Vec::new();
        };
        COMMANDS
            .iter()
            .copied()
            .filter(|command| command.info().0.starts_with(query))
            .collect()
    }

    pub(crate) fn selected(&self, text: &str) -> Option<Command> {
        self.options(text).get(self.selected).copied()
    }

    pub(crate) fn move_selection(&mut self, text: &str, down: bool) -> bool {
        let count = self.options(text).len();
        if count == 0 {
            return false;
        }
        self.selected = (self.selected + if down { 1 } else { count - 1 }) % count;
        self.scroll.scroll_to_item(self.selected);
        true
    }

    pub(crate) fn render<F>(
        &self,
        text: &str,
        height: Pixels,
        on_pick: F,
    ) -> Option<impl IntoElement>
    where
        F: Fn(Command, &ClickEvent, &mut Window, &mut App) + Clone + 'static,
    {
        let options = self.options(text);
        if options.is_empty() {
            return None;
        }

        Some(
            div()
                .id("slash-menu")
                .flex()
                .flex_col()
                .min_h_0()
                .w_full()
                .mb(px(8.))
                .rounded(px(14.))
                .border_1()
                .border_color(rgb(theme::HOVER))
                .bg(rgb(theme::CHROME))
                .shadow_md()
                .block_mouse_except_scroll()
                .child(
                    div()
                        .id("command-options")
                        .min_h_0()
                        .max_h(height)
                        .p(px(5.))
                        .overflow_y_scroll()
                        .track_scroll(&self.scroll)
                        .children(options.into_iter().enumerate().map(|(index, command)| {
                            let (name, description, shortcut) = command.info();
                            let on_pick = on_pick.clone();
                            div()
                                .id(("slash-command", index))
                                .flex()
                                .items_center()
                                .gap(px(10.))
                                .px(px(10.))
                                .py(px(6.))
                                .rounded(px(8.))
                                .cursor_pointer()
                                .when(index == self.selected, |row| row.bg(rgb(theme::ACTIVE)))
                                .hover(|style| style.bg(rgb(theme::HOVER)))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .child(
                                            div()
                                                .text_size(px(14.))
                                                .line_height(px(19.))
                                                .font_weight(FontWeight::SEMIBOLD)
                                                .text_color(rgb(theme::TEXT))
                                                .child(format!("/{name}")),
                                        )
                                        .child(
                                            div()
                                                .text_size(px(12.))
                                                .line_height(px(16.))
                                                .text_color(rgb(theme::MUTED))
                                                .child(description),
                                        ),
                                )
                                .when(!shortcut.is_empty(), |row| {
                                    row.child(
                                        div()
                                            .flex_shrink_0()
                                            .px(px(6.))
                                            .py(px(2.))
                                            .rounded(px(5.))
                                            .border_1()
                                            .border_color(rgb(theme::FOCUS))
                                            .text_size(px(11.))
                                            .text_color(rgb(theme::TEXT))
                                            .child(shortcut),
                                    )
                                })
                                .on_click(move |event, window, cx| {
                                    on_pick(command, event, window, cx)
                                })
                        })),
                ),
        )
    }
}
