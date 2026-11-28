use std::path::{Path, PathBuf};

use gpui::{Context, Window};
use morpheus::{
    assembly::agent_runtime,
    git::worktree::{self, AssignedBy, Binding, WorktreeCommand, parse_worktree_command},
    setup::Setup,
};

use super::Desktop;
use crate::ui::command_dialog::CommandDialog;

impl Desktop {
    pub(super) fn run_worktree(&mut self, args: &str, window: &mut Window, cx: &mut Context<Self>) {
        let chat = self.chats[self.active].clone();
        let command = if args.is_empty() {
            Some(WorktreeCommand::List)
        } else {
            parse_worktree_command(Some(args))
        };
        match command {
            Some(WorktreeCommand::List) => self.list_worktrees(window, cx),
            Some(_) if chat.read(cx).is_busy() => {
                chat.update(cx, |chat, cx| {
                    chat.notice("Change worktrees between turns.", cx)
                });
            }
            Some(WorktreeCommand::Exit) => {
                chat.update(cx, |chat, cx| {
                    if chat.worktree().is_some() {
                        chat.set_worktree(None, &Setup::load(), &self.config);
                        chat.notice(format!("Working directory restored to {}.", chat.cwd()), cx);
                    } else {
                        chat.notice("This chat already uses its original directory.", cx);
                    }
                });
            }
            Some(WorktreeCommand::Add { path, branch }) => {
                self.attach_worktree(path.map(PathBuf::from), branch, cx);
            }
            None => chat.update(cx, |chat, cx| {
                chat.notice(
                    "Use /worktree list, /worktree add [path] <branch>, or /worktree exit.",
                    cx,
                );
            }),
        }
    }

    fn list_worktrees(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let cwd = PathBuf::from(self.chats[self.active].read(cx).cwd());
        let repo = PathBuf::from(&self.config.cwd);
        let dialog = self
            .show_dialog(window, cx, CommandDialog::worktrees)
            .downgrade();
        let worker = agent_runtime().spawn_blocking(move || {
            worktree::list(&repo).map(|entries| {
                entries
                    .into_iter()
                    .map(|entry| {
                        let branch = entry.branch.as_deref().unwrap_or("Detached HEAD");
                        let current = if worktree::same_path(&cwd, &entry.path) {
                            " · Current"
                        } else {
                            ""
                        };
                        format!("{branch}{current}\n{}", entry.path.display())
                    })
                    .collect::<Vec<_>>()
            })
        });
        cx.spawn(async move |_, cx| {
            let rows = match worker.await {
                Ok(Ok(rows)) if !rows.is_empty() => rows,
                Ok(Ok(_)) => vec!["Git reported no worktrees.".into()],
                Ok(Err(error)) => vec![format!("Could not list worktrees. {error}")],
                Err(_) => vec!["Could not list worktrees.".into()],
            };
            let _ = dialog.update(cx, |dialog, cx| dialog.show_worktrees(rows, cx));
        })
        .detach();
    }

    fn attach_worktree(&mut self, path: Option<PathBuf>, branch: String, cx: &mut Context<Self>) {
        let repo = PathBuf::from(&self.config.cwd);
        let owner = &self.chats[self.active];
        owner.update(cx, |chat, cx| chat.set_worktree_busy(true, cx));
        let owner = owner.downgrade();
        let worker = agent_runtime().spawn_blocking(move || {
            worktree::create_or_adopt(&repo, &branch, path.as_deref(), AssignedBy::User)
        });
        cx.spawn(async move |desktop, cx| {
            let result = worker.await;
            let _ = desktop.update(cx, |desktop, cx| {
                let Some(owner) = owner.upgrade() else {
                    return;
                };
                let config = &desktop.config;
                let message = match result {
                    Ok(Ok((worktree, _)))
                        if desktop.chats.iter().any(|chat| {
                            chat != &owner
                                && worktree::same_path(
                                    Path::new(chat.read(cx).cwd()),
                                    &worktree.path,
                                )
                        }) =>
                    {
                        format!("Another chat is already using {}.", worktree.path.display())
                    }
                    Ok(Ok((worktree, binding))) => {
                        let action = match binding {
                            Binding::Created => "Created worktree",
                            Binding::Adopted => "Attached to worktree",
                        };
                        let message =
                            format!("{action} {} · {}", worktree.branch, worktree.path.display());
                        owner.update(cx, |chat, _| {
                            chat.set_worktree(Some(worktree), &Setup::load(), config)
                        });
                        message
                    }
                    Ok(Err(error)) => format!("Could not open the worktree. {error}"),
                    Err(_) => "Could not open the worktree.".into(),
                };
                owner.update(cx, |chat, cx| {
                    chat.set_worktree_busy(false, cx);
                    chat.notice(message, cx);
                });
            });
        })
        .detach();
    }
}
