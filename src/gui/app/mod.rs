mod render;

use gpui::{
    App, AppContext as _, Context, Entity, EntityId, KeyBinding, ScrollHandle, Window, actions,
};

use crate::routes::session::Chat;

actions!(desktop, [NewTab, CloseTab, NextTab, PreviousTab]);

pub(crate) fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("secondary-t", NewTab, Some("Desktop")),
        KeyBinding::new("secondary-w", CloseTab, Some("Desktop")),
        KeyBinding::new("ctrl-tab", NextTab, Some("Desktop")),
        KeyBinding::new("ctrl-shift-tab", PreviousTab, Some("Desktop")),
    ]);
}

pub(crate) struct Desktop {
    chats: Vec<Entity<Chat>>,
    active: usize,
    tab_scroll: ScrollHandle,
}

impl Desktop {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let chat = cx.new(Chat::new);
        chat.read(cx).focus_prompt(window, cx);
        Self {
            chats: vec![chat],
            active: 0,
            tab_scroll: ScrollHandle::new(),
        }
    }

    fn open_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.chats.push(cx.new(Chat::new));
        self.activate(self.chats.len() - 1, window, cx);
    }

    fn activate(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.active = index;
        self.tab_scroll.scroll_to_item(index);
        self.chats[index].read(cx).focus_prompt(window, cx);
        cx.notify();
    }

    fn chat_index(&self, id: EntityId) -> Option<usize> {
        self.chats.iter().position(|chat| chat.entity_id() == id)
    }

    fn close_chat(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.chats.remove(index);
        if self.chats.is_empty() {
            self.open_chat(window, cx);
        } else if index == self.active {
            self.activate(index.min(self.chats.len() - 1), window, cx);
        } else {
            if index < self.active {
                self.active -= 1;
            }
            cx.notify();
        }
    }
}
