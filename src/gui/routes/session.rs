use gpui::{App, Entity, Window, div, prelude::*, px};

use crate::component::prompt::Prompt;

/// A tab owns its chat state for the lifetime of the tab.
pub(crate) struct Chat {
    prompt: Entity<Prompt>,
}

impl Chat {
    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
        Self {
            prompt: cx.new(Prompt::new),
        }
    }

    pub(crate) fn focus_prompt(&self, window: &mut Window, cx: &App) {
        self.prompt.read(cx).focus(window);
    }
}

impl Render for Chat {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .items_center()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .p(px(24.))
            .child(div().id("transcript").flex_1().min_h_0().w_full())
            .child(div().w_full().max_w(px(760.)).child(self.prompt.clone()))
    }
}
