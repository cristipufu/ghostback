use std::path::Path;

use crate::herd::TaskRole;
use crate::macos::SpawnDisposition;
use crate::sessions::Agent;

use super::canvas::{Canvas, HitTarget, Rect, Style, palette};
use super::state::{CHILD_AGENT_CHOICES, Modal, SpawnTargetChoice, child_agent_label};

pub fn render_active_modal(modal: &Modal, canvas: &mut Canvas) {
    match modal {
        Modal::None | Modal::Filter => {}
        Modal::Help => render_help_modal(canvas),
        Modal::RenameTitle { buffer, .. } => render_title_modal(buffer, canvas),
        Modal::Spawn {
            cwd,
            disposition,
            choice_index,
            ..
        } => render_spawn_modal(cwd, *disposition, *choice_index, canvas),
        Modal::SpawnChild {
            parent_agent,
            cwd,
            role,
            agent_index,
            prompt_buffer,
            ..
        } => render_pair_modal(
            *parent_agent,
            cwd,
            *role,
            *agent_index,
            prompt_buffer,
            canvas,
        ),
    }
}

fn render_spawn_modal(
    cwd: &Path,
    disposition: SpawnDisposition,
    choice_index: usize,
    canvas: &mut Canvas,
) {
    let mw = 64u16.min(canvas.width.saturating_sub(4));
    let mh = 15u16.min(canvas.height.saturating_sub(2));
    let mx = (canvas.width.saturating_sub(mw)) / 2;
    let my = (canvas.height.saturating_sub(mh)) / 2;
    let rect = Rect::new(mx, my, mw, mh);

    canvas.draw_box(
        rect,
        palette::BORDER_MODAL,
        palette::BG_MODAL,
        true,
        Some(palette::WORKING_FG),
    );
    canvas.put_str(
        mx + 2,
        my,
        mw.saturating_sub(4),
        " + SPAWN NEW GHOSTTY SURFACE ",
        Style::new(palette::FG_BRIGHT, palette::BG_MODAL).bold(),
    );
    canvas.put_str(
        mx + 2,
        my + 1,
        mw.saturating_sub(4),
        &format!("Workspace: {} · ⚡ MCP Auto-Injected", cwd.display()),
        Style::new(palette::FG_SECONDARY, palette::BG_MODAL),
    );

    // Disposition selector pills
    let mut dx = mx + 2;
    let narrow = mw < 58;
    let disp_choices: [(SpawnDisposition, &str); 4] = if narrow {
        [
            (SpawnDisposition::SplitRight, "◨ Right"),
            (SpawnDisposition::SplitDown, "⬒ Down"),
            (SpawnDisposition::NewTab, "❐ Tab"),
            (SpawnDisposition::NewWindow, "❐ Win"),
        ]
    } else {
        [
            (
                SpawnDisposition::SplitRight,
                SpawnDisposition::SplitRight.label(),
            ),
            (
                SpawnDisposition::SplitDown,
                SpawnDisposition::SplitDown.label(),
            ),
            (SpawnDisposition::NewTab, SpawnDisposition::NewTab.label()),
            (
                SpawnDisposition::NewWindow,
                SpawnDisposition::NewWindow.label(),
            ),
        ]
    };
    for (disp, label) in disp_choices {
        let active = disp == disposition;
        let (fg, bg) = if active {
            (palette::FG_BRIGHT, palette::ACCENT_BRAND)
        } else {
            (palette::FG_SECONDARY, palette::BG_PILL)
        };
        let pw = canvas.draw_pill(
            dx,
            my + 3,
            (mx + mw - 2).saturating_sub(dx),
            label,
            Style::new(fg, bg).with_bold(active),
        );
        canvas.register_hit(
            Rect::new(dx, my + 3, pw, 1),
            HitTarget::ModalSelectDisposition(disp),
        );
        dx = dx.saturating_add(pw + 1);
    }

    // Target choices
    for (i, choice) in SpawnTargetChoice::ALL.iter().enumerate() {
        let cy = my + 5 + i as u16;
        if cy + 2 >= my + mh {
            break;
        }
        let selected = i == choice_index;
        let row_rect = Rect::new(mx + 2, cy, mw.saturating_sub(4), 1);
        let bg = if selected {
            palette::BG_CARD_SELECTED
        } else {
            palette::BG_MODAL
        };
        canvas.fill_rect(row_rect, bg);
        canvas.register_hit(row_rect, HitTarget::ModalSelectChoice(i));
        let prefix = if selected { "▸" } else { " " };
        canvas.put_str(
            mx + 3,
            cy,
            mw.saturating_sub(6),
            &format!("{prefix} {}. {}", i + 1, choice.label()),
            Style::new(
                if selected {
                    palette::FG_BRIGHT
                } else {
                    palette::FG_PRIMARY
                },
                bg,
            )
            .bold(),
        );
    }

    let fy = my + mh - 2;
    let lw = canvas.draw_pill(
        mx + 2,
        fy,
        20,
        "↵ Launch Surface",
        Style::new(palette::FG_BRIGHT, palette::WORKING_BG).bold(),
    );
    canvas.register_hit(Rect::new(mx + 2, fy, lw, 1), HitTarget::ModalConfirm);
    let cw = canvas.draw_pill(
        mx + 4 + lw,
        fy,
        16,
        "Esc Cancel",
        Style::new(palette::FG_SECONDARY, palette::BG_PILL),
    );
    canvas.register_hit(Rect::new(mx + 4 + lw, fy, cw, 1), HitTarget::ModalCancel);
}

fn render_title_modal(buffer: &str, canvas: &mut Canvas) {
    let mw = 58u16.min(canvas.width.saturating_sub(4));
    let mh = 7u16.min(canvas.height.saturating_sub(2));
    let mx = (canvas.width.saturating_sub(mw)) / 2;
    let my = (canvas.height.saturating_sub(mh)) / 2;
    let rect = Rect::new(mx, my, mw, mh);

    canvas.draw_box(
        rect,
        palette::BORDER_MODAL,
        palette::BG_MODAL,
        true,
        Some(palette::WAITING_FG),
    );
    canvas.put_str(
        mx + 2,
        my,
        mw.saturating_sub(4),
        " ✎ GHOSTTY TITLE OVERRIDE ",
        Style::new(palette::FG_BRIGHT, palette::BG_MODAL).bold(),
    );
    canvas.put_str(
        mx + 2,
        my + 1,
        mw.saturating_sub(4),
        "Type a custom terminal/tab title (leave empty to clear override):",
        Style::new(palette::FG_SECONDARY, palette::BG_MODAL),
    );
    let input_rect = Rect::new(mx + 2, my + 3, mw.saturating_sub(4), 1);
    canvas.fill_rect(input_rect, palette::BG_INPUT);
    canvas.put_str(
        mx + 3,
        my + 3,
        mw.saturating_sub(6),
        &format!("{buffer}█"),
        Style::new(palette::FG_BRIGHT, palette::BG_INPUT).bold(),
    );
    canvas.put_str(
        mx + 2,
        my + 5,
        mw.saturating_sub(4),
        "↵ Apply Override   Ctrl+U: Clear   Esc: Cancel",
        Style::new(palette::FG_MUTED, palette::BG_MODAL),
    );
}

fn render_pair_modal(
    parent_agent: Option<Agent>,
    cwd: &Path,
    role: TaskRole,
    agent_index: usize,
    prompt_buffer: &str,
    canvas: &mut Canvas,
) {
    let mw = 68u16.min(canvas.width.saturating_sub(2));
    let mh = 18u16.min(canvas.height.saturating_sub(2));
    let mx = (canvas.width.saturating_sub(mw)) / 2;
    let my = (canvas.height.saturating_sub(mh)) / 2;
    let rect = Rect::new(mx, my, mw, mh);

    canvas.draw_box(
        rect,
        palette::BORDER_MODAL,
        palette::BG_MODAL,
        true,
        Some(palette::BRANCH_FG),
    );
    let parent_lbl = parent_agent
        .map(|a| a.name().to_uppercase())
        .unwrap_or_else(|| "SHELL".into());
    canvas.put_str(
        mx + 2,
        my,
        mw.saturating_sub(4),
        &format!(" ⑂ PAIR CHILD AGENT (PARENT: {parent_lbl}) "),
        Style::new(palette::FG_BRIGHT, palette::BG_MODAL).bold(),
    );
    canvas.put_str(
        mx + 2,
        my + 1,
        mw.saturating_sub(4),
        &format!("Workspace: {} · ⚡ MCP Auto-Injected", cwd.display()),
        Style::new(palette::FG_SECONDARY, palette::BG_MODAL),
    );

    // Role selector pills (Tab / Left / Right)
    let mut rx = mx + 2;
    for r in TaskRole::ALL {
        let active = r == role;
        let (fg, bg) = if active {
            (palette::FG_BRIGHT, palette::ACCENT_BRAND)
        } else {
            (palette::FG_SECONDARY, palette::BG_PILL)
        };
        let label = if mw < 48 {
            r.label().to_owned()
        } else {
            format!("{} {}", r.short_badge(), r.label())
        };
        let pw = canvas.draw_pill(
            rx,
            my + 2,
            (mx + mw - 2).saturating_sub(rx),
            &label,
            Style::new(fg, bg).with_bold(active),
        );
        canvas.register_hit(Rect::new(rx, my + 2, pw, 1), HitTarget::ModalSelectRole(r));
        rx = rx.saturating_add(pw + 1);
    }

    // Child agent harness choices (Up / Down / 1..6)
    for (i, &agent) in CHILD_AGENT_CHOICES.iter().enumerate() {
        let cy = my + 3 + i as u16;
        if cy + 5 >= my + mh {
            break;
        }
        let selected = i == agent_index;
        let row_rect = Rect::new(mx + 2, cy, mw.saturating_sub(4), 1);
        let bg = if selected {
            palette::BG_CARD_SELECTED
        } else {
            palette::BG_MODAL
        };
        canvas.fill_rect(row_rect, bg);
        canvas.register_hit(row_rect, HitTarget::ModalSelectChoice(i));
        let prefix = if selected { "▸" } else { " " };
        canvas.put_str(
            mx + 3,
            cy,
            mw.saturating_sub(6),
            &format!("{prefix} {}. {}", i + 1, child_agent_label(agent)),
            Style::new(
                if selected {
                    palette::FG_BRIGHT
                } else {
                    palette::FG_PRIMARY
                },
                bg,
            )
            .bold(),
        );
    }

    // Prompt input box
    let py = (my + mh).saturating_sub(5);
    canvas.put_str(
        mx + 2,
        py,
        mw.saturating_sub(4),
        "Goal / Prompt (optional @model [effort] prefix, Ctrl+U clear):",
        Style::new(palette::FG_SECONDARY, palette::BG_MODAL),
    );
    let input_rect = Rect::new(mx + 2, py + 1, mw.saturating_sub(4), 1);
    canvas.fill_rect(input_rect, palette::BG_INPUT);
    let max_prompt_chars = mw.saturating_sub(7) as usize;
    let total_chars = prompt_buffer.chars().count();
    let tail: String = if total_chars > max_prompt_chars {
        prompt_buffer
            .chars()
            .skip(total_chars - max_prompt_chars)
            .collect()
    } else {
        prompt_buffer.to_owned()
    };
    canvas.put_str(
        mx + 3,
        py + 1,
        mw.saturating_sub(6),
        &format!("{tail}█"),
        Style::new(palette::FG_BRIGHT, palette::BG_INPUT).bold(),
    );

    let fy = my + mh - 2;
    let lw = canvas.draw_pill(
        mx + 2,
        fy,
        22,
        "↵ Delegate Child",
        Style::new(palette::FG_BRIGHT, palette::WORKING_BG).bold(),
    );
    canvas.register_hit(Rect::new(mx + 2, fy, lw, 1), HitTarget::ModalConfirm);
    let cw = canvas.draw_pill(
        mx + 4 + lw,
        fy,
        16,
        "Esc Cancel",
        Style::new(palette::FG_SECONDARY, palette::BG_PILL),
    );
    canvas.register_hit(Rect::new(mx + 4 + lw, fy, cw, 1), HitTarget::ModalCancel);
}

fn render_help_modal(canvas: &mut Canvas) {
    let mw = 68u16.min(canvas.width.saturating_sub(4));
    let mh = 17u16.min(canvas.height.saturating_sub(2));
    let mx = (canvas.width.saturating_sub(mw)) / 2;
    let my = (canvas.height.saturating_sub(mh)) / 2;
    let rect = Rect::new(mx, my, mw, mh);

    canvas.draw_box(
        rect,
        palette::BORDER_MODAL,
        palette::BG_MODAL,
        true,
        Some(palette::ACCENT_BRAND),
    );
    canvas.put_str(
        mx + 2,
        my,
        mw.saturating_sub(4),
        " ◈ GHOSTBACK HERD · KEYBOARD & MOUSE CONTROLS ",
        Style::new(palette::FG_BRIGHT, palette::BG_MODAL).bold(),
    );

    let lines = [
        "Enter / Click          Focus or re-attach hidden session in right pane",
        "Drag Card / J / K      Swap two panes or move session into target slot",
        "- or z                 Hide session from view (keeps agent alive in background)",
        "x                      Close & terminate session and its terminal pane",
        "m or [1×1..3×3]        Apply right-pane grid (1×1, 1×2, 2×1, 2×2, 3×3)",
        "j / k / ↑ / ↓ / Wheel  Move selection down / up",
        "1 / 2 / 3 or v         Switch between Board, Fleet, and Sidebar views",
        "u / Space              Jump to & focus next Waiting / Attention agent",
        "p / n                  Pair Child agent (p) or Spawn new surface (n)",
        "t / d                  Set title override (t) or Dock sidebar on left (d)",
        "r / R / c              Retry selected (r) / all (R), or Copy resume cmd (c)",
        "/ / a / g / w          Filter (/), Agents-only (a), Group (g), Scan (w)",
        "q / Esc                Close modal or quit UI",
    ];
    for (i, line) in lines.iter().enumerate() {
        let ly = my + 1 + i as u16;
        if ly + 1 >= my + mh {
            break;
        }
        canvas.put_str(
            mx + 2,
            ly,
            mw.saturating_sub(4),
            line,
            Style::new(palette::FG_PRIMARY, palette::BG_MODAL),
        );
    }
}
