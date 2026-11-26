mod render;

use std::sync::Arc;

use gpui::{AppContext as _, Context, Entity, EntityId, ScrollHandle, Window};
use morpheus::assembly::config::AgentConfig;

use crate::routes::session::Chat;

pub(crate) struct Desktop {
    config: Arc<AgentConfig>,
    chats: Vec<Entity<Chat>>,
    active: usize,
    tab_scroll: ScrollHandle,
    sidebar_visible: bool,
}

impl Desktop {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let cwd = std::env::current_dir().unwrap_or_else(|_| ".".into());
        let config = Arc::new(AgentConfig::load(&cwd.to_string_lossy()));
        let chat = cx.new(|cx| Chat::new(config.clone(), cx));
        chat.read(cx).focus_prompt(window, cx);
        Self {
            config,
            chats: vec![chat],
            active: 0,
            tab_scroll: ScrollHandle::new(),
            sidebar_visible: true,
        }
    }

    fn open_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.chats
            .push(cx.new(|cx| Chat::new(self.config.clone(), cx)));
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
