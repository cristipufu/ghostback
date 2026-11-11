use crate::herd::TaskRole;
use crate::macos::SpawnDisposition;
use crate::sessions::Agent;

use super::snapshot::Activity;
use super::state::ViewMode;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

pub mod palette {
    use super::Rgb;
    pub const BG_CANVAS: Rgb = Rgb(11, 14, 20);
    pub const BG_HEADER: Rgb = Rgb(17, 22, 33);
    pub const BG_TOOLBAR: Rgb = Rgb(14, 18, 27);
    pub const BG_COLUMN: Rgb = Rgb(14, 18, 26);
    pub const BG_CARD: Rgb = Rgb(20, 26, 38);
    pub const BG_CARD_HOVER: Rgb = Rgb(26, 34, 50);
    pub const BG_CARD_SELECTED: Rgb = Rgb(30, 40, 62);
    pub const BG_INSPECTOR: Rgb = Rgb(15, 20, 30);
    pub const BG_MODAL: Rgb = Rgb(19, 25, 38);
    pub const BG_INPUT: Rgb = Rgb(10, 13, 20);
    pub const BG_PILL: Rgb = Rgb(28, 36, 52);
    pub const BG_PILL_HOVER: Rgb = Rgb(40, 52, 75);

    pub const BORDER_SUBTLE: Rgb = Rgb(35, 45, 64);
    pub const BORDER_HOVER: Rgb = Rgb(68, 85, 120);
    pub const BORDER_SELECTED: Rgb = Rgb(99, 102, 241);
    pub const BORDER_MODAL: Rgb = Rgb(129, 140, 248);

    pub const FG_PRIMARY: Rgb = Rgb(238, 242, 255);
    pub const FG_SECONDARY: Rgb = Rgb(158, 172, 199);
    pub const FG_MUTED: Rgb = Rgb(100, 116, 139);
    pub const FG_BRIGHT: Rgb = Rgb(255, 255, 255);

    pub const ACCENT_BRAND: Rgb = Rgb(99, 102, 241);
    pub const BG_BRAND: Rgb = Rgb(49, 46, 129);

    pub const WORKING_FG: Rgb = Rgb(52, 211, 153);
    pub const WORKING_BG: Rgb = Rgb(6, 52, 38);

    pub const WAITING_FG: Rgb = Rgb(251, 191, 36);
    pub const WAITING_BG: Rgb = Rgb(56, 36, 8);

    pub const ATTENTION_FG: Rgb = Rgb(251, 113, 133);
    pub const ATTENTION_BG: Rgb = Rgb(64, 14, 28);

    pub const SHELL_FG: Rgb = Rgb(56, 189, 248);
    pub const SHELL_BG: Rgb = Rgb(12, 42, 64);

    pub const SAVED_FG: Rgb = Rgb(148, 163, 184);
    pub const SAVED_BG: Rgb = Rgb(28, 35, 49);

    pub const BRANCH_FG: Rgb = Rgb(167, 139, 250);
    pub const BRANCH_BG: Rgb = Rgb(34, 24, 62);
}

pub fn activity_colors(activity: Activity) -> (Rgb, Rgb) {
    match activity {
        Activity::Working => (palette::WORKING_FG, palette::WORKING_BG),
        Activity::Waiting => (palette::WAITING_FG, palette::WAITING_BG),
        Activity::Attention => (palette::ATTENTION_FG, palette::ATTENTION_BG),
        Activity::Shell => (palette::SHELL_FG, palette::SHELL_BG),
        Activity::Saved => (palette::SAVED_FG, palette::SAVED_BG),
    }
}

pub fn agent_colors(agent: Option<Agent>) -> (Rgb, Rgb) {
    match agent {
        Some(Agent::Claude) => (Rgb(251, 146, 60), Rgb(54, 25, 10)),
        Some(Agent::Codex) => (Rgb(52, 211, 153), Rgb(8, 46, 34)),
        Some(Agent::Agy) => (Rgb(192, 132, 252), Rgb(42, 20, 68)),
        Some(Agent::Opencode) => (Rgb(34, 211, 238), Rgb(8, 44, 56)),
        Some(Agent::Pi) => (Rgb(244, 114, 182), Rgb(54, 16, 40)),
        Some(Agent::Grok) => (Rgb(226, 232, 240), Rgb(36, 44, 60)),
        None => (palette::SHELL_FG, palette::SHELL_BG),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Style {
    pub fg: Rgb,
    pub bg: Rgb,
    pub bold: bool,
    pub dim: bool,
    pub underline: bool,
}

impl Style {
    pub const fn new(fg: Rgb, bg: Rgb) -> Self {
        Self {
            fg,
            bg,
            bold: false,
            dim: false,
            underline: false,
        }
    }

    pub const fn bold(mut self) -> Self {
        self.bold = true;
        self
    }

    pub const fn with_bold(mut self, bold: bool) -> Self {
        self.bold = bold;
        self
    }

    pub const fn dim(mut self) -> Self {
        self.dim = true;
        self
    }

    pub const fn underline(mut self) -> Self {
        self.underline = true;
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cell {
    pub ch: char,
    pub style: Style,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            ch: ' ',
            style: Style::new(palette::FG_PRIMARY, palette::BG_CANVAS),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
}

impl Rect {
    pub const fn new(x: u16, y: u16, w: u16, h: u16) -> Self {
        Self { x, y, w, h }
    }

    pub const fn contains(self, col: u16, row: u16) -> bool {
        col >= self.x
            && col < self.x.saturating_add(self.w)
            && row >= self.y
            && row < self.y.saturating_add(self.h)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HitTarget {
    SelectCard(usize),
    FocusCard(usize),
    SetView(ViewMode),
    FilterStatus(Option<Activity>),
    ToggleAgentsOnly,
    ToggleShowSaved,
    ToggleSidebarGroup,
    JumpNextWaiting,
    OpenSpawnModal,
    OpenPairModal,
    OpenTitleModal,
    OpenFilterBar,
    ToggleHelp,
    TriggerRetrySelected,
    TriggerScan,
    TriggerDockSidebar,
    TriggerCopyCommand,
    QuickSplitRight,
    QuickSplitDown,
    QuickNewTab,
    ModalSelectDisposition(SpawnDisposition),
    ModalSelectRole(TaskRole),
    ModalSelectChoice(usize),
    ModalConfirm,
    ModalCancel,
}

pub struct Canvas {
    pub width: u16,
    pub height: u16,
    pub cells: Vec<Cell>,
    pub hits: Vec<(Rect, HitTarget)>,
}

impl Canvas {
    pub fn new(width: u16, height: u16) -> Self {
        let count = usize::from(width) * usize::from(height);
        Self {
            width,
            height,
            cells: vec![Cell::default(); count],
            hits: Vec::new(),
        }
    }

    pub fn register_hit(&mut self, rect: Rect, target: HitTarget) {
        if rect.w > 0 && rect.h > 0 {
            self.hits.push((rect, target));
        }
    }

    pub fn hit_at(&self, col: u16, row: u16) -> Option<&HitTarget> {
        self.hits
            .iter()
            .rev()
            .find(|(rect, _)| rect.contains(col, row))
            .map(|(_, target)| target)
    }

    pub fn set(&mut self, x: u16, y: u16, ch: char, style: Style) {
        if x < self.width && y < self.height {
            let idx = usize::from(y) * usize::from(self.width) + usize::from(x);
            if let Some(cell) = self.cells.get_mut(idx) {
                cell.ch = ch;
                cell.style = style;
            }
        }
    }

    pub fn fill_rect(&mut self, rect: Rect, bg: Rgb) {
        let style = Style::new(palette::FG_PRIMARY, bg);
        let max_y = rect.y.saturating_add(rect.h).min(self.height);
        let max_x = rect.x.saturating_add(rect.w).min(self.width);
        for y in rect.y..max_y {
            for x in rect.x..max_x {
                self.set(x, y, ' ', style);
            }
        }
    }

    pub fn draw_box(
        &mut self,
        rect: Rect,
        border_fg: Rgb,
        bg: Rgb,
        rounded: bool,
        left_accent: Option<Rgb>,
    ) {
        if rect.w < 2 || rect.h < 2 {
            self.fill_rect(rect, bg);
            return;
        }
        self.fill_rect(rect, bg);
        let bstyle = Style::new(border_fg, bg);
        let (tl, tr, bl, br) = if rounded {
            ('╭', '╮', '╰', '╯')
        } else {
            ('┌', '┐', '└', '┘')
        };
        let x2 = rect.x.saturating_add(rect.w).saturating_sub(1);
        let y2 = rect.y.saturating_add(rect.h).saturating_sub(1);

        for x in (rect.x + 1)..x2 {
            self.set(x, rect.y, '─', bstyle);
            self.set(x, y2, '─', bstyle);
        }
        for y in (rect.y + 1)..y2 {
            if let Some(accent) = left_accent {
                self.set(rect.x, y, '▎', Style::new(accent, bg).bold());
            } else {
                self.set(rect.x, y, '│', bstyle);
            }
            self.set(x2, y, '│', bstyle);
        }
        self.set(rect.x, rect.y, tl, bstyle);
        self.set(x2, rect.y, tr, bstyle);
        self.set(rect.x, y2, bl, bstyle);
        self.set(x2, y2, br, bstyle);
    }

    pub fn put_str(&mut self, x: u16, y: u16, max_w: u16, text: &str, style: Style) -> u16 {
        if y >= self.height || x >= self.width || max_w == 0 {
            return 0;
        }
        let limit = max_w.min(self.width.saturating_sub(x));
        if limit == 0 {
            return 0;
        }
        let chars: Vec<char> = text.chars().filter(|c| !c.is_control()).collect();
        if chars.len() <= usize::from(limit) {
            for (i, ch) in chars.iter().enumerate() {
                self.set(x + i as u16, y, *ch, style);
            }
            return chars.len() as u16;
        }
        if limit == 1 {
            self.set(x, y, '…', style);
            return 1;
        }
        let take = usize::from(limit - 1);
        for (i, ch) in chars.iter().take(take).enumerate() {
            self.set(x + i as u16, y, *ch, style);
        }
        if chars.get(take.saturating_sub(1)) != Some(&'…') {
            self.set(x + limit - 1, y, '…', style);
        }
        limit
    }

    pub fn draw_pill(&mut self, x: u16, y: u16, max_w: u16, label: &str, style: Style) -> u16 {
        if max_w < 3 || x >= self.width || y >= self.height {
            return 0;
        }
        let padded = format!(" {label} ");
        self.put_str(x, y, max_w, &padded, style)
    }

    /// Render the canvas using Ghostty's synchronized output mode (`?2026h` / `?2026l`).
    pub fn render_ansi(&self) -> String {
        let mut out = String::with_capacity(usize::from(self.width) * usize::from(self.height) * 4);
        out.push_str("\x1b[?2026h\x1b[H");
        for y in 0..self.height {
            if y > 0 {
                out.push_str(&format!("\x1b[{};1H", y + 1));
            }
            self.write_row(&mut out, y);
        }
        out.push_str("\x1b[0m\x1b[?2026l");
        out
    }

    /// Render a static snapshot suitable for standard output (`--once`).
    pub fn render_snapshot(&self, color: bool) -> String {
        let mut out = String::new();
        for y in 0..self.height {
            if color {
                self.write_row(&mut out, y);
                out.push_str("\x1b[0m\n");
            } else {
                let start = usize::from(y) * usize::from(self.width);
                let end = start + usize::from(self.width);
                let line: String = self.cells[start..end].iter().map(|c| c.ch).collect();
                out.push_str(line.trim_end());
                out.push('\n');
            }
        }
        out
    }

    fn write_row(&self, out: &mut String, y: u16) {
        use std::fmt::Write as _;
        let mut cur_style: Option<Style> = None;
        let row_start = usize::from(y) * usize::from(self.width);
        for x in 0..self.width {
            let cell = self.cells[row_start + usize::from(x)];
            if cur_style != Some(cell.style) {
                let s = cell.style;
                let _ = write!(
                    out,
                    "\x1b[0;{};38;2;{};{};{};48;2;{};{};{}m",
                    match (s.bold, s.dim, s.underline) {
                        (true, _, true) => "1;4",
                        (true, _, false) => "1",
                        (false, true, true) => "2;4",
                        (false, true, false) => "2",
                        (false, false, true) => "4",
                        (false, false, false) => "22",
                    },
                    s.fg.0,
                    s.fg.1,
                    s.fg.2,
                    s.bg.0,
                    s.bg.1,
                    s.bg.2
                );
                cur_style = Some(s);
            }
            out.push(cell.ch);
        }
    }
}
