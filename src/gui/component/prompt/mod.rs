mod input;
mod layout;
mod models;
mod send_button;

use std::ops::Range;

use gpui::{
    BoxShadow, CursorStyle, Div, EventEmitter, FocusHandle, MouseButton, Stateful, Window, div,
    point, prelude::*, px, rgb, rgba,
};

use layout::{Editor, LINE_HEIGHT, Layout};
use models::Draft;

use crate::{
    keymap::RunCommand,
    theme,
    ui::{
        model_picker::ModelPicker,
        slash_menu::{Command, Menu},
    },
};

pub(crate) struct Prompt {
    focus: FocusHandle,
    text: String,
    anchor: usize,
    caret: usize,
    marked: Option<Range<usize>>,
    layout: Option<Layout>,
    selecting: bool,
    busy: bool,
    menu: Menu,
    model_picker: ModelPicker,
    model_draft: Option<Draft>,
    field: Option<Field>,
}

struct Field {
    placeholder: &'static str,
    masked: bool,
}

pub(crate) struct Submitted(pub(crate) String);

impl EventEmitter<Submitted> for Prompt {}

impl Prompt {
    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
        Self {
            focus: cx.focus_handle(),
            text: String::new(),
            anchor: 0,
            caret: 0,
            marked: None,
            layout: None,
            selecting: false,
            busy: false,
            menu: Menu::default(),
            model_picker: ModelPicker::default(),
            model_draft: None,
            field: None,
        }
    }

    pub(crate) fn field(
        placeholder: &'static str,
        text: String,
        masked: bool,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut input = Self::new(cx);
        input.field = Some(Field {
            placeholder,
            masked,
        });
        input.text = text;
        input.caret = input.text.len();
        input.anchor = input.caret;
        input
    }

    pub(crate) fn value(&self) -> &str {
        &self.text
    }

    pub(crate) fn select_all(&mut self) {
        self.anchor = 0;
        self.caret = self.text.len();
    }

    pub(crate) fn clear_field(&mut self, cx: &mut Context<Self>) {
        self.text.clear();
        self.reset_editing();
        cx.notify();
    }

    fn masked(&self) -> bool {
        self.field.as_ref().is_some_and(|field| field.masked)
    }

    fn single_line(&self) -> bool {
        self.field.is_some() || self.model_draft.is_some()
    }

    pub(crate) fn focus(&self, window: &mut Window) {
        self.focus.focus(window);
    }

    pub(crate) fn set_busy(&mut self, busy: bool, cx: &mut Context<Self>) {
        self.busy = busy;
        cx.notify();
    }

    pub(crate) fn toggle_commands(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.focus(window);
        if !self.close_models() {
            self.menu.toggle(&self.text);
        }
        cx.notify();
    }

    pub(crate) fn dismiss_commands(&mut self, cx: &mut Context<Self>) -> bool {
        let dismissed = self.close_models() || self.menu.dismiss(&self.text);
        if dismissed {
            cx.notify();
        }
        dismissed
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.marked.is_some() {
            return;
        }

        if self.field.is_some() {
            cx.emit(Submitted(self.text.clone()));
            return;
        }

        if self.model_draft.is_some() {
            if let Some(model) = self.model_picker.selected(&self.text).cloned() {
                self.pick_model(model, window, cx);
            }
            return;
        }

        if let Some((command, args)) = self.menu.submit(&self.text) {
            self.pick(command, args.to_owned(), window, cx);
            return;
        }
        if self.busy || self.text.trim().is_empty() {
            return;
        }

        let text = std::mem::take(&mut self.text);
        self.reset_editing();
        cx.emit(Submitted(text));
        cx.notify();
    }

    fn pick(
        &mut self,
        command: Command,
        args: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.menu.consumes_draft() {
            self.text.clear();
            self.reset_editing();
        }
        self.menu.dismiss(&self.text);
        window.dispatch_action(Box::new(RunCommand(command, args)), cx);
        cx.notify();
    }

    fn reset_editing(&mut self) {
        self.anchor = 0;
        self.caret = 0;
        self.marked = None;
        self.layout = None;
        self.selecting = false;
        self.menu.input_changed();
    }

    fn selection(&self) -> Range<usize> {
        self.anchor.min(self.caret)..self.anchor.max(self.caret)
    }

    fn current_layout(&self) -> Option<&Layout> {
        self.layout.as_ref().filter(|layout| {
            if self.masked() {
                layout.text.len() == self.text.len()
            } else {
                layout.text.as_ref() == self.text
            }
        })
    }

    fn render_input(&self, id: &'static str, cx: &mut Context<Self>) -> Stateful<Div> {
        div()
            .id(id)
            .track_focus(&self.focus)
            .cursor(CursorStyle::IBeam)
            .flex()
            .flex_col()
            .text_size(px(15.))
            .text_color(rgb(theme::TEXT))
            .on_key_down(cx.listener(Self::key_down))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_move(cx.listener(Self::mouse_move))
            .on_scroll_wheel(cx.listener(Self::scroll))
            .child(Editor(cx.entity()))
    }

    fn render_composer(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let searching = self.model_draft.is_some();
        let focused = !searching && self.focus.is_focused(window);
        let can_send = !searching
            && (!self.busy || self.menu.submit(&self.text).is_some())
            && !self.text.trim().is_empty();
        let vertical_padding = px(28.);
        let min_input_height = LINE_HEIGHT + vertical_padding * 2. + px(2.);
        let frame_top = px(28.);

        let input = if let Some(draft) = &self.model_draft {
            self.render_draft(draft, cx)
        } else {
            self.render_input("prompt", cx)
        };
        let input = input
            .relative()
            .w_full()
            .min_h(min_input_height)
            .min_w_0()
            .px(px(16.))
            .pr(px(if can_send { 64. } else { 16. }))
            .py(vertical_padding)
            .rounded(px(16.))
            .border_1()
            .border_color(rgba(if focused { 0xffffff22 } else { 0xffffff16 }))
            .bg(rgb(theme::SURFACE))
            .shadow(vec![BoxShadow {
                color: rgba(0x00000033).into(),
                offset: point(px(0.), px(2.)),
                blur_radius: px(8.),
                spread_radius: px(0.),
            }])
            .when(can_send, |input| {
                input.child(div().absolute().right(px(16.)).bottom(px(21.)).child(
                    send_button::render(
                        cx.listener(|prompt, _, window, cx| prompt.submit(window, cx)),
                    ),
                ))
            });

        div()
            .relative()
            .flex()
            .flex_col()
            .min_h(min_input_height + frame_top)
            .w_full()
            .pt(frame_top)
            .child(
                div()
                    .absolute()
                    .top_0()
                    .left(px(16.))
                    .right(px(16.))
                    .h(px(52.))
                    .rounded(px(16.))
                    .border_1()
                    .border_color(rgb(theme::BORDER))
                    .bg(rgb(theme::CHROME)),
            )
            .child(input)
    }
}

impl Render for Prompt {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(field) = &self.field {
            return self
                .render_input("route-input", cx)
                .min_w_0()
                .relative()
                .px(px(12.))
                .py(px(8.))
                .rounded(px(8.))
                .bg(rgb(theme::SURFACE))
                .border_1()
                .border_color(rgb(if self.focus.is_focused(window) {
                    theme::FOCUS
                } else {
                    theme::BORDER
                }))
                .when(self.text.is_empty(), |input| {
                    input.child(
                        div()
                            .absolute()
                            .top(px(8.))
                            .left(px(13.))
                            .text_size(px(14.))
                            .line_height(LINE_HEIGHT)
                            .text_color(rgb(theme::MUTED))
                            .child(field.placeholder),
                    )
                })
                .into_any_element();
        }
        let searching = self.model_draft.is_some();
        div()
            .relative()
            .flex()
            .flex_col()
            .min_h_0()
            .min_w_0()
            .when(!searching, |prompt| {
                let view = cx.entity().downgrade();
                prompt.children(self.menu.render(
                    &self.text,
                    (window.viewport_size().height - px(218.)).min(px(294.)),
                    move |command, _, window, cx| {
                        let _ = view.update(cx, |prompt, cx| {
                            let args = prompt
                                .menu
                                .submit(&prompt.text)
                                .filter(|(typed, _)| *typed == command)
                                .map_or("", |(_, args)| args)
                                .to_owned();
                            prompt.pick(command, args, window, cx);
                        });
                    },
                ))
            })
            .child(self.render_composer(window, cx))
            .when(searching, |prompt| {
                prompt.child(self.render_models(window, cx))
            })
            .into_any_element()
    }
}
