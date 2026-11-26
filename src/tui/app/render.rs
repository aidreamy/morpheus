use crate::ui::dialogs::DialogType;
use std::time::Instant;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;

use super::{
    App, AppMode, BACK_LINK_TEXT, EMPTY_SESSION_PROMPT_MIN_WIDTH, EMPTY_SESSION_PROMPT_RATIO,
    MIN_PROMPT_RESERVE_ROWS, SessionArea,
};
use crate::component::budget::{budget_bar_style, budget_block, budget_pct_style};
use crate::component::prompt::PanePromptInfo;
use crate::left_panel::{MIN_WIDTH_FOR_LEFT_PANEL, Mode};
use crate::routes::session::agents_split::{MANAGER_LABEL, brand_name_styles};
use crate::routes::session::footer::FooterView;
use crate::routes::session::right_panel::render_right_panel;
use crate::state::PendingQueues;
use crate::theme::rgba_color;
use morpheus_agents::palette::display_name;

/// Re-exported so the geometry, hit-testing and tests keep naming the row
/// type through this module (the renderer itself lives in the shared
/// [`crate::routes::session::pending_queue`] component, which the chat and
/// every `/agents` pane paint through).
pub(crate) use crate::routes::session::pending_queue::PendingQueueRow;

impl App {
    pub(super) fn render(&mut self, frame: &mut Frame<'_>, delta_time: f64) {
        self.telemetry_view();
        // The draw loop can run at 60 fps, but frame-counted animations were
        // designed for 30 fps. Advance them by elapsed time so they keep the
        // same visual speed at either pacing rate.
        self.animation_tick_remainder =
            (self.animation_tick_remainder + delta_time.max(0.0)).min(4.0 / 30.0);
        let animation_ticks = (self.animation_tick_remainder * 30.0) as u32;
        self.animation_tick_remainder -= f64::from(animation_ticks) / 30.0;
        // TODO completion strike-through: advance the per-item frame counters
        // (holds 2 frames, sweeps left-to-right over 12). Trailing frames are
        // consumed too — while a sweep runs the panel redraws every tick; when
        // the last counter passes its total the counter is dropped and the
        // strike becomes permanent. Not needed to drive the visuals when the
        // tick count is 0 (nothing changed), but kept unconditional so the
        // counters can never get behind the loop's own clock.
        self.state.right_panel.advance_todo_strikes(animation_ticks);
        // Sync live_requested — keeps the render loop running smoothly.
        // Session: during streaming, sticky scroll needs continuous re-rendering.
        // RAG: when the spinner is active (fetching/embedding), enable live mode
        //      so event::poll uses 8ms instead of 50ms, keeping animation smooth.
        // Tool spinners: keep live mode while any tool spinner is active/finishing
        // so the beam sweep animation advances every frame, even when the session
        // is idle (no streaming).
        let has_active_spinner = self
            .session_view
            .tool_state
            .tool_spinners
            .iter()
            .any(|(_, s)| !s.is_idle());
        // The /agents panes' tool-call spinners keep live mode too (the
        // right panel's `subagent_tool_spinners` feed the same predicate
        // through the working-session checks): without this a running
        // pane's sweep would freeze mid-label until the next event.
        let has_active_pane_spinner = self
            .agents_split
            .as_ref()
            .is_some_and(|split| split.tool_spinners.values().any(|s| !s.is_idle()));
        let mut live = self.session_view.is_auto_scrolling
            || self.state.right_panel.is_auto_scrolling
            || (self.state.status == crate::types::SessionStatus::Working
                && self.session_view.is_sticky_bottom);
        // Keep the one-off correction spinner animated whenever the agent
        // loop is not already using that shared row above the prompt.
        // The agent spinner itself also holds live frames: a Working session
        // with the user scrolled away from the bottom (not sticky), no
        // streaming text and no active tool spinner otherwise reached the
        // idle path, dropping the beam animation to ~5 fps.
        let agent_spinner_live = (self.state.status == crate::types::SessionStatus::Working
            || self
                .agents_split
                .as_ref()
                .is_some_and(crate::routes::session::agents_split::AgentsSplitState::any_running)
            || (self.agents_split.is_none()
                && self.parked_agents_split.as_ref().is_some_and(
                    crate::routes::session::agents_split::AgentsSplitState::any_running,
                )))
            && self.agent_spinner.is_some();
        live = live || agent_spinner_live;
        live = live || (self.prompt_correction_active && !agent_spinner_live);
        live = live || self.rag_spinner_active();
        live = live || has_active_spinner;
        live = live || has_active_pane_spinner;
        // Free-gateway recommendation: its fake-streaming text advances per
        // rendered frame (wall-clock-compensated, so no chars are lost, but a
        // 200 ms idle gap made it visibly chunky).
        live = live || self.free_gateway_dialog.visible;
        // TODO completion strikes: while any strike animation is running the
        // panel must redraw every frame, even in an idle session — otherwise
        // the sweep would freeze mid-label until the next unrelated event.
        live = live || self.state.right_panel.todo_strike_animating();
        // A running compaction stopwatch must tick every frame.
        live = live || self.has_running_compaction();
        // The scroll-to-bottom pill's slide/fade must finish even with no
        // other live source (idle session, mid-history scroll): otherwise the
        // animation would freeze on a half-drawn frame until the next event.
        live = live || self.session_view.pill_animating();
        live = live
            || self
                .agents_split
                .as_ref()
                .is_some_and(|split| split.pill_animating());
        // A speaking voice drives the equalizer every frame (and a pending
        // synthesis must be polled); without this the bars would freeze
        // between unrelated input events.
        live = live || self.speech.is_animating();
        self.live_requested = live;
        let area = frame.area();

        {
            let buf = frame.buffer_mut();

            let bg_color = rgba_color(self.theme.background);
            let _bg_start = Instant::now();
            // Background fill via a flat style patch: `Terminal::swap_buffers`
            // resets the draw buffer to `Cell::EMPTY` (symbol " ") before every
            // frame, so the symbol loop below the old implementation was
            // redundant work — only the style channels need writing. One
            // `Style` merge over the backing `Vec<Cell>` per row is an order
            // of magnitude cheaper than two virtual calls per cell and keeps
            // the frame pacer comfortably in its 60 fps tier.
            let empty_style = Style::default().bg(bg_color);
            let buf_w = buf.area.width as usize;
            let start_row = area.y as usize;
            let end_row = area.bottom() as usize;
            let col_start = area.x as usize;
            let col_end = area.right() as usize;
            for row in start_row..end_row {
                let base = row * buf_w;
                for cell in &mut buf.content[base + col_start..base + col_end] {
                    cell.set_style(empty_style);
                }
            }
            let _bg_us = _bg_start.elapsed().as_micros();
            // Rate-limited: the fill always exceeds 200us, so without this the
            // log would receive one line per frame for the whole session
            // lifetime (a 400+ MB stdout.log in 8 hours).
            self.perf_frame = self.perf_frame.wrapping_add(1);
            if _bg_us > 200 && self.perf_frame.is_multiple_of(60) {
                log::debug!(
                    "[PERF] bg_fill: {_bg_us}us area={}x{}",
                    area.width,
                    area.height
                );
            }

            let title_chars: Vec<char> = BACK_LINK_TEXT.chars().collect();
            let title_w = title_chars.len() as u16;
            let back_link_area = (area.width > 1).then(|| {
                Rect::new(
                    area.x + 1,
                    area.y,
                    title_w.min(area.width.saturating_sub(1)),
                    1,
                )
            });
            let back_link_fg = if let Some(link_area) = back_link_area
                && self.last_mouse_x >= link_area.x
                && self.last_mouse_x < link_area.right()
                && self.last_mouse_y >= link_area.y
                && self.last_mouse_y < link_area.bottom()
            {
                rgba_color(self.theme.text)
            } else {
                rgba_color(self.theme.text_muted)
            };
            let header_style = Style::default().fg(back_link_fg);
            for (i, ch) in title_chars.iter().enumerate() {
                if let Some(cell) = buf.cell_mut((area.x + 1 + i as u16, area.y)) {
                    cell.set_char(*ch);
                    cell.set_style(header_style);
                }
            }
            self.back_link_area = back_link_area;

            let SessionArea {
                main: main_area,
                sidebar_w,
                right_panel_w,
            } = self.session_main_area(area);

            // The ASCII wallpaper is the BASE layer: painted right after the
            // background fill, with no background style of its own, so every
            // pane, dialog and the transcript paint over it and keep their
            // own fill. The header row and the footer row stay clear of it.
            if matches!(self.mode(), AppMode::Session)
                && let Some(wallpaper) = &mut self.wallpaper
            {
                let art_area = Rect::new(
                    main_area.x,
                    area.y.saturating_add(1),
                    main_area.width,
                    main_area.height.saturating_sub(2),
                );
                let ink = rgba_color(self.theme.session_logo);
                wallpaper.render(buf, art_area, ink);
            }

            // Context info bar — only after the user has sent at least one
            // message. Shows the agent's current context against the model
            // window to the left of the budget bar, with thousands separators
            // so large token counts are readable at a glance.
            let has_content = self
                .state
                .current_session()
                .is_some_and(|s| !s.messages.is_empty());
            if matches!(self.mode(), AppMode::Session) && has_content {
                let tokens = self
                    .context_info
                    .as_ref()
                    .map_or(0, |info| info.total_tokens);
                let max = self.context_info.as_ref().map_or(0, |info| info.max_tokens);

                // "9,612 / 100,000 tok" + bar + pct, composed in
                // `component::budget` (the pane headers paint the same
                // pair).
                //
                // Variant selection is EXACT, not a guessed threshold: the
                // whole right cluster (cost + budget) must fit between the
                // title's end and the right margin with one separator cell
                // to spare, i.e.
                //   cost + gap + block ≤ main.right − max(title_end, main.x) − 2
                // Sizing only the budget part let a wide cost push the
                // cluster over the back button; counting the cost flips to
                // the compact `9.6k/100k 9%` exactly one column before the
                // collision. A screen that fits not even the compact
                // cluster paints nothing (the pane headers hide their
                // budget the same way).
                //
                // Session cost — for USD-billed providers the session's
                // dollar total (green). For Charm Hyper the header shows the
                // REMAINING Hypercredit balance instead: credits are not
                // money, and the balance is the figure the user can verify
                // against their account — it DECREASES as they spend (same
                // number Crush shows). Only the ◆ glyph is pink (#FF60FF);
                // the figure stays in the normal text color. Hidden while
                // nothing is known yet (no guessed $0 / ◆0).
                //
                // Gated on the DATA, not a provider-name string: ONLY Charm
                // Hyper reports `usage.remaining.hypercredits`, so a
                // `Some(balance)` is the authoritative signal. String-matching
                // the provider name would miss custom providers pointed at
                // the Charm gateway (their name is user-chosen, not "charm")
                // and the header would wrongly fall back to dollars.
                //
                // Computed BEFORE the variant selection: the cost's width is
                // part of the cluster the inequality sizes.
                let credits_mode = self.hypercredit_balance.is_some();
                let cost_str = if credits_mode {
                    self.hypercredit_balance.map(|b| format!("\u{25c6} {b:.0}"))
                } else {
                    self.session_cost().map(crate::usage::format_usd)
                };
                let cost_w = cost_str.as_ref().map_or(0, |s| s.chars().count()) as u16;
                // Fixed like the old constant (it doubled as the
                // counter↔bar gap then): the cost separator does not shrink
                // with the variant, so the selection math stays exact.
                let cost_gap: u16 = if cost_str.is_some() { 2 } else { 0 };

                let title_end = area.x.saturating_add(1).saturating_add(title_w);
                let cluster_left = title_end.max(main_area.x);
                let avail = main_area
                    .right()
                    .saturating_sub(cluster_left)
                    .saturating_sub(2)
                    .saturating_sub(cost_w + cost_gap);
                let block = budget_block(tokens as u64, max as u64, avail);
                // No variant fits: empty strings paint nothing below, and
                // the cost hides with the budget (nothing readable would
                // survive the overlap anyway).
                let cost_str = if block.is_some() { cost_str } else { None };
                let (token_str, budget_str, pct_str, pct, gap) = match block {
                    Some(block) => (
                        block.counter,
                        block.meter,
                        block.pct_text,
                        block.pct,
                        block.gap,
                    ),
                    None => (String::new(), String::new(), String::new(), 0, 0),
                };

                let display_w = (cost_w as usize
                    + cost_gap as usize
                    + (token_str.chars().count()
                        + gap as usize
                        + budget_str.chars().count()
                        + pct_str.chars().count())) as u16;
                let right_x = main_area.right().saturating_sub(display_w + 1);

                // Session cost — the leftmost element of the header line.
                if let Some(cost) = &cost_str {
                    if credits_mode {
                        // ◆ gets its OWN cell in Charm pink; the number is
                        // drawn after it with the plain default style.
                        let mut chars = cost.chars();
                        if let Some(diamond) = chars.next()
                            && let Some(cell) = buf.cell_mut((right_x, area.y))
                        {
                            cell.set_char(diamond);
                            cell.set_style(
                                Style::default().fg(ratatui::style::Color::Rgb(255, 96, 255)),
                            );
                        }
                        for (i, ch) in chars.enumerate() {
                            if let Some(cell) = buf.cell_mut((right_x + 1 + i as u16, area.y)) {
                                cell.set_char(ch);
                            }
                        }
                    } else {
                        let cost_style = Style::default().fg(rgba_color(self.theme.success));
                        for (i, ch) in cost.chars().enumerate() {
                            if let Some(cell) = buf.cell_mut((right_x + i as u16, area.y)) {
                                cell.set_char(ch);
                                cell.set_style(cost_style);
                            }
                        }
                    }
                }

                // Token counter — to the right of the cost (or left of the
                // budget bar when no price is known yet).
                let token_x = right_x + cost_w + cost_gap;
                for (i, ch) in token_str.chars().enumerate() {
                    if let Some(cell) = buf.cell_mut((token_x + i as u16, area.y)) {
                        cell.set_char(ch);
                        cell.set_style(Style::default().fg(rgba_color(self.theme.text_muted)));
                    }
                }

                // Budget bar (with conditional color) + bold percentage.
                let offset = cost_w + cost_gap + token_str.chars().count() as u16 + gap;
                let budget_style = budget_bar_style(pct, &self.theme);
                for (i, ch) in budget_str.chars().enumerate() {
                    if let Some(cell) = buf.cell_mut((right_x + offset + i as u16, area.y)) {
                        cell.set_char(ch);
                        cell.set_style(budget_style);
                    }
                }
                let pct_style = budget_pct_style(pct, &self.theme);
                let pct_offset = offset + budget_str.chars().count() as u16;
                for (i, ch) in pct_str.chars().enumerate() {
                    if let Some(cell) = buf.cell_mut((right_x + pct_offset + i as u16, area.y)) {
                        cell.set_char(ch);
                        cell.set_style(pct_style);
                    }
                }
            }

            // Session header sparkle — Astra-style starfield flourish, ported
            // from the Codex TUI (`chat_composer/sparkle.rs`). Session router
            // ONLY: the component arms per session id and paints exclusively
            // on blank cells of the header strip, so the "← esc" hint (and
            // the cost/token/budget info already drawn above) is never
            // covered. Other routers never reach this call.
            let is_session_mode = matches!(self.mode(), AppMode::Session);
            if is_session_mode {
                let header_strip = Rect::new(main_area.x, area.y, main_area.width, 1);
                let hint_w = BACK_LINK_TEXT.chars().count() as u16;
                let protected_hint = Rect::new(area.x + 1, area.y, hint_w, 1);
                let (fg_r, fg_g, fg_b, _) = self.theme.text.to_ints();
                // Night-window gate (see component/sparkle.rs): local
                // minutes since midnight, read here so tests can inject
                // arbitrary times into the component.
                use chrono::Timelike;
                let local_minutes =
                    (chrono::Local::now().time().num_seconds_from_midnight() / 60) as u16;
                self.sparkle.render(
                    crate::component::sparkle::SparkleFrame {
                        area: header_strip,
                        cursor: None,
                        protected: Some(protected_hint),
                        session_key: self.state.current_session_id.as_deref(),
                        in_session: true,
                        terminal_focused: self.terminal_focused,
                        foreground: (fg_r, fg_g, fg_b),
                        local_minutes,
                    },
                    buf,
                );
            }

            // Right panel (independent of left panel state)
            if right_panel_w > 0 {
                render_right_panel(
                    buf,
                    Rect::new(
                        area.right().saturating_sub(right_panel_w),
                        area.y,
                        right_panel_w,
                        area.height,
                    ),
                    &mut self.state.right_panel,
                    &self.theme,
                    area.width,
                );
                self.state.right_panel.handle_auto_scroll(delta_time);
            }

            if self.sidebar.open && area.width >= MIN_WIDTH_FOR_LEFT_PANEL {
                if matches!(self.left_panel, Mode::Explorer) {
                    // File explorer (Ctrl+F): the tree owns the whole panel,
                    // including the header box.
                    if let Some(explorer) = &mut self.file_explorer {
                        // Refresh the git + LSP status index first: while the
                        // explorer is hidden it stays untouched, and `refresh`
                        // is a no-op (two integer compares) when neither the
                        // diagnostics version nor the git revision moved.
                        if let Some(watcher) = self.explorer_git_watcher.as_ref() {
                            self.explorer_statuses
                                .refresh(&explorer.root.clone(), watcher);
                        }
                        explorer.render(
                            buf,
                            Rect::new(area.x, area.y, sidebar_w, area.height),
                            &self.theme,
                            &self.explorer_statuses,
                        );
                    }
                } else if matches!(self.left_panel, Mode::Dashboard) {
                    // Usage dashboard: a self-contained panel (session usage
                    // + spend per provider/total for the selected period).
                    let data = self.dashboard_data();
                    crate::left_panel::dashboard::render(
                        buf,
                        Rect::new(area.x, area.y, sidebar_w, area.height),
                        &data,
                        &self.theme,
                    );
                } else {
                    self.sidebar.render(
                        buf,
                        Rect::new(area.x, area.y, sidebar_w, area.height),
                        &self.state.session_summaries,
                        &self.theme,
                    );

                    // Hover tooltip: when the mouse is over a sessions-list
                    // item whose title was LLM-generated, show the full title
                    // as a toast.  Checked every frame so it works even when
                    // the terminal doesn't send Move events.
                    let mx = self.last_mouse_x;
                    let my = self.last_mouse_y;
                    if mx < sidebar_w
                        && let Some(idx) =
                            crate::left_panel::layout::SessionListLayout::item_index_at(
                                area,
                                self.sidebar.selection.scroll_offset,
                                my,
                            )
                        && let Some(summary) = self.state.session_summaries.get(idx)
                        && summary.title_generated
                        && self.state.status == crate::types::SessionStatus::Idle
                    {
                        self.toast_state
                            .show_tooltip(crate::ui::toast::ToastOptions {
                                title: None,
                                message: summary.title.clone(),
                                variant: crate::ui::toast::ToastVariant::Info,
                                duration_ms: 3000,
                            });
                    }
                }
            }

            let footer_y = main_area.bottom().saturating_sub(1);
            let is_session = matches!(self.mode(), AppMode::Session);

            // When question, permission, queue-choice, or gateway recommendation
            // dialog is visible, hide prompt and spinner (like OpenCode).
            let hide_prompt_and_spinner = is_session
                && (self.question_dialog.visible
                    || self.permission_dialog.visible
                    || self.queue_choice_dialog.visible
                    || self.free_gateway_dialog.visible);

            // Detect empty session — no messages yet (like OpenCode initial state).
            // The /agents split suppresses the landing treatment: the logo
            // band above the prompt would paint straight over the panes.
            let is_empty_session = is_session
                && !hide_prompt_and_spinner
                && self.agents_split.is_none()
                && self
                    .state
                    .current_session()
                    .is_none_or(|s| s.messages.is_empty());

            let full_w = main_area.width.saturating_sub(4);
            let (prompt_area_x, prompt_area_w) = if is_empty_session {
                let narrow = std::cmp::max(
                    EMPTY_SESSION_PROMPT_MIN_WIDTH,
                    (main_area.width as f64 * EMPTY_SESSION_PROMPT_RATIO) as u16,
                )
                .min(full_w);
                (main_area.x + (main_area.width - narrow) / 2, narrow)
            } else {
                (main_area.x + 2, full_w)
            };

            let prompt_h = if is_session && !hide_prompt_and_spinner {
                // Responsive vertical budget: everything between the header and
                // the footer, minus the rows always reserved for the session.
                let prompt_budget = footer_y
                    .saturating_sub(area.y + 1)
                    .saturating_sub(MIN_PROMPT_RESERVE_ROWS);
                self.prompt_view
                    .required_height(prompt_area_w, prompt_budget)
            } else {
                0
            };

            // Permission dialog (same position as question, mutually exclusive)
            let permission_h = if is_session && self.permission_dialog.visible {
                self.permission_dialog
                    .required_height(main_area.width.saturating_sub(4))
            } else {
                0
            };
            // Pending queued-message region (color-coded rows above the prompt).
            // Each queued message is ONE visual line, visually truncated to
            // the band's width (never word-wrapped). The layout runs ONCE per
            // frame here: renderer, height and mouse hit-testing share the
            // same rows.
            let pending_rows = if is_session && !hide_prompt_and_spinner {
                let pending_w = main_area.width.saturating_sub(4);
                self.state
                    .current_pending_queues()
                    .map(|q| Self::pending_queue_layout(q, pending_w))
            } else {
                None
            };
            // The layout already carries each message's own internal padding
            // (one pad row above and below its text, painted as part of the
            // band), so the strip height is simply the number of layout rows.
            // An empty strip reserves nothing, as before.
            let pending_h = pending_rows.as_ref().map_or(0, Vec::len) as u16;
            // Queue-choice dialog (same position as question/permission).
            let queue_choice_h = if is_session && self.queue_choice_dialog.visible {
                self.queue_choice_dialog
                    .required_height(main_area.width.saturating_sub(4))
            } else {
                0
            };
            // Free-gateway recommendation dialog (same position, Session
            // only — every trigger lives in a session).
            let recommendation_h =
                if self.gateway_dialog_route() && self.free_gateway_dialog.visible {
                    self.free_gateway_dialog
                        .required_height(main_area.width.saturating_sub(4))
                } else {
                    0
                };

            // Shared with the mouse hit-testing helpers (`App::prompt_vertical_layout`)
            // so clicks always land on the row that was actually drawn. An
            // empty session centers the prompt between the header and the
            // footer; a populated one stays bottom-anchored.
            let prompt_area_y =
                Self::prompt_vertical_layout(area.y, footer_y, prompt_h, is_empty_session);

            // Both spinners share the row above the prompt. The correction
            // indicator yields to the agent loop rather than replacing it.
            // The /agents split keeps the spinner alive while ANY pane
            // streams (its turns live outside the shared session loop, so
            // `status` alone cannot see them) — a PARKED split too (the
            // stream follows the panes, not the screen). This predicate
            // MUST match `agent_spinner_active` in the mouse layout
            // (`mod.rs`): a one-row drift here skews every hit-test below
            // the prompt for the whole parked turn.
            let agents_panes_running =
                self.agents_split.as_ref().is_some_and(
                    crate::routes::session::agents_split::AgentsSplitState::any_running,
                ) || (self.agents_split.is_none()
                    && self.parked_agents_split.as_ref().is_some_and(
                        crate::routes::session::agents_split::AgentsSplitState::any_running,
                    ));
            // A rehydrating pane streams its replayed history too — same
            // spinner-row contract, live or parked.
            let agents_panes_rehydrating = self.agents_split.as_ref().is_some_and(
                crate::routes::session::agents_split::AgentsSplitState::any_rehydrating,
            ) || (self.agents_split.is_none()
                && self.parked_agents_split.as_ref().is_some_and(
                    crate::routes::session::agents_split::AgentsSplitState::any_rehydrating,
                ));
            let agent_spinner_active = (self.state.status == crate::types::SessionStatus::Working
                || agents_panes_running
                || agents_panes_rehydrating)
                && self.agent_spinner.is_some();
            let correction_spinner_active = self.prompt_correction_active && !agent_spinner_active;
            let spinner_h = u16::from(
                is_session
                    && !hide_prompt_and_spinner
                    && (agent_spinner_active || correction_spinner_active),
            );

            // Long queued messages wrap into several rows; clamp the strip so
            // it never covers the header row (area.y + 1) or runs off-screen.
            let pending_h = pending_h.min(prompt_area_y.saturating_sub(area.y + 1));
            let pending_area_y = prompt_area_y.saturating_sub(pending_h);
            // The spinner sits ABOVE the pending queues when they are shown;
            // with no queues (pending_h = 0) it stays in exactly the same spot
            // (directly above the prompt).
            let spinner_area_y = pending_area_y.saturating_sub(spinner_h);
            let max_dialog_h = pending_area_y.saturating_sub(area.y + 1);
            let permission_h = permission_h.min(max_dialog_h);
            let queue_choice_h = queue_choice_h.min(max_dialog_h);
            let recommendation_h = recommendation_h.min(max_dialog_h);
            // Question dialog: shared geometry with the mouse router
            // (`App::question_dialog_area`) so clicks and the wheel always
            // agree with what is drawn. With the dialog visible the prompt,
            // pending queues and spinner are all hidden (pending_area_y ==
            // footer_y), which is exactly the layout that helper assumes.
            let question_area = if is_session && self.question_dialog.visible {
                self.question_dialog_area()
            } else {
                Rect::new(
                    main_area.x + 2,
                    pending_area_y,
                    main_area.width.saturating_sub(4),
                    0,
                )
            };
            let question_area_y = question_area.y;
            let permission_area_y = pending_area_y.saturating_sub(permission_h);
            let queue_choice_area_y = pending_area_y.saturating_sub(queue_choice_h);
            let recommendation_area_y = pending_area_y.saturating_sub(recommendation_h);
            let pending_area = Rect::new(
                main_area.x + 2,
                pending_area_y,
                main_area.width.saturating_sub(4),
                pending_h,
            );
            let queue_choice_area = Rect::new(
                main_area.x + 2,
                queue_choice_area_y,
                main_area.width.saturating_sub(4),
                queue_choice_h,
            );
            let prompt_padding: u16 = 1;
            // Session bottom is below whichever dialog is visible (mutually
            // exclusive, never both), below the pending region and below the
            // spinner (which sits above the queues when they are shown).
            let session_bottom = question_area_y
                .min(permission_area_y)
                .min(queue_choice_area_y)
                .min(recommendation_area_y)
                .min(pending_area_y)
                .min(spinner_area_y)
                .saturating_sub(prompt_padding);

            let prompt_area = Rect::new(prompt_area_x, prompt_area_y, prompt_area_w, prompt_h);
            let spinner_area = Rect::new(prompt_area_x, spinner_area_y, prompt_area_w, spinner_h);
            let permission_area = Rect::new(
                main_area.x + 2,
                permission_area_y,
                main_area.width.saturating_sub(4),
                permission_h,
            );
            let recommendation_area = Rect::new(
                main_area.x + 2,
                recommendation_area_y,
                main_area.width.saturating_sub(4),
                recommendation_h,
            );
            let session_area = Rect::new(
                main_area.x,
                area.y + 1,
                main_area.width,
                session_bottom.saturating_sub(area.y + 1),
            );

            // The pill only ever animates inside the session view. Leaving
            // for settings, router etc. mid-animation would freeze
            // `pill_progress` mid-flight while the mode-independent live gate
            // keeps redrawing, so park it whenever another route renders.
            if !matches!(self.mode(), AppMode::Session) {
                self.session_view.clear_pill();
            }
            match self.mode() {
                AppMode::InternalTools => {
                    self.prompt_view.blur();
                    let tools_area = Rect::new(
                        session_area.x,
                        session_area.y,
                        session_area.width,
                        session_area.height.saturating_sub(1),
                    );
                    self.internal_tools_view
                        .render(buf, tools_area, &self.theme);
                }
                AppMode::AddProvider => {
                    self.prompt_view.blur();
                    self.add_provider_view.search_bar.cursor.terminal_focused =
                        self.terminal_focused;
                    let tools_area = Rect::new(
                        session_area.x,
                        session_area.y,
                        session_area.width,
                        session_area.height.saturating_sub(1),
                    );
                    self.add_provider_view
                        .render(buf, tools_area, &self.theme, &self.setup);
                }
                AppMode::Settings => {
                    self.prompt_view.blur();
                    let settings_area = Rect::new(
                        session_area.x,
                        session_area.y,
                        session_area.width,
                        session_area.height.saturating_sub(1),
                    );
                    self.settings_view
                        .render(buf, settings_area, &self.theme, &self.setup);
                }
                AppMode::Router => {
                    self.prompt_view.blur();
                    // The router owns the header row too: its tab buttons
                    // sit on the same line as the "← esc" hint, to the right
                    // of it. Same bottom row as the other routes.
                    let router_area = Rect::new(
                        session_area.x,
                        area.y,
                        session_area.width,
                        session_area.height,
                    );
                    let all_models = self.collect_cached_models();
                    self.router_view.render(
                        buf,
                        router_area,
                        &self.theme,
                        &all_models,
                        std::time::SystemTime::now(),
                    );
                }
                #[cfg(feature = "embed")]
                AppMode::Rag => {
                    self.render_rag_view(buf, session_area);
                }
                AppMode::Session => {
                    // Blur prompt when a dialog is visible (like OpenCode)
                    if self.question_dialog.visible
                        || self.permission_dialog.visible
                        || self.queue_choice_dialog.visible
                        || self.free_gateway_dialog.visible
                    {
                        self.prompt_view.blur();
                    }

                    // Advance the fake streaming text in the recommendation dialog
                    if self.free_gateway_dialog.visible {
                        self.free_gateway_dialog.advance_stream();
                    }

                    // The animated chat-logo ("O" with a red center and a laser
                    // beam) replaces the static logo on the empty session. It is
                    // rendered by the prompt view, which owns its animation state.
                    self.prompt_view.cursor.terminal_focused = self.terminal_focused;
                    self.session_view.drag_selection = self.drag_selection;
                    self.session_view
                        .tool_state
                        .advance_tool_spinners(delta_time);
                    // Subagent tool-call spinners live in the right panel
                    // state and share the same per-frame tick.
                    for spinner in self.state.right_panel.subagent_tool_spinners.values_mut() {
                        spinner.advance(delta_time);
                    }
                    // The /agents panes' tool-call spinners follow the same
                    // contract: advanced right before the split renders, so
                    // InProgress rows sweep in lockstep with the panel's.
                    if let Some(split) = &mut self.agents_split {
                        for spinner in split.tool_spinners.values_mut() {
                            spinner.advance(delta_time);
                        }
                    }

                    // The correction spinner occupies this row only while the
                    // agent-loop spinner is absent.
                    if agent_spinner_active && let Some(spinner) = &mut self.agent_spinner {
                        for _ in 0..animation_ticks {
                            spinner.advance();
                        }
                    } else if correction_spinner_active
                        && let Some(spinner) = &mut self.prompt_correction_spinner
                    {
                        for _ in 0..animation_ticks {
                            spinner.advance();
                        }
                    }

                    let unique_agents = self.state.unique_agents();
                    let agent_colors = crate::agent_colors::AgentColors::from_theme(&self.theme);
                    // End-cap color for the pending-queue strip: the current
                    // harness mode's hue (same one the prompt's mode
                    // indicator paints with).
                    let cap_color = crate::component::prompt::agent_mode_color(
                        &self.theme,
                        &agent_colors,
                        &unique_agents,
                        self.state.mode,
                    );
                    if let Some(spinner) = &mut self.agent_spinner {
                        spinner.set_mode_color(cap_color);
                    }
                    if self.agents_split.is_some() {
                        // The /agents split replaces the single transcript:
                        // one pane per chosen agent renders its own sub-agent
                        // activity stream; the shared prompt below is
                        // untouched. Pane rects are recorded by the split's
                        // render for the mouse hit-test. The split is inset
                        // with the SAME lateral breathing room the prompt
                        // has (`session_area.x + 2`, width − 4): the panes
                        // keep a gap to the screen edge and align with the
                        // prompt's rails instead of touching it.
                        if let Some(split) = &mut self.agents_split {
                            let inset_area = Rect::new(
                                session_area.x.saturating_add(2),
                                session_area.y,
                                session_area.width.saturating_sub(4),
                                session_area.height,
                            );
                            crate::routes::session::agents_split::render(
                                buf,
                                inset_area,
                                split,
                                &self.theme,
                                delta_time,
                            );
                            // Drag auto-scroll advances once per frame, right
                            // after the panes laid out (the same placement the
                            // right panel's auto-scroll uses).
                            split.handle_auto_scroll(delta_time);
                        }
                    } else {
                        self.session_view.render(
                            buf,
                            session_area,
                            &self.state,
                            &self.theme,
                            &self.config,
                            delta_time,
                        );
                    }
                    // Voice controls ride each answer's final line: their
                    // anchors come from the transcript layout this frame, so
                    // no row is reserved and nothing below ever moves. The
                    // answer still streaming is skipped — only its finalized
                    // siblings keep their speak/pause controls while the
                    // agent works.
                    let streaming_slot = self.streaming_answer_slot();
                    let anchors: Vec<(usize, Rect)> = if self.speech_controls_visible() {
                        self.session_view
                            .answer_anchors
                            .iter()
                            .filter(|(slot, _)| Some(*slot) != streaming_slot)
                            .copied()
                            .collect()
                    } else {
                        Vec::new()
                    };
                    self.speech.render_inline(buf, &anchors, &self.theme);
                    // Question/permission/queue-choice/recommendation dialogs
                    // rendered inline between messages and prompt (mutually exclusive).
                    if self.question_dialog.visible {
                        let now = std::time::SystemTime::now();
                        // Sync focus so the answer input's cursor blurs when the
                        // terminal loses focus (same as every other cursor).
                        self.question_dialog.cursor.terminal_focused = self.terminal_focused;
                        self.question_dialog
                            .render(buf, question_area, &self.theme, now);
                    } else if self.permission_dialog.visible {
                        self.permission_dialog
                            .render(buf, permission_area, &self.theme);
                    } else if self.queue_choice_dialog.visible {
                        self.queue_choice_dialog
                            .render(buf, queue_choice_area, &self.theme);
                    } else if self.free_gateway_dialog.visible {
                        self.free_gateway_dialog
                            .render(buf, recommendation_area, &self.theme);
                    }
                    // Pending queued messages, color-coded per queue, above the
                    // prompt (never rendered while a dialog covers that spot).
                    // The rows were wrapped once at the top of this frame;
                    // painting reuses them (no second word-wrap per frame).
                    if let Some(rows) = &pending_rows
                        && !hide_prompt_and_spinner
                        && !rows.is_empty()
                    {
                        // The mode color was already resolved above with the
                        // same `agent_colors`/`unique_agents` the session view
                        // uses — no per-frame duplicate scan.
                        crate::routes::session::pending_queue::render_rows(
                            buf,
                            pending_area,
                            rows,
                            self.hovered_queue_row,
                            &self.theme,
                            cap_color,
                        );
                    }
                    // Scroll-to-bottom pill: painted after every inline layer
                    // sharing its row (transcript, pending queues) so it is
                    // never buried; the slash menu, modal dialogs and toasts
                    // render later and legitimately cover it. Hidden while an
                    // inline dialog replaces the prompt. The anchor is the
                    // row directly above the pending strip / prompt — the
                    // exact line the agent spinner occupies when active — so
                    // the arrow stays aligned with the spinner instead of
                    // being pushed around when it appears/disappears (the
                    // spinner shrinks the session viewport by a row, which
                    // would drag a bottom-anchored pill upward).
                    if hide_prompt_and_spinner || self.agents_split.is_some() {
                        // The split replaces the transcript: a pill computed
                        // from a session_view that did not render this frame
                        // would be stale geometry painted over the panes.
                        self.session_view.clear_pill();
                    } else {
                        self.session_view.render_pill_overlay(
                            buf,
                            pending_area.y.saturating_sub(1),
                            session_area,
                            &self.theme,
                            delta_time,
                        );
                    }
                    // Hide spinner and prompt when dialog is visible (like OpenCode)
                    if !self.question_dialog.visible
                        && !self.permission_dialog.visible
                        && !self.queue_choice_dialog.visible
                    {
                        if agent_spinner_active && let Some(spinner) = &self.agent_spinner {
                            spinner.render(buf, spinner_area.x + 1, spinner_area.y);
                        } else if correction_spinner_active
                            && let Some(spinner) = &self.prompt_correction_spinner
                        {
                            spinner.render(buf, spinner_area.x + 1, spinner_area.y);
                        }
                        let provider = self.llm_config.provider.as_str();
                        let model_name = self.llm_config.model.as_deref().unwrap_or("");
                        let reasoning = self.llm_config.reasoning.as_deref();
                        // Focused `/agents` pane badge: the prompt targets
                        // that pane, so the agent-label row leads with the
                        // pane's brand name + serving triple instead of the
                        // gateway triple. The manager pane IS the main agent
                        // itself, but its bridge never emits a `SessionModel`
                        // report — its triple resolves from the pane's own
                        // selection source (`effective_pane_prompt_info`):
                        // a committed per-pane pick shows the moment it is
                        // picked, not only on the pane's next turn, and a
                        // manager pane with no pick of its own keeps the
                        // row exactly the mode + gateway triple it always
                        // was (no badge). An ACP pane keeps its reported
                        // triple (provider only when advertised) — empty
                        // until the first report, replaced on change.
                        // Unfocused (`None`) is the main agent: unchanged.
                        let focused_index =
                            self.agents_split.as_ref().and_then(|split| split.focused);
                        let mut name_styles = Vec::new();
                        // The badge's inputs, resolved BEFORE the prompt's
                        // mutable borrow: the ACP harness's own report, or —
                        // for the manager (morpheus) pane, whose bridge never
                        // reports one — the pane's own selection source (the
                        // per-pane pick, the session markers). A manager pane
                        // with no pick of its own drops the badge entirely:
                        // the row keeps the mode + gateway triple it always
                        // painted (the pre-feature behavior). The info copy
                        // is OWNED so the `&self` read ends here — the
                        // `prompt_view.render` below borrows the prompt
                        // mutably.
                        let mut badge_name: Option<&str> = None;
                        let mut badge_info: Option<
                            crate::routes::session::agents_split::AgentSessionInfo,
                        > = None;
                        if let Some(index) = focused_index
                            && let Some(pane) =
                                self.agents_split.as_ref().and_then(|s| s.panes.get(index))
                        {
                            let name = display_name(&pane.agent);
                            let info = self.effective_pane_prompt_info(index);
                            if info.is_some() || pane.agent != MANAGER_LABEL {
                                badge_name = Some(name);
                                badge_info = Some(info.unwrap_or_default());
                                // The header's own per-glyph sweep, resolved
                                // once: the badge logo IS the pane logo.
                                name_styles = brand_name_styles(name, &self.theme, true);
                            }
                        }
                        let pane_badge: Option<PanePromptInfo<'_>> = badge_name.map(|name| {
                            let info = badge_info.as_ref();
                            PanePromptInfo {
                                name,
                                name_styles: &name_styles,
                                model: info.and_then(|i| i.model.as_deref()).unwrap_or(""),
                                model_label: info.and_then(|i| i.model_label.as_deref()),
                                provider: info.and_then(|i| i.provider.as_deref()).unwrap_or(""),
                                provider_label: info.and_then(|i| i.provider_label.as_deref()),
                                reasoning: info.and_then(|i| i.reasoning.as_deref()),
                            }
                        });
                        self.prompt_view.render(
                            buf,
                            prompt_area,
                            &self.state,
                            &self.theme,
                            &agent_colors,
                            &unique_agents,
                            std::time::SystemTime::now(),
                            provider,
                            model_name,
                            reasoning,
                            pane_badge,
                        );
                    }
                }
            }

            let footer_area = Rect::new(main_area.x, footer_y, main_area.width, 1);
            // Hover state for the footer's update button, resolved against
            // the hitbox the PREVIOUS frame stored — read BEFORE the clear
            // below (the same one-frame-muted→text swap the header's
            // `bug report` / `← esc` links use, which also read their area
            // before overwriting it later in the frame).
            let update_hover = self.footer_update_area.is_some_and(|link_area| {
                self.last_mouse_x >= link_area.x
                    && self.last_mouse_x < link_area.right()
                    && self.last_mouse_y >= link_area.y
                    && self.last_mouse_y < link_area.bottom()
            });
            // Stale-hitbox guard: the footer's update-button hitbox is only
            // (re)assigned by the Session arm below, so clear it every frame
            // first — a leftover rect from a previous session frame must
            // never stay clickable after leaving that route (the same
            // re-assignment contract `bug_link_area`/`back_link_area` follow).
            self.footer_update_area = None;
            match self.mode() {
                AppMode::Session => {
                    let hide_text = self.question_dialog.visible
                        || self.permission_dialog.visible
                        || self.queue_choice_dialog.visible
                        || self.free_gateway_dialog.visible;
                    self.footer_update_area = FooterView::render_with_mode(
                        buf,
                        footer_area,
                        &self.state,
                        &self.theme,
                        hide_text,
                        self.update_latest_version.as_deref(),
                        update_hover,
                    );
                }
                _ => {}
            }
            let now = std::time::SystemTime::now();
            // Sync terminal_focused to the dialog cursor so ThemeList/ModelList/ApiKeyInput
            // all respect the terminal focus state (blur when user clicks outside).
            if let Some(d) = self.dialog.current_mut() {
                d.cursor.terminal_focused = self.terminal_focused;
                // Animate the ModelList loading spinner while models are
                // being fetched through the API (frame advances only when
                // the loading dialog is on top).
                if let DialogType::ModelList { loading: true, .. } = &d.dialog_type {
                    for _ in 0..animation_ticks {
                        d.spinner.advance();
                    }
                }
            }
            // Animate the slash menu's loading row the same way: the menu
            // shows its spinner while an agent command snapshot probe is
            // in flight (frame advances only while the menu is visible).
            if self.slash_menu.loading {
                for _ in 0..animation_ticks {
                    self.slash_menu.spinner.advance();
                }
            }
            // The slash menu renders first so modal dialogs (e.g. the Ctrl+C
            // "Quit morpheus?" confirm) always paint on top of it — the menu is
            // inline chrome, the dialog is a modal overlay that owns the
            // whole input surface while visible (sovereign keys + mouse).
            // Its borders and selection follow the current harness mode's
            // color, like the prompt's own rails.
            let unique_agents = self.state.unique_agents();
            let agent_colors = crate::agent_colors::AgentColors::from_theme(&self.theme);
            let mode_color = crate::component::prompt::agent_mode_color(
                &self.theme,
                &agent_colors,
                &unique_agents,
                self.state.mode,
            );
            self.slash_menu
                .render(buf, prompt_area, &self.theme, mode_color);
            // Hover state for the API-key dialog's Login button, resolved
            // against the tracked pointer — the button's rect is pure
            // geometry (`api_key_login_button_area`), so this reads the SAME
            // source the render and the mouse hit-test use (no stored hitbox
            // to go stale). Written back into the instance the render below
            // reads; a non-ApiKeyInput stack top simply keeps `false`.
            if let Some(d) = self.dialog.current_mut() {
                let hover = match &d.dialog_type {
                    DialogType::ApiKeyInput {
                        device_login: true,
                        input,
                        ..
                    } => crate::ui::dialogs::api_key_login_button_area(area, input).is_some_and(
                        |r| {
                            self.last_mouse_x >= r.x
                                && self.last_mouse_x < r.right()
                                && self.last_mouse_y == r.y
                        },
                    ),
                    _ => false,
                };
                d.login_hover = hover;
            }
            self.dialog.render(buf, area, &self.theme, now);
            // The /agents picker renders above every modal panel (a
            // centered box like the registration forms) but below the
            // toasts, which stay transient. Its search caret uses the
            // shared cursor component, so it gets the same focus sync the
            // dialog cursors receive (blur ⇒ static dim caret).
            self.agents_picker.cursor.terminal_focused = self.terminal_focused;
            self.agents_picker.render(buf, area, &self.theme, now);
            // Toasts render last so transient feedback (copied, not saved,
            // unavailable clipboard) is never buried under a modal panel —
            // the centered registration forms cover the top-right toast
            // corner on compact terminals. The tracked mouse position drives
            // the ⧉ copy button's hover highlight (None when untracked).
            self.toast_state.render(
                buf,
                area,
                &self.theme,
                Some((self.last_mouse_x, self.last_mouse_y)),
            );
        }
    }

    /// Visual rows of the pending queues, one layout entry per drawn row —
    /// the shared strip's layout ([`crate::routes::session::pending_queue`]),
    /// re-exported here because the chat's geometry helpers and mouse
    /// hit-testing read it through `App`.
    pub(super) fn pending_queue_layout(queues: &PendingQueues, width: u16) -> Vec<PendingQueueRow> {
        crate::routes::session::pending_queue::layout(queues, width)
    }

    /// Render the pending queued messages above the prompt, each preserving
    /// FIFO order, in the current harness mode's hue (the same hue the
    /// prompt's mode indicator paints).
    pub(super) fn render_pending_queues(&self, buf: &mut ratatui::buffer::Buffer, area: Rect) {
        let Some(queues) = self.state.current_pending_queues() else {
            return;
        };
        let unique_agents = self.state.unique_agents();
        let agent_colors = crate::agent_colors::AgentColors::from_theme(&self.theme);
        let cap_color = crate::component::prompt::agent_mode_color(
            &self.theme,
            &agent_colors,
            &unique_agents,
            self.state.mode,
        );
        crate::routes::session::pending_queue::render(
            buf,
            area,
            queues,
            self.hovered_queue_row,
            &self.theme,
            cap_color,
        );
    }
}
