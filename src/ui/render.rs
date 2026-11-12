use std::time::Duration;

use crate::herd::{HerdTask, TaskStatus};
use crate::macos::SIDEBAR_PANE_TITLE;

use super::canvas::{Canvas, HitTarget, Rect, Rgb, Style, activity_colors, agent_colors, palette};
use super::modals::render_active_modal;
use super::snapshot::{Activity, FleetItem, short_key};
use super::state::{AppState, GridLayout, Modal, SidebarGroup, SidebarSection, ViewMode};

pub fn render_app(state: &AppState, canvas: &mut Canvas) {
    canvas.fill_rect(
        Rect::new(0, 0, canvas.width, canvas.height),
        palette::BG_CANVAS,
    );
    if canvas.width < 24 || canvas.height < 8 {
        canvas.put_str(
            1,
            1,
            canvas.width.saturating_sub(2),
            "Resize Ghostty pane (min 24x8)",
            Style::new(palette::FG_PRIMARY, palette::BG_CANVAS).bold(),
        );
        return;
    }

    let view = state.effective_view(canvas.width);
    render_header(state, view, canvas);

    let body_top = 2u16;
    let footer_h = 1u16;
    let body_h = canvas.height.saturating_sub(body_top + footer_h);
    let body_rect = Rect::new(0, body_top, canvas.width, body_h);

    match view {
        ViewMode::Board => render_board_view(state, body_rect, canvas),
        ViewMode::Fleet => render_fleet_view(state, body_rect, canvas),
        ViewMode::Sidebar => render_sidebar_view(state, body_rect, canvas),
    }

    render_footer(state, view, canvas.height.saturating_sub(1), canvas);
    render_active_modal(&state.modal, canvas);
}

fn render_header(state: &AppState, view: ViewMode, canvas: &mut Canvas) {
    let w = canvas.width;
    canvas.fill_rect(Rect::new(0, 0, w, 1), palette::BG_HEADER);
    canvas.fill_rect(Rect::new(0, 1, w, 1), palette::BG_TOOLBAR);

    // Row 0: Brand + Status summary pills + Watcher telemetry
    let mut x = 1u16;
    let brand = if w < 52 {
        SIDEBAR_PANE_TITLE
    } else {
        "◈ GHOSTBACK HERD"
    };
    let used = canvas.draw_pill(
        x,
        0,
        w.saturating_sub(x),
        brand,
        Style::new(palette::FG_BRIGHT, palette::BG_BRAND).bold(),
    );
    x = x.saturating_add(used + 1);

    let pills = [
        (
            Activity::Working,
            format!(
                "{} {}",
                Activity::Working.icon(state.tick),
                state.snapshot.working_count
            ),
            format!(
                "{} {} Working",
                Activity::Working.icon(state.tick),
                state.snapshot.working_count
            ),
        ),
        (
            Activity::Waiting,
            format!("◆ {}", state.snapshot.waiting_count),
            format!("◆ {} Waiting", state.snapshot.waiting_count),
        ),
        (
            Activity::Attention,
            format!("▲ {}", state.snapshot.attention_count),
            format!("▲ {} Attention", state.snapshot.attention_count),
        ),
        (
            Activity::Shell,
            format!("○ {}", state.snapshot.shell_count),
            format!("○ {} Shells", state.snapshot.shell_count),
        ),
    ];

    for (act, compact, full) in pills {
        if x.saturating_add(6) >= w {
            break;
        }
        let label = if w < 78 { &compact } else { &full };
        let (fg, mut bg) = activity_colors(act);
        let active_filter = state.status_filter == Some(act);
        if active_filter {
            bg = palette::BG_CARD_SELECTED;
        }
        let pw = canvas.draw_pill(
            x,
            0,
            w.saturating_sub(x),
            label,
            Style::new(fg, bg).with_bold(active_filter),
        );
        let next_filter = if active_filter { None } else { Some(act) };
        canvas.register_hit(Rect::new(x, 0, pw, 1), HitTarget::FilterStatus(next_filter));
        x = x.saturating_add(pw + 1);
    }

    if state.snapshot.saved_count > 0 && x.saturating_add(8) < w {
        let saved_label = if w < 78 {
            format!("◇ {}", state.snapshot.saved_count)
        } else if state.show_saved {
            format!("◇ {} Saved:ON (s)", state.snapshot.saved_count)
        } else {
            format!("◇ {} Saved (s)", state.snapshot.saved_count)
        };
        let (fg, bg) = if state.show_saved {
            (palette::FG_BRIGHT, palette::BG_CARD_SELECTED)
        } else {
            (palette::SAVED_FG, palette::SAVED_BG)
        };
        let pw = canvas.draw_pill(
            x,
            0,
            w.saturating_sub(x),
            &saved_label,
            Style::new(fg, bg).with_bold(state.show_saved),
        );
        canvas.register_hit(Rect::new(x, 0, pw, 1), HitTarget::ToggleShowSaved);
        x = x.saturating_add(pw + 1);
    }

    // Right side of Row 0: Watcher status + Help button
    if w >= 48 {
        let help_label = " ? Help ";
        let hx = w.saturating_sub(help_label.len() as u16 + 1);
        if hx > x {
            canvas.put_str(
                hx,
                0,
                help_label.len() as u16,
                help_label,
                Style::new(palette::FG_SECONDARY, palette::BG_PILL),
            );
            canvas.register_hit(
                Rect::new(hx, 0, help_label.len() as u16, 1),
                HitTarget::ToggleHelp,
            );
            let watcher_text = if state.snapshot.watcher_alive {
                "● watcher live "
            } else {
                "○ watcher idle "
            };
            let watcher_fg = if state.snapshot.watcher_alive {
                palette::WORKING_FG
            } else {
                palette::FG_MUTED
            };
            let wx = hx.saturating_sub(watcher_text.chars().count() as u16 + 1);
            if wx > x {
                canvas.put_str(
                    wx,
                    0,
                    watcher_text.chars().count() as u16,
                    watcher_text,
                    Style::new(watcher_fg, palette::BG_HEADER),
                );
            }
        }
    }

    // Row 1: View Switcher Tabs + Quick Action Buttons (or active Filter input)
    let mut tx = 1u16;
    if w < 52 {
        let vlabel = match view {
            ViewMode::Board => "v:Board",
            ViewMode::Fleet => "v:Fleet",
            ViewMode::Sidebar => "v:Side",
        };
        let vw = canvas.draw_pill(
            tx,
            1,
            w.saturating_sub(tx),
            vlabel,
            Style::new(palette::FG_BRIGHT, palette::ACCENT_BRAND).bold(),
        );
        canvas.register_hit(Rect::new(tx, 1, vw, 1), HitTarget::SetView(view.next()));
        tx = tx.saturating_add(vw + 1);
    } else {
        let views = [
            (ViewMode::Board, "1 ▦ Board"),
            (ViewMode::Fleet, "2 ☰ Fleet"),
            (ViewMode::Sidebar, "3 ◧ Sidebar"),
        ];
        for (vmode, vlabel) in views {
            if tx.saturating_add(4) >= w {
                break;
            }
            let selected = view == vmode;
            let (fg, bg) = if selected {
                (palette::FG_BRIGHT, palette::ACCENT_BRAND)
            } else {
                (palette::FG_SECONDARY, palette::BG_PILL)
            };
            let vw = canvas.draw_pill(
                tx,
                1,
                w.saturating_sub(tx),
                vlabel,
                Style::new(fg, bg).with_bold(selected),
            );
            canvas.register_hit(Rect::new(tx, 1, vw, 1), HitTarget::SetView(vmode));
            tx = tx.saturating_add(vw + 1);
        }
    }

    if state.modal == Modal::Filter || !state.filter_query.is_empty() {
        let prompt = format!(" / {}█ ", state.filter_query);
        let fw = canvas.put_str(
            tx,
            1,
            w.saturating_sub(tx + 1),
            &prompt,
            Style::new(palette::WAITING_FG, palette::BG_INPUT).bold(),
        );
        canvas.register_hit(Rect::new(tx, 1, fw.max(8), 1), HitTarget::OpenFilterBar);
        return;
    }

    let actions: &[(&str, HitTarget, Rgb)] = if w < 64 {
        &[
            ("+New", HitTarget::OpenSpawnModal, palette::WORKING_FG),
            ("⑂Pair", HitTarget::OpenPairModal, palette::BRANCH_FG),
            ("⚡Next", HitTarget::JumpNextWaiting, palette::WAITING_FG),
            ("/Find", HitTarget::OpenFilterBar, palette::FG_SECONDARY),
        ]
    } else {
        &[
            (
                "⚡ u:Next Waiting",
                HitTarget::JumpNextWaiting,
                palette::WAITING_FG,
            ),
            ("+ n:Spawn", HitTarget::OpenSpawnModal, palette::WORKING_FG),
            (
                "⑂ p:Pair Child",
                HitTarget::OpenPairModal,
                palette::BRANCH_FG,
            ),
            (
                "✎ t:Title",
                HitTarget::OpenTitleModal,
                palette::FG_SECONDARY,
            ),
            (
                "↻ r:Retry",
                HitTarget::TriggerRetrySelected,
                palette::ATTENTION_FG,
            ),
            ("◧ d:Dock", HitTarget::TriggerDockSidebar, palette::SHELL_FG),
            (
                if state.agents_only {
                    "a:Agents Only"
                } else {
                    "a:All Panes"
                },
                HitTarget::ToggleAgentsOnly,
                palette::BRANCH_FG,
            ),
            ("/ Filter", HitTarget::OpenFilterBar, palette::FG_SECONDARY),
        ]
    };

    for (label, target, fg) in actions {
        if tx.saturating_add(label.chars().count() as u16 + 3) >= w {
            break;
        }
        let aw = canvas.draw_pill(
            tx,
            1,
            w.saturating_sub(tx),
            label,
            Style::new(*fg, palette::BG_PILL),
        );
        canvas.register_hit(Rect::new(tx, 1, aw, 1), target.clone());
        tx = tx.saturating_add(aw + 1);
    }
}

fn render_board_view(state: &AppState, rect: Rect, canvas: &mut Canvas) {
    let inspector_h = if rect.h >= 16 { 6u16 } else { 0u16 };
    let board_h = rect.h.saturating_sub(inspector_h);
    let col_count = if rect.w >= 104 {
        4u16
    } else if rect.w >= 72 {
        3u16
    } else {
        2u16
    };
    let gap = 1u16;
    let total_gaps = gap * (col_count.saturating_sub(1));
    let col_w = rect.w.saturating_sub(total_gaps) / col_count;

    let columns: [(&str, &str, Activity, Rgb); 4] = [
        (
            "● WORKING",
            "● WORKING",
            Activity::Working,
            palette::WORKING_FG,
        ),
        (
            "◆ WAITING FOR INPUT",
            "◆ WAITING",
            Activity::Waiting,
            palette::WAITING_FG,
        ),
        (
            "▲ NEEDS ATTENTION",
            "▲ ATTENTION",
            Activity::Attention,
            palette::ATTENTION_FG,
        ),
        (
            "○ SHELLS & SAVED",
            "○ SHELLS",
            Activity::Shell,
            palette::SHELL_FG,
        ),
    ];

    let visible = state.visible_items();

    for (c, &(full_title, short_title, _, header_fg)) in
        columns.iter().enumerate().take(usize::from(col_count))
    {
        let col_x = rect.x + (c as u16) * (col_w + gap);
        let cur_w = if c + 1 == usize::from(col_count) {
            rect.w.saturating_sub(col_x)
        } else {
            col_w
        };
        let col_rect = Rect::new(col_x, rect.y, cur_w, board_h);
        canvas.draw_box(
            col_rect,
            palette::BORDER_SUBTLE,
            palette::BG_COLUMN,
            true,
            None,
        );

        let col_items: Vec<(usize, &FleetItem)> = visible
            .iter()
            .copied()
            .filter(|(_, item)| {
                let target_col = item.activity.column_index().min(usize::from(col_count) - 1);
                target_col == c
            })
            .collect();

        let title = if col_count < 4 && c + 1 == usize::from(col_count) {
            "▲ ATTN & SHELLS"
        } else if cur_w < 26 {
            short_title
        } else {
            full_title
        };
        let header_text = format!(" {} ({}) ", title, col_items.len());
        canvas.put_str(
            col_x + 2,
            rect.y,
            cur_w.saturating_sub(4),
            &header_text,
            Style::new(header_fg, palette::BG_COLUMN).bold(),
        );

        let inner_y = rect.y + 1;
        let inner_h = board_h.saturating_sub(2);
        let card_h = 5u16;
        let max_cards = usize::from(inner_h / card_h).max(1);

        if col_items.is_empty() {
            if inner_h >= 3 {
                let empty_label = if cur_w < 29 {
                    "Empty lane"
                } else {
                    "No terminals in this lane"
                };
                canvas.put_str(
                    col_x + 2,
                    inner_y + 1,
                    cur_w.saturating_sub(4),
                    empty_label,
                    Style::new(palette::FG_MUTED, palette::BG_COLUMN).dim(),
                );
            }
            continue;
        }

        // Scroll column so selected item in this column stays visible
        let selected_pos = col_items
            .iter()
            .position(|(idx, _)| *idx == state.selected_index)
            .unwrap_or(0);
        let scroll_offset = if selected_pos >= max_cards {
            selected_pos + 1 - max_cards
        } else {
            0
        };

        for (slot, (item_idx, item)) in col_items
            .iter()
            .skip(scroll_offset)
            .take(max_cards)
            .enumerate()
        {
            let cy = inner_y + (slot as u16) * card_h;
            if cy + card_h > rect.y + board_h {
                break;
            }
            let card_rect = Rect::new(col_x + 1, cy, cur_w.saturating_sub(2), card_h);
            render_board_card(state, *item_idx, item, card_rect, canvas);
        }
    }

    if inspector_h > 0 {
        let insp_rect = Rect::new(rect.x, rect.y + board_h, rect.w, inspector_h);
        render_inspector(state, insp_rect, canvas);
    }
}

fn render_board_card(
    state: &AppState,
    idx: usize,
    item: &FleetItem,
    rect: Rect,
    canvas: &mut Canvas,
) {
    let selected = state.selected_index == idx;
    let hovered = state
        .hover_pos
        .is_some_and(|(hx, hy)| rect.contains(hx, hy));
    let (act_fg, _) = activity_colors(item.activity);
    let (bg, border_fg) = if selected {
        (palette::BG_CARD_SELECTED, palette::BORDER_SELECTED)
    } else if hovered {
        (palette::BG_CARD_HOVER, palette::BORDER_HOVER)
    } else {
        (palette::BG_CARD, palette::BORDER_SUBTLE)
    };

    canvas.draw_box(rect, border_fg, bg, true, Some(act_fg));
    canvas.register_hit(rect, HitTarget::SelectCard(idx));

    let inner_x = rect.x + 2;
    let inner_w = rect.w.saturating_sub(3);
    if inner_w < 6 {
        return;
    }

    // Top border badge: Surface location (`W1·T2`) + focused indicator
    let surf = if item.focused_in_ghostty {
        format!(" ★ {} ", item.surface_badge())
    } else {
        format!(" {} ", item.surface_badge())
    };
    let surf_w = surf.chars().count() as u16;
    if rect.w > surf_w + 4 {
        let sx = rect.x + rect.w - surf_w - 2;
        let sfg = if item.focused_in_ghostty {
            palette::WAITING_FG
        } else {
            palette::FG_SECONDARY
        };
        canvas.put_str(sx, rect.y, surf_w, &surf, Style::new(sfg, bg).bold());
    }

    // Row 1 inside card: Agent pill + optional Herd task badge + Project Name
    let (ag_fg, ag_bg) = agent_colors(item.agent);
    let pw = canvas.draw_pill(
        inner_x,
        rect.y + 1,
        inner_w,
        item.agent_label(),
        Style::new(ag_fg, ag_bg).bold(),
    );
    let mut proj_x = inner_x.saturating_add(pw + 1);
    if let Some(ctask) = state.child_task_for(&item.terminal_key) {
        let badge = format!("↳{}·R{}", ctask.role.short_badge(), ctask.round);
        let bw = canvas.put_str(
            proj_x,
            rect.y + 1,
            (rect.x + rect.w - 1).saturating_sub(proj_x),
            &badge,
            Style::new(palette::BRANCH_FG, bg).bold(),
        );
        proj_x = proj_x.saturating_add(bw + 1);
    } else {
        let pcount = state.parent_tasks_for(&item.terminal_key).len();
        if pcount > 0 {
            let badge = format!("⑂{pcount}");
            let bw = canvas.put_str(
                proj_x,
                rect.y + 1,
                (rect.x + rect.w - 1).saturating_sub(proj_x),
                &badge,
                Style::new(palette::WORKING_FG, bg).bold(),
            );
            proj_x = proj_x.saturating_add(bw + 1);
        }
    }
    if proj_x < rect.x + rect.w - 1 {
        canvas.put_str(
            proj_x,
            rect.y + 1,
            (rect.x + rect.w - 1).saturating_sub(proj_x),
            &item.project,
            Style::new(palette::FG_BRIGHT, bg).bold(),
        );
    }

    // Row 2 inside card: Git Branch + Short Conversation ID
    let mut rx = inner_x;
    if let Some(git) = &item.git {
        let branch_txt = if git.is_worktree {
            format!("⎇ {}·wt", git.branch)
        } else {
            format!("⎇ {}", git.branch)
        };
        let bw = canvas.put_str(
            rx,
            rect.y + 2,
            inner_w / 2 + 2,
            &branch_txt,
            Style::new(palette::BRANCH_FG, bg),
        );
        rx = rx.saturating_add(bw + 2);
    }
    if rx < rect.x + rect.w - 1 {
        let id_str = item.short_session_id();
        canvas.put_str(
            rx,
            rect.y + 2,
            (rect.x + rect.w - 1).saturating_sub(rx),
            &id_str,
            Style::new(palette::FG_SECONDARY, bg),
        );
    }

    // Row 3 inside card: Live Activity / Status / Tool
    let icon = item.activity.icon(state.tick);
    let status_line = format!("{icon} {}", item.status_text);
    let status_fg = match item.activity {
        Activity::Working => palette::WORKING_FG,
        Activity::Waiting => palette::WAITING_FG,
        Activity::Attention => palette::ATTENTION_FG,
        Activity::Shell => palette::FG_SECONDARY,
        Activity::Saved => palette::FG_MUTED,
    };
    canvas.put_str(
        inner_x,
        rect.y + 3,
        inner_w,
        &status_line,
        Style::new(status_fg, bg),
    );
}

fn render_fleet_view(state: &AppState, rect: Rect, canvas: &mut Canvas) {
    let inspector_h = if rect.h >= 14 { 6u16 } else { 0u16 };
    let table_h = rect.h.saturating_sub(inspector_h);
    let table_rect = Rect::new(rect.x, rect.y, rect.w, table_h);
    canvas.draw_box(
        table_rect,
        palette::BORDER_SUBTLE,
        palette::BG_COLUMN,
        true,
        None,
    );

    let w = rect.w.saturating_sub(4);
    let x0 = rect.x + 2;
    let header_style = Style::new(palette::FG_MUTED, palette::BG_COLUMN).bold();
    canvas.put_str(
        x0,
        rect.y + 1,
        w,
        "STATE        AGENT      PROJECT          BRANCH           SURFACE     CONVERSATION ID    ACTIVITY / DIAGNOSTIC",
        header_style,
    );

    let visible = state.visible_items();
    let rows_avail = usize::from(table_h.saturating_sub(3)).max(1);
    let selected_pos = visible
        .iter()
        .position(|(idx, _)| *idx == state.selected_index)
        .unwrap_or(0);
    let scroll = if selected_pos >= rows_avail {
        selected_pos + 1 - rows_avail
    } else {
        0
    };

    for (row_i, (idx, item)) in visible
        .into_iter()
        .skip(scroll)
        .take(rows_avail)
        .enumerate()
    {
        let y = rect.y + 2 + row_i as u16;
        let selected = state.selected_index == idx;
        let hovered = state
            .hover_pos
            .is_some_and(|(hx, hy)| hy == y && hx > rect.x && hx + 1 < rect.x + rect.w);
        let row_bg = if selected {
            palette::BG_CARD_SELECTED
        } else if hovered {
            palette::BG_CARD_HOVER
        } else {
            palette::BG_COLUMN
        };
        let row_rect = Rect::new(rect.x + 1, y, rect.w.saturating_sub(2), 1);
        canvas.fill_rect(row_rect, row_bg);
        canvas.register_hit(row_rect, HitTarget::SelectCard(idx));

        let (act_fg, _) = activity_colors(item.activity);
        if selected {
            canvas.set(
                rect.x + 1,
                y,
                '▎',
                Style::new(palette::BORDER_SELECTED, row_bg).bold(),
            );
        }
        let state_str = format!(
            "{} {:<9}",
            item.activity.icon(state.tick),
            item.activity.label()
        );
        canvas.put_str(x0, y, 12, &state_str, Style::new(act_fg, row_bg).bold());

        let (ag_fg, _) = agent_colors(item.agent);
        canvas.put_str(
            x0 + 13,
            y,
            10,
            item.agent_label(),
            Style::new(ag_fg, row_bg).bold(),
        );
        canvas.put_str(
            x0 + 24,
            y,
            16,
            &item.project,
            Style::new(palette::FG_BRIGHT, row_bg).bold(),
        );
        let branch = item
            .git
            .as_ref()
            .map(|g| format!("⎇ {}", g.branch))
            .unwrap_or_else(|| "—".into());
        canvas.put_str(
            x0 + 41,
            y,
            16,
            &branch,
            Style::new(palette::BRANCH_FG, row_bg),
        );
        canvas.put_str(
            x0 + 58,
            y,
            11,
            &item.surface_badge(),
            Style::new(palette::FG_SECONDARY, row_bg),
        );
        canvas.put_str(
            x0 + 70,
            y,
            18,
            &item.short_session_id(),
            Style::new(palette::FG_SECONDARY, row_bg),
        );
        let rem = (rect.x + rect.w - 1).saturating_sub(x0 + 89);
        if rem > 0 {
            let fleet_status = if let Some(ctask) = state.child_task_for(&item.terminal_key) {
                format!(
                    "[↳ {}·R{}] {}",
                    ctask.role.short_badge(),
                    ctask.round,
                    item.status_text
                )
            } else {
                let pcount = state.parent_tasks_for(&item.terminal_key).len();
                if pcount > 0 {
                    format!("[⑂{pcount}] {}", item.status_text)
                } else {
                    item.status_text.clone()
                }
            };
            canvas.put_str(
                x0 + 89,
                y,
                rem,
                &fleet_status,
                Style::new(palette::FG_PRIMARY, row_bg),
            );
        }
    }

    if inspector_h > 0 {
        let insp_rect = Rect::new(rect.x, rect.y + table_h, rect.w, inspector_h);
        render_inspector(state, insp_rect, canvas);
    }
}

enum SidebarVLine<'a> {
    SectionHeader {
        title: String,
        fg: Rgb,
    },
    CardTop {
        idx: usize,
        item: &'a FleetItem,
        child_info: Option<(&'a HerdTask, bool)>,
    },
    CardBottom {
        idx: usize,
        item: &'a FleetItem,
        child_info: Option<(&'a HerdTask, bool)>,
    },
}

fn build_sidebar_vlines<'a>(
    state: &AppState,
    groups: Vec<SidebarSection<'a>>,
) -> (Vec<SidebarVLine<'a>>, Option<(usize, usize)>, usize, usize) {
    let mut vlines = Vec::new();
    let mut selected_span = None;
    let mut selected_ord = 1usize;
    let mut card_counter = 0usize;

    for section in groups {
        let header_line = vlines.len();
        vlines.push(SidebarVLine::SectionHeader {
            title: section.title,
            fg: section.fg,
        });
        for (row_in_sec, row) in section.rows.into_iter().enumerate() {
            card_counter += 1;
            let top_line = vlines.len();
            vlines.push(SidebarVLine::CardTop {
                idx: row.idx,
                item: row.item,
                child_info: row.child_info,
            });
            vlines.push(SidebarVLine::CardBottom {
                idx: row.idx,
                item: row.item,
                child_info: row.child_info,
            });
            if state.selected_index == row.idx {
                selected_ord = card_counter;
                let span_start = if row_in_sec == 0 {
                    header_line
                } else {
                    top_line
                };
                selected_span = Some((span_start, top_line + 2));
            }
        }
    }

    (vlines, selected_span, selected_ord, card_counter)
}

fn render_sidebar_view(state: &AppState, rect: Rect, canvas: &mut Canvas) {
    canvas.draw_box(rect, palette::BORDER_SUBTLE, palette::BG_COLUMN, true, None);

    let group_label = match state.sidebar_group {
        SidebarGroup::Status => " ◧ SIDEBAR · Group: Status (g) ",
        SidebarGroup::Project => " ◧ SIDEBAR · Group: Project (g) ",
    };
    let gw = canvas.put_str(
        rect.x + 2,
        rect.y,
        rect.w.saturating_sub(4),
        group_label,
        Style::new(palette::FG_SECONDARY, palette::BG_COLUMN).bold(),
    );
    canvas.register_hit(
        Rect::new(rect.x + 2, rect.y, gw, 1),
        HitTarget::ToggleSidebarGroup,
    );

    let show_grid_bar = rect.h >= 12;
    if show_grid_bar {
        let gy = rect.y + 1;
        canvas.fill_rect(
            Rect::new(rect.x + 1, gy, rect.w.saturating_sub(2), 1),
            palette::BG_TOOLBAR,
        );
        let mut gx = rect.x + 2;
        for layout in GridLayout::ALL {
            if gx.saturating_add(3) >= rect.x + rect.w - 1 {
                break;
            }
            let active = state.grid_layout == layout;
            let (fg, bg) = if active {
                (palette::FG_BRIGHT, palette::ACCENT_BRAND)
            } else {
                (palette::FG_SECONDARY, palette::BG_PILL)
            };
            let pw = canvas.put_str(
                gx,
                gy,
                (rect.x + rect.w - 1).saturating_sub(gx),
                layout.short_label(),
                Style::new(fg, bg).with_bold(active),
            );
            canvas.register_hit(Rect::new(gx, gy, pw, 1), HitTarget::ApplyGrid(layout));
            gx = gx.saturating_add(pw + 1);
        }
        if gx.saturating_add(5) < rect.x + rect.w {
            let hw = canvas.put_str(
                gx,
                gy,
                (rect.x + rect.w - 1).saturating_sub(gx),
                "-Hide",
                Style::new(palette::WAITING_FG, palette::BG_PILL).bold(),
            );
            canvas.register_hit(
                Rect::new(gx, gy, hw, 1),
                HitTarget::HideCard(state.selected_index),
            );
            gx = gx.saturating_add(hw + 1);
        }
        if gx.saturating_add(4) < rect.x + rect.w {
            let xw = canvas.put_str(
                gx,
                gy,
                (rect.x + rect.w - 1).saturating_sub(gx),
                "×End",
                Style::new(palette::ATTENTION_FG, palette::BG_PILL).bold(),
            );
            canvas.register_hit(
                Rect::new(gx, gy, xw, 1),
                HitTarget::CloseCard(state.selected_index),
            );
        }
    }

    let groups = state.sidebar_sections();
    if groups.is_empty() {
        canvas.put_str(
            rect.x + 2,
            rect.y + 3,
            rect.w.saturating_sub(4),
            "No matching terminals.",
            Style::new(palette::FG_MUTED, palette::BG_COLUMN),
        );
        return;
    }

    let show_mini_insp = rect.h >= 15 && state.selected_item().is_some();
    let list_top = if show_grid_bar {
        rect.y + 2
    } else {
        rect.y + 1
    };
    let list_bottom = if show_mini_insp {
        rect.y + rect.h.saturating_sub(4)
    } else {
        rect.y + rect.h.saturating_sub(1)
    };
    let list_h = usize::from(list_bottom.saturating_sub(list_top));
    if list_h == 0 {
        return;
    }

    let (vlines, selected_span, selected_ord, card_counter) = build_sidebar_vlines(state, groups);

    let pos_badge = format!(" {selected_ord}/{card_counter} ");
    let pb_w = pos_badge.chars().count() as u16;
    if rect.w > gw + pb_w + 5 {
        let px = rect.x + rect.w - 2 - pb_w;
        canvas.put_str(
            px,
            rect.y,
            pb_w,
            &pos_badge,
            Style::new(palette::FG_MUTED, palette::BG_COLUMN),
        );
    }

    let total_vlines = vlines.len();
    let scroll_y = if let Some((span_start, span_end)) = selected_span {
        if span_end > list_h {
            (span_end - list_h).min(span_start)
        } else {
            0
        }
    } else {
        0
    };

    for (offset, vline) in vlines.iter().skip(scroll_y).take(list_h).enumerate() {
        let y = list_top + offset as u16;
        match vline {
            SidebarVLine::SectionHeader { title, fg } => {
                canvas.put_str(
                    rect.x + 2,
                    y,
                    rect.w.saturating_sub(4),
                    title,
                    Style::new(*fg, palette::BG_COLUMN).bold(),
                );
            }
            SidebarVLine::CardTop {
                idx,
                item,
                child_info,
            } => render_sidebar_card_top(state, rect, y, *idx, item, *child_info, canvas),
            SidebarVLine::CardBottom {
                idx,
                item,
                child_info,
            } => render_sidebar_card_bottom(state, rect, y, *idx, item, *child_info, canvas),
        }
    }

    render_sidebar_scrollbar(
        rect,
        list_top,
        list_bottom,
        list_h,
        scroll_y,
        total_vlines,
        canvas,
    );

    if show_mini_insp && let Some(item) = state.selected_item() {
        render_sidebar_mini_inspector(state, rect, list_bottom, item, canvas);
    }
}

fn render_sidebar_card_top(
    state: &AppState,
    rect: Rect,
    y: u16,
    idx: usize,
    item: &FleetItem,
    child_info: Option<(&HerdTask, bool)>,
    canvas: &mut Canvas,
) {
    let selected = state.selected_index == idx;
    let hovered = state.hover_pos.is_some_and(|(hx, hy)| {
        (hy == y || hy == y + 1) && hx > rect.x && hx + 1 < rect.x + rect.w
    });
    let bg = if selected {
        palette::BG_CARD_SELECTED
    } else if hovered {
        palette::BG_CARD_HOVER
    } else {
        palette::BG_CARD
    };
    let row_rect = Rect::new(rect.x + 1, y, rect.w.saturating_sub(2), 1);
    canvas.fill_rect(row_rect, bg);
    canvas.register_hit(row_rect, HitTarget::SelectCard(idx));

    let (act_fg, _) = activity_colors(item.activity);
    let bar_fg = if selected {
        palette::BORDER_SELECTED
    } else if child_info.is_some() {
        palette::BRANCH_FG
    } else {
        act_fg
    };
    canvas.set(rect.x + 1, y, '▎', Style::new(bar_fg, bg).bold());

    let mut lx = rect.x + 3;
    if let Some((task, is_last)) = child_info {
        let branch = if is_last {
            "└─ ↳ "
        } else {
            "├─ ↳ "
        };
        let bw = canvas.put_str(lx, y, 6, branch, Style::new(palette::BRANCH_FG, bg).bold());
        lx = lx.saturating_add(bw);
        let role_ag = format!(
            "{}·{} R{}",
            task.role.short_badge(),
            item.agent_label(),
            task.round
        );
        let (ag_fg, _) = agent_colors(item.agent);
        let rw = canvas.put_str(lx, y, 16, &role_ag, Style::new(ag_fg, bg).bold());
        lx = lx.saturating_add(rw + 1);
    } else {
        let icon = item.activity.icon(state.tick);
        canvas.set(lx, y, icon, Style::new(act_fg, bg).bold());
        lx += 2;

        let (ag_fg, _) = agent_colors(item.agent);
        let ag_w = canvas.put_str(lx, y, 9, item.agent_label(), Style::new(ag_fg, bg).bold());
        lx = lx.saturating_add(ag_w + 1);
    }

    let in_right = state.is_in_right_pane(item);
    let focus_btn = if item.is_hidden {
        " ◌ HIDDEN ↵ ".to_owned()
    } else if in_right {
        " ◉ RIGHT ".to_owned()
    } else {
        format!(" {} ↵ ", item.surface_badge())
    };
    let fb_w = focus_btn.chars().count() as u16;
    let max_proj_x = (rect.x + rect.w - 1).saturating_sub(fb_w + 1);
    if lx < max_proj_x && child_info.is_none() {
        let parent_tasks = state.parent_tasks_for(&item.terminal_key);
        let title_str = if parent_tasks.is_empty() {
            item.project.clone()
        } else {
            format!("{} ⑂{}", item.project, parent_tasks.len())
        };
        canvas.put_str(
            lx,
            y,
            max_proj_x - lx,
            &title_str,
            Style::new(palette::FG_BRIGHT, bg).bold(),
        );
    }
    if rect.w > fb_w + 6 {
        let fx = rect.x + rect.w - 1 - fb_w;
        let (fb_fg, fb_bg) = if item.is_hidden {
            (palette::WAITING_FG, palette::WAITING_BG)
        } else if in_right {
            (palette::WORKING_FG, palette::WORKING_BG)
        } else if selected {
            (palette::FG_BRIGHT, palette::BG_BRAND)
        } else {
            (palette::FG_BRIGHT, palette::BG_PILL)
        };
        canvas.put_str(fx, y, fb_w, &focus_btn, Style::new(fb_fg, fb_bg).bold());
        canvas.register_hit(Rect::new(fx, y, fb_w, 1), HitTarget::FocusCard(idx));
    }
}

fn render_sidebar_card_bottom(
    state: &AppState,
    rect: Rect,
    y: u16,
    idx: usize,
    item: &FleetItem,
    child_info: Option<(&HerdTask, bool)>,
    canvas: &mut Canvas,
) {
    let selected = state.selected_index == idx;
    let hovered = state.hover_pos.is_some_and(|(hx, hy)| {
        (hy == y || hy + 1 == y) && hx > rect.x && hx + 1 < rect.x + rect.w
    });
    let bg = if selected {
        palette::BG_CARD_SELECTED
    } else if hovered {
        palette::BG_CARD_HOVER
    } else {
        palette::BG_CARD
    };
    let row_rect = Rect::new(rect.x + 1, y, rect.w.saturating_sub(2), 1);
    canvas.fill_rect(row_rect, bg);
    canvas.register_hit(row_rect, HitTarget::SelectCard(idx));

    let (act_fg, _) = activity_colors(item.activity);
    let bar_fg = if selected {
        palette::BORDER_SELECTED
    } else if child_info.is_some() {
        palette::BRANCH_FG
    } else {
        act_fg
    };
    canvas.set(rect.x + 1, y, '▎', Style::new(bar_fg, bg).bold());

    if let Some((task, is_last)) = child_info {
        let prefix = if is_last { "     " } else { "│    " };
        let pw = canvas.put_str(rect.x + 3, y, 5, prefix, Style::new(palette::BRANCH_FG, bg));
        let summary = task
            .last_message
            .as_ref()
            .map(|m| m.body.as_str())
            .unwrap_or(&task.goal);
        let status_fg = match task.status {
            TaskStatus::Approved | TaskStatus::Done => palette::WORKING_FG,
            TaskStatus::ChangesRequested => palette::ATTENTION_FG,
            TaskStatus::WaitingParent => palette::WAITING_FG,
            TaskStatus::WaitingChild => palette::SHELL_FG,
        };
        let line = format!("{} · {}", task.status.badge(), summary);
        canvas.put_str(
            rect.x + 3 + pw,
            y,
            rect.w.saturating_sub(5 + pw),
            &line,
            Style::new(status_fg, bg),
        );
    } else {
        let mut l2 = String::new();
        if let Some(git) = &item.git {
            l2.push_str(&format!("⎇ {} · ", git.branch));
        }
        l2.push_str(&item.status_text);
        canvas.put_str(
            rect.x + 3,
            y,
            rect.w.saturating_sub(5),
            &l2,
            Style::new(palette::FG_SECONDARY, bg),
        );
    }
}

fn render_sidebar_scrollbar(
    rect: Rect,
    list_top: u16,
    list_bottom: u16,
    list_h: usize,
    scroll_y: usize,
    total_vlines: usize,
    canvas: &mut Canvas,
) {
    if total_vlines <= list_h || list_h < 2 {
        return;
    }
    let thumb_h = ((list_h * list_h) / total_vlines).max(1);
    let thumb_top = (scroll_y * list_h.saturating_sub(thumb_h))
        .checked_div(total_vlines.saturating_sub(list_h))
        .unwrap_or(0);
    for sy in 0..thumb_h {
        let ry = list_top + (thumb_top + sy) as u16;
        if ry < list_bottom {
            canvas.set(
                rect.x + rect.w - 1,
                ry,
                '┃',
                Style::new(palette::ACCENT_BRAND, palette::BG_COLUMN).bold(),
            );
        }
    }
}

fn render_sidebar_mini_inspector(
    state: &AppState,
    rect: Rect,
    iy: u16,
    item: &FleetItem,
    canvas: &mut Canvas,
) {
    let bstyle = Style::new(palette::BORDER_SUBTLE, palette::BG_INSPECTOR);
    canvas.fill_rect(
        Rect::new(rect.x + 1, iy, rect.w.saturating_sub(2), 3),
        palette::BG_INSPECTOR,
    );
    for x in (rect.x + 1)..(rect.x + rect.w - 1) {
        canvas.set(x, iy, '─', bstyle);
    }
    canvas.set(rect.x, iy, '├', bstyle);
    canvas.set(rect.x + rect.w - 1, iy, '┤', bstyle);
    if let Some(task) = state.child_task_for(&item.terminal_key).or_else(|| {
        state
            .parent_tasks_for(&item.terminal_key)
            .into_iter()
            .next()
    }) {
        let parent_lbl = task
            .parent_agent
            .map(|a| a.name().to_ascii_uppercase())
            .unwrap_or_else(|| "PARENT".into());
        let child_lbl = task.child_agent.name().to_ascii_uppercase();
        let title = format!(
            " ⑂ {} ⇄ {} ({}·R{}) ",
            parent_lbl,
            child_lbl,
            task.role.short_badge(),
            task.round
        );
        canvas.put_str(
            rect.x + 2,
            iy,
            rect.w.saturating_sub(4),
            &title,
            Style::new(palette::BRANCH_FG, palette::BG_INSPECTOR).bold(),
        );
        canvas.put_str(
            rect.x + 2,
            iy + 1,
            rect.w.saturating_sub(4),
            &format!("{} · {}", task.task_id, task.status.badge()),
            Style::new(palette::FG_BRIGHT, palette::BG_INSPECTOR),
        );
        let msg = task
            .last_message
            .as_ref()
            .map(|m| m.body.as_str())
            .unwrap_or(&task.goal);
        canvas.put_str(
            rect.x + 2,
            iy + 2,
            rect.w.saturating_sub(4),
            &format!("💬 {msg}"),
            Style::new(palette::FG_SECONDARY, palette::BG_INSPECTOR),
        );
    } else {
        let title = format!(" {} · {} ", item.agent_label(), item.surface_badge());
        canvas.put_str(
            rect.x + 2,
            iy,
            rect.w.saturating_sub(4),
            &title,
            Style::new(palette::FG_BRIGHT, palette::BG_INSPECTOR).bold(),
        );
        canvas.put_str(
            rect.x + 2,
            iy + 1,
            rect.w.saturating_sub(4),
            &format!("{}", item.cwd.display()),
            Style::new(palette::FG_PRIMARY, palette::BG_INSPECTOR),
        );
        let sid = item.short_session_id();
        let l3 = format!("ID: {sid} · {}", item.status_text);
        canvas.put_str(
            rect.x + 2,
            iy + 2,
            rect.w.saturating_sub(4),
            &l3,
            Style::new(palette::FG_SECONDARY, palette::BG_INSPECTOR),
        );
    }
}

fn render_inspector(state: &AppState, rect: Rect, canvas: &mut Canvas) {
    canvas.draw_box(
        rect,
        palette::BORDER_SUBTLE,
        palette::BG_INSPECTOR,
        true,
        Some(palette::ACCENT_BRAND),
    );
    let Some(item) = state.selected_item() else {
        let x0 = rect.x + 2;
        let max_w = rect.w.saturating_sub(4);
        canvas.put_str(
            x0,
            rect.y,
            max_w,
            " INSPECTOR · EMPTY FLEET ",
            Style::new(palette::FG_BRIGHT, palette::BG_INSPECTOR).bold(),
        );
        canvas.put_str(
            x0,
            rect.y + 1,
            max_w,
            "No active Ghostty terminals or saved agent sessions match the current view.",
            Style::new(palette::FG_PRIMARY, palette::BG_INSPECTOR),
        );
        if let Some(err) = &state.snapshot.ghostty_error {
            canvas.put_str(
                x0,
                rect.y + 2,
                max_w,
                &format!("Ghostty inventory note: {err}"),
                Style::new(palette::WAITING_FG, palette::BG_INSPECTOR),
            );
        }
        canvas.put_str(
            x0,
            rect.y + 3,
            max_w,
            "Tip: Press Shift+D (or run `ghostback ui --demo`) to preview an 8-agent Herdr fleet, or press `n` to spawn.",
            Style::new(palette::FG_SECONDARY, palette::BG_INSPECTOR),
        );
        return;
    };

    let x0 = rect.x + 2;
    let max_w = rect.w.saturating_sub(4);

    // Header on box top border
    let header = format!(
        " INSPECTOR · {} · {} ({}) ",
        item.agent_label(),
        item.project,
        item.surface_badge()
    );
    canvas.put_str(
        x0,
        rect.y,
        max_w,
        &header,
        Style::new(palette::FG_BRIGHT, palette::BG_INSPECTOR).bold(),
    );

    // Row 1: Path + Branch + Status
    let branch_info = item
        .git
        .as_ref()
        .map(|g| {
            if g.is_worktree {
                format!("  ⎇ {} (worktree)", g.branch)
            } else {
                format!("  ⎇ {}", g.branch)
            }
        })
        .unwrap_or_default();
    let row1 = format!(
        "Path: {}{}   Status: {}",
        item.cwd.display(),
        branch_info,
        item.status_text
    );
    canvas.put_str(
        x0,
        rect.y + 1,
        max_w,
        &row1,
        Style::new(palette::FG_PRIMARY, palette::BG_INSPECTOR),
    );

    // Row 2: Exact IDs & TTY/PID
    let sid = item.session_id.as_deref().unwrap_or("(unbound shell)");
    let pid_str = item
        .pid
        .map(|p| p.to_string())
        .unwrap_or_else(|| "—".into());
    let tty_str = if item.tty.is_empty() {
        "—"
    } else {
        &item.tty
    };
    let row2 = format!(
        "Conversation: {}   Terminal UUID: {}   TTY: {} (PID {})",
        sid, item.terminal_key, tty_str, pid_str
    );
    canvas.put_str(
        x0,
        rect.y + 2,
        max_w,
        &row2,
        Style::new(palette::FG_SECONDARY, palette::BG_INSPECTOR),
    );

    // Row 3: HerdTask (Parent/Child relationship) or Resume command preview
    if let Some(task) = state.child_task_for(&item.terminal_key) {
        let msg = task
            .last_message
            .as_ref()
            .map(|m| m.body.as_str())
            .unwrap_or(task.goal.as_str())
            .lines()
            .next()
            .unwrap_or("");
        let row3 = format!(
            "Task #{}: {} [R{} · {}] Parent: {} ({}) · \"{}\"",
            task.task_id,
            task.role.label(),
            task.round,
            task.status.badge(),
            task.parent_agent
                .map(|a| a.name().to_uppercase())
                .unwrap_or_else(|| "SHELL".into()),
            short_key(&task.parent_terminal_key),
            msg
        );
        canvas.put_str(
            x0,
            rect.y + 3,
            max_w,
            &row3,
            Style::new(palette::BRANCH_FG, palette::BG_INSPECTOR),
        );
    } else {
        let ptasks = state.parent_tasks_for(&item.terminal_key);
        if let Some(first_task) = ptasks.first() {
            let msg = first_task
                .last_message
                .as_ref()
                .map(|m| m.body.as_str())
                .unwrap_or(first_task.goal.as_str())
                .lines()
                .next()
                .unwrap_or("");
            let row3 = format!(
                "Parent of {} child task(s) · Latest #{}: {} ({}) [R{} · {}] · \"{}\"",
                ptasks.len(),
                first_task.task_id,
                first_task.child_agent.name().to_uppercase(),
                first_task.role.label(),
                first_task.round,
                first_task.status.badge(),
                msg
            );
            canvas.put_str(
                x0,
                rect.y + 3,
                max_w,
                &row3,
                Style::new(palette::BRANCH_FG, palette::BG_INSPECTOR),
            );
        } else {
            let cmd_preview = item.resume_command.as_deref().unwrap_or(
                "No saved resume command for this pane (ordinary shell or uncaptured session)",
            );
            canvas.put_str(
                x0,
                rect.y + 3,
                max_w,
                &format!("Command: {cmd_preview}"),
                Style::new(palette::FG_MUTED, palette::BG_INSPECTOR),
            );
        }
    }

    // Row 4: Clickable Inspector Action Bar
    if rect.h >= 6 {
        let mut bx = x0;
        let y = rect.y + 4;
        let buttons: &[(&str, HitTarget, Rgb)] = &[
            (
                "↵ Focus in Ghostty",
                HitTarget::FocusCard(state.selected_index),
                palette::FG_BRIGHT,
            ),
            (
                "◌ Hide (-)",
                HitTarget::HideCard(state.selected_index),
                palette::WAITING_FG,
            ),
            (
                "× Close (x)",
                HitTarget::CloseCard(state.selected_index),
                palette::ATTENTION_FG,
            ),
            ("⊞ Grid (m)", HitTarget::CycleGrid, palette::ACCENT_BRAND),
            (
                "⑂ Pair Child (p)",
                HitTarget::OpenPairModal,
                palette::BRANCH_FG,
            ),
            (
                "◫ Split Right",
                HitTarget::QuickSplitRight,
                palette::WORKING_FG,
            ),
            (
                "⬒ Split Down",
                HitTarget::QuickSplitDown,
                palette::WORKING_FG,
            ),
            ("+ New Tab", HitTarget::QuickNewTab, palette::SHELL_FG),
            (
                "✎ Rename Title (t)",
                HitTarget::OpenTitleModal,
                palette::WAITING_FG,
            ),
            (
                "↻ Retry (r)",
                HitTarget::TriggerRetrySelected,
                palette::ATTENTION_FG,
            ),
            (
                "⧉ Copy Cmd (c)",
                HitTarget::TriggerCopyCommand,
                palette::BRANCH_FG,
            ),
        ];
        for (label, target, fg) in buttons {
            if bx.saturating_add(label.chars().count() as u16 + 3) >= rect.x + rect.w - 1 {
                break;
            }
            let bg = if matches!(target, HitTarget::FocusCard(_)) {
                palette::BG_BRAND
            } else {
                palette::BG_PILL
            };
            let bw = canvas.draw_pill(
                bx,
                y,
                (rect.x + rect.w - 1).saturating_sub(bx),
                label,
                Style::new(*fg, bg).with_bold(matches!(target, HitTarget::FocusCard(_))),
            );
            canvas.register_hit(Rect::new(bx, y, bw, 1), target.clone());
            bx = bx.saturating_add(bw + 1);
        }
    }
}

fn render_footer(state: &AppState, view: ViewMode, y: u16, canvas: &mut Canvas) {
    let w = canvas.width;
    canvas.fill_rect(Rect::new(0, y, w, 1), palette::BG_HEADER);

    if let Some((msg, is_error, at)) = &state.toast
        && at.elapsed() < Duration::from_secs(5)
    {
        let fg = if *is_error {
            palette::ATTENTION_FG
        } else {
            palette::WORKING_FG
        };
        let prefix = if *is_error { " ▲ " } else { " ✓ " };
        canvas.put_str(
            1,
            y,
            w.saturating_sub(2),
            &format!("{prefix}{msg}"),
            Style::new(fg, palette::BG_HEADER).bold(),
        );
        return;
    }

    let hint = match view {
        ViewMode::Sidebar => {
            " ↵/Click:Show  Drag/J/K:Swap  -:Hide  x:Close  m:Grid(1×1..3×3)  p:Pair  n:New  q:Quit "
        }
        ViewMode::Board => {
            " ↵/DblClick:Focus  Drag/J/K:Swap  -:Hide  x:Close  m:Grid  p:Pair  n:Spawn  /:Filter  q:Quit "
        }
        ViewMode::Fleet => {
            " ↵/DblClick:Focus  J/K:Swap  -:Hide  x:Close  m:Grid  1/2/3:Views  p:Pair  n:Spawn  q:Quit "
        }
    };
    canvas.put_str(
        1,
        y,
        w.saturating_sub(2),
        hint,
        Style::new(palette::FG_SECONDARY, palette::BG_HEADER),
    );
}
