use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use uuid::Uuid;

use crate::herd::{self, HerdTask, TaskRole};
use crate::macos::SpawnDisposition;
use crate::sessions::{Agent, shell_quote};

use super::canvas::{Canvas, HitTarget, Rgb, activity_colors, palette};
use super::demo::demo_snapshot;
use super::input::{KeyEvent, MouseEvent, MouseKind};
use super::snapshot::{Activity, FleetItem, FleetSnapshot};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewMode {
    Board,
    Fleet,
    Sidebar,
}

impl ViewMode {
    pub fn next(self) -> Self {
        match self {
            Self::Board => Self::Fleet,
            Self::Fleet => Self::Sidebar,
            Self::Sidebar => Self::Board,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidebarGroup {
    Status,
    Project,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GridLayout {
    Single,
    TwoCols,
    TwoRows,
    Grid2x2,
    Grid3x3,
}

impl GridLayout {
    pub const ALL: [Self; 5] = [
        Self::Single,
        Self::TwoCols,
        Self::TwoRows,
        Self::Grid2x2,
        Self::Grid3x3,
    ];

    pub const fn dimensions(self) -> (u8, u8) {
        match self {
            Self::Single => (1, 1),
            Self::TwoCols => (1, 2),
            Self::TwoRows => (2, 1),
            Self::Grid2x2 => (2, 2),
            Self::Grid3x3 => (3, 3),
        }
    }

    pub const fn cells(self) -> usize {
        let (r, c) = self.dimensions();
        (r as usize) * (c as usize)
    }

    pub const fn short_label(self) -> &'static str {
        match self {
            Self::Single => "1×1",
            Self::TwoCols => "1×2",
            Self::TwoRows => "2×1",
            Self::Grid2x2 => "2×2",
            Self::Grid3x3 => "3×3",
        }
    }

    pub const fn next(self) -> Self {
        match self {
            Self::Single => Self::TwoCols,
            Self::TwoCols => Self::TwoRows,
            Self::TwoRows => Self::Grid2x2,
            Self::Grid2x2 => Self::Grid3x3,
            Self::Grid3x3 => Self::Single,
        }
    }
}

pub struct SidebarRow<'a> {
    pub idx: usize,
    pub item: &'a FleetItem,
    pub child_info: Option<(&'a HerdTask, bool)>,
}

pub struct SidebarSection<'a> {
    pub title: String,
    pub fg: Rgb,
    pub rows: Vec<SidebarRow<'a>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Modal {
    None,
    Help,
    Filter,
    RenameTitle {
        terminal_key: String,
        buffer: String,
    },
    Spawn {
        cwd: PathBuf,
        target_terminal: Option<Uuid>,
        disposition: SpawnDisposition,
        choice_index: usize,
    },
    SpawnChild {
        parent_key: String,
        parent_agent: Option<Agent>,
        cwd: PathBuf,
        role: TaskRole,
        agent_index: usize,
        prompt_buffer: String,
    },
}

pub const CHILD_AGENT_CHOICES: [Agent; 6] = [
    Agent::Claude,
    Agent::Codex,
    Agent::Agy,
    Agent::Opencode,
    Agent::Pi,
    Agent::Grok,
];

pub fn child_agent_label(agent: Agent) -> &'static str {
    match agent {
        Agent::Claude => "Claude Code (claude)",
        Agent::Codex => "OpenAI Codex (codex)",
        Agent::Agy => "Antigravity CLI (agy)",
        Agent::Opencode => "OpenCode TUI (opencode)",
        Agent::Pi => "Pi Coding Agent (pi)",
        Agent::Grok => "Grok Build (grok)",
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpawnTargetChoice {
    Shell,
    Claude,
    Codex,
    Agy,
    Opencode,
    Pi,
    Grok,
}

impl SpawnTargetChoice {
    pub const ALL: [Self; 7] = [
        Self::Shell,
        Self::Claude,
        Self::Codex,
        Self::Agy,
        Self::Opencode,
        Self::Pi,
        Self::Grok,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Shell => "Interactive Shell ($SHELL)",
            Self::Claude => "Claude Code (claude)",
            Self::Codex => "OpenAI Codex (codex)",
            Self::Agy => "Antigravity CLI (agy)",
            Self::Opencode => "OpenCode TUI (opencode)",
            Self::Pi => "Pi Coding Agent (pi)",
            Self::Grok => "Grok Build (grok)",
        }
    }

    pub fn command_name(self) -> Option<&'static str> {
        match self {
            Self::Shell => None,
            Self::Claude => Some("claude"),
            Self::Codex => Some("codex"),
            Self::Agy => Some("agy"),
            Self::Opencode => Some("opencode"),
            Self::Pi => Some("pi"),
            Self::Grok => Some("grok"),
        }
    }
}

pub enum UiEffect {
    None,
    Refresh,
    Focus(Uuid),
    RenameTitle(Uuid, String),
    Spawn {
        target: Option<Uuid>,
        disposition: SpawnDisposition,
        cwd: PathBuf,
        command: Option<String>,
    },
    SpawnChild {
        parent_key: String,
        parent_agent: Option<Agent>,
        child_agent: Agent,
        role: TaskRole,
        goal: String,
        cwd: PathBuf,
    },
    Retry(Option<String>),
    DockSidebar,
    CopyCommand(String),
    HideSession(usize),
    CloseSession(usize),
    SwapSessions(usize, usize),
    ApplyGridLayout(GridLayout),
    AttachHiddenPty {
        pty_id: String,
        cwd: PathBuf,
        title: String,
    },
}

pub struct AppState {
    pub snapshot: FleetSnapshot,
    pub view: ViewMode,
    pub auto_sidebar: bool,
    pub sidebar_group: SidebarGroup,
    pub grid_layout: GridLayout,
    pub agents_only: bool,
    pub show_saved: bool,
    pub status_filter: Option<Activity>,
    pub filter_query: String,
    pub selected_index: usize,
    pub selected_key: Option<String>,
    pub hover_pos: Option<(u16, u16)>,
    pub drag_source: Option<usize>,
    pub drag_active: bool,
    pub last_click: Option<(usize, Instant)>,
    pub modal: Modal,
    pub toast: Option<(String, bool, Instant)>,
    pub tick: usize,
    pub should_quit: bool,
    pub self_key: Option<String>,
    pub state_dir: Option<PathBuf>,
    pub demo_mode: bool,
}

fn update_pair_modal_role(role: &mut TaskRole, prompt_buffer: &mut String, next_role: TaskRole) {
    *role = next_role;
    if prompt_buffer.starts_with("Review the current changes")
        || prompt_buffer.starts_with("Implement the requested")
        || prompt_buffer.starts_with("Analyze the current")
    {
        *prompt_buffer = match next_role {
            TaskRole::Reviewer => {
                "Review the current changes in this repository and report any bugs or approval."
                    .into()
            }
            TaskRole::Worker => {
                "Implement the requested task in this repository and report when complete.".into()
            }
            TaskRole::Advisor => {
                "Analyze the current architecture/problem and provide concrete recommendations."
                    .into()
            }
        };
    }
}

impl AppState {
    pub fn new(snapshot: FleetSnapshot, force_sidebar: bool, self_key: Option<String>) -> Self {
        let show_saved = !force_sidebar && snapshot.saved_count <= 5;
        let selected_key = std::env::var("GHOSTBACK_SELECT_KEY")
            .ok()
            .filter(|s| !s.is_empty());
        let state_dir = std::env::var("GHOSTBACK_STATE_DIR")
            .ok()
            .filter(|s| !s.is_empty())
            .map(PathBuf::from);
        let mut state = Self {
            snapshot,
            view: if force_sidebar {
                ViewMode::Sidebar
            } else {
                ViewMode::Board
            },
            auto_sidebar: !force_sidebar,
            sidebar_group: SidebarGroup::Status,
            grid_layout: GridLayout::Single,
            agents_only: false,
            show_saved,
            status_filter: None,
            filter_query: String::new(),
            selected_index: 0,
            selected_key,
            hover_pos: None,
            drag_source: None,
            drag_active: false,
            last_click: None,
            modal: Modal::None,
            toast: None,
            tick: 0,
            should_quit: false,
            self_key,
            state_dir,
            demo_mode: false,
        };
        if state.selected_key.is_none()
            && force_sidebar
            && let Some(right_key) = state
                .snapshot
                .items
                .iter()
                .find(|i| state.is_in_right_pane(i))
                .map(|i| i.terminal_key.clone())
        {
            state.selected_key = Some(right_key);
        }
        state.clamp_selection();
        state
    }

    pub fn self_tab_location(&self) -> Option<(u32, u32)> {
        let self_item = if let Some(ref sk) = self.self_key {
            self.snapshot
                .items
                .iter()
                .find(|i| i.terminal_key.eq_ignore_ascii_case(sk))
        } else {
            self.snapshot.items.iter().find(|i| i.is_self_ui)
        }?;
        Some((self_item.window_index?, self_item.tab_index?))
    }

    pub fn is_in_right_pane(&self, item: &FleetItem) -> bool {
        !item.is_self_ui
            && self
                .self_tab_location()
                .is_some_and(|(w, t)| item.window_index == Some(w) && item.tab_index == Some(t))
    }

    pub fn current_tab_right_pane_uuid(&self) -> Option<Uuid> {
        if let Some(sel) = self.selected_item()
            && self.is_in_right_pane(sel)
            && let Ok(uuid) = Uuid::parse_str(&sel.terminal_key)
        {
            return Some(uuid);
        }
        if let Some(item) = self
            .snapshot
            .items
            .iter()
            .find(|i| self.is_in_right_pane(i))
            && let Ok(uuid) = Uuid::parse_str(&item.terminal_key)
        {
            return Some(uuid);
        }
        self.self_key
            .as_deref()
            .and_then(|k| Uuid::parse_str(k).ok())
    }

    pub fn child_task_for(&self, terminal_key: &str) -> Option<&HerdTask> {
        let pty_alias = self
            .snapshot
            .items
            .iter()
            .find(|i| i.terminal_key.eq_ignore_ascii_case(terminal_key))
            .and_then(|i| i.pty_id.as_deref());
        self.snapshot.tasks.iter().find(|t| {
            t.child_terminal_key.eq_ignore_ascii_case(terminal_key)
                || pty_alias.is_some_and(|p| t.child_terminal_key.eq_ignore_ascii_case(p))
        })
    }

    pub fn parent_tasks_for(&self, terminal_key: &str) -> Vec<&HerdTask> {
        let pty_alias = self
            .snapshot
            .items
            .iter()
            .find(|i| i.terminal_key.eq_ignore_ascii_case(terminal_key))
            .and_then(|i| i.pty_id.as_deref());
        self.snapshot
            .tasks
            .iter()
            .filter(|t| {
                t.parent_terminal_key.eq_ignore_ascii_case(terminal_key)
                    || pty_alias.is_some_and(|p| t.parent_terminal_key.eq_ignore_ascii_case(p))
            })
            .collect()
    }

    pub fn is_attached_child_in(&self, item: &FleetItem, visible: &[(usize, &FleetItem)]) -> bool {
        let Some(task) = self.child_task_for(&item.terminal_key) else {
            return false;
        };
        !item.matches_key(&task.parent_terminal_key)
            && visible
                .iter()
                .any(|(_, p)| p.matches_key(&task.parent_terminal_key))
    }

    pub fn attached_children_of<'a>(
        &self,
        parent_key: &str,
        visible: &[(usize, &'a FleetItem)],
    ) -> Vec<(usize, &'a FleetItem)> {
        let parent_pty = self
            .snapshot
            .items
            .iter()
            .find(|i| i.terminal_key.eq_ignore_ascii_case(parent_key))
            .and_then(|i| i.pty_id.as_deref());
        visible
            .iter()
            .copied()
            .filter(|(_, candidate)| {
                !candidate.terminal_key.eq_ignore_ascii_case(parent_key)
                    && self
                        .child_task_for(&candidate.terminal_key)
                        .is_some_and(|t| {
                            t.parent_terminal_key.eq_ignore_ascii_case(parent_key)
                                || parent_pty
                                    .is_some_and(|p| t.parent_terminal_key.eq_ignore_ascii_case(p))
                        })
            })
            .collect()
    }

    pub fn effective_view(&self, width: u16) -> ViewMode {
        if self.auto_sidebar && width < 64 && self.view == ViewMode::Board {
            ViewMode::Sidebar
        } else {
            self.view
        }
    }

    pub fn visible_items(&self) -> Vec<(usize, &FleetItem)> {
        let has_live_non_self = self
            .snapshot
            .items
            .iter()
            .any(|i| i.activity != Activity::Saved && !i.is_self_ui);
        let has_any_live = self
            .snapshot
            .items
            .iter()
            .any(|i| i.activity != Activity::Saved);
        let hide_saved = !self.show_saved
            && self.status_filter != Some(Activity::Saved)
            && !self.demo_mode
            && has_any_live;
        let hide_self_in_sidebar =
            self.view == ViewMode::Sidebar && !self.show_saved && has_live_non_self;

        self.snapshot
            .items
            .iter()
            .enumerate()
            .filter(|(_, item)| {
                if hide_saved && item.activity == Activity::Saved {
                    return false;
                }
                if hide_self_in_sidebar && item.is_self_ui {
                    return false;
                }
                if self.agents_only && item.agent.is_none() {
                    return false;
                }
                if let Some(status) = self.status_filter
                    && item.activity != status
                {
                    return false;
                }
                item.matches_filter(&self.filter_query)
            })
            .collect()
    }

    /// Single source of truth for Sidebar grouping and Parent -> Child tree ordering,
    /// shared by both keyboard navigation (`visual_order`) and rendering (`render_sidebar_view`).
    pub fn sidebar_sections<'a>(&'a self) -> Vec<SidebarSection<'a>> {
        let visible = self.visible_items();
        let expand_section_rows = |roots: Vec<(usize, &'a FleetItem)>| -> Vec<SidebarRow<'a>> {
            let mut out = Vec::new();
            for (idx, item) in roots {
                out.push(SidebarRow {
                    idx,
                    item,
                    child_info: None,
                });
                let children = self.attached_children_of(&item.terminal_key, &visible);
                let total_c = children.len();
                for (ci, (c_idx, c_item)) in children.into_iter().enumerate() {
                    let is_last = ci + 1 == total_c;
                    let task = self.child_task_for(&c_item.terminal_key);
                    out.push(SidebarRow {
                        idx: c_idx,
                        item: c_item,
                        child_info: task.map(|t| (t, is_last)),
                    });
                }
            }
            out
        };

        match self.sidebar_group {
            SidebarGroup::Status => {
                let mut out = Vec::new();
                for act in [
                    Activity::Working,
                    Activity::Waiting,
                    Activity::Attention,
                    Activity::Shell,
                    Activity::Saved,
                ] {
                    let roots: Vec<_> = visible
                        .iter()
                        .copied()
                        .filter(|(_, item)| {
                            item.activity == act && !self.is_attached_child_in(item, &visible)
                        })
                        .collect();
                    let rows = expand_section_rows(roots);
                    if !rows.is_empty() {
                        let (fg, _) = activity_colors(act);
                        out.push(SidebarSection {
                            title: format!("▾ {} ({})", act.label(), rows.len()),
                            fg,
                            rows,
                        });
                    }
                }
                out
            }
            SidebarGroup::Project => {
                let mut map: BTreeMap<String, Vec<(usize, &FleetItem)>> = BTreeMap::new();
                for pair in &visible {
                    if !self.is_attached_child_in(pair.1, &visible) {
                        map.entry(pair.1.project.clone()).or_default().push(*pair);
                    }
                }
                map.into_iter()
                    .filter_map(|(proj, roots)| {
                        let rows = expand_section_rows(roots);
                        (!rows.is_empty()).then(|| SidebarSection {
                            title: format!("▾ {} ({})", proj, rows.len()),
                            fg: palette::BRANCH_FG,
                            rows,
                        })
                    })
                    .collect()
            }
        }
    }

    pub fn visual_order(&self, width: u16) -> Vec<usize> {
        if self.effective_view(width) == ViewMode::Sidebar {
            self.sidebar_sections()
                .into_iter()
                .flat_map(|sec| sec.rows.into_iter().map(|r| r.idx))
                .collect()
        } else {
            self.visible_items().into_iter().map(|(i, _)| i).collect()
        }
    }

    pub fn sync_selected_key(&mut self) {
        self.selected_key = self.selected_item().map(|i| i.terminal_key.clone());
    }

    pub fn clamp_selection(&mut self) {
        let visible = self.visible_items();
        if visible.is_empty() {
            self.selected_index = 0;
            self.selected_key = None;
            return;
        }
        if let Some(ref key) = self.selected_key
            && let Some((idx, _)) = visible.iter().find(|(_, item)| item.matches_key(key))
        {
            self.selected_index = *idx;
            return;
        }
        if !visible.iter().any(|(idx, _)| *idx == self.selected_index) {
            self.selected_index = visible[0].0;
        }
        self.sync_selected_key();
    }

    pub fn update_snapshot(&mut self, snapshot: FleetSnapshot) {
        self.snapshot = snapshot;
        self.clamp_selection();
    }

    pub fn selected_item(&self) -> Option<&FleetItem> {
        self.snapshot.items.get(self.selected_index)
    }

    pub fn set_toast(&mut self, msg: impl Into<String>, is_error: bool) {
        self.toast = Some((msg.into(), is_error, Instant::now()));
    }

    pub fn move_selection(&mut self, delta: isize) {
        self.move_selection_in_view(delta, 120);
    }

    pub fn move_selection_in_view(&mut self, delta: isize, width: u16) {
        let ordered = self.visual_order(width);
        if ordered.is_empty() {
            return;
        }
        let current_pos = ordered
            .iter()
            .position(|&i| i == self.selected_index)
            .unwrap_or(0);
        let len = ordered.len() as isize;
        let next_pos = (current_pos as isize + delta).rem_euclid(len) as usize;
        self.selected_index = ordered[next_pos];
        self.sync_selected_key();
    }

    pub fn move_board_column(&mut self, delta_col: isize) {
        let visible = self.visible_items();
        if visible.is_empty() {
            return;
        }
        let current_col = self
            .selected_item()
            .map(|item| item.activity.column_index())
            .unwrap_or(0);
        for step in 1..=4 {
            let next_col = (current_col as isize + delta_col * step).rem_euclid(4) as usize;
            if let Some((idx, _)) = visible
                .iter()
                .find(|(_, item)| item.activity.column_index() == next_col)
            {
                self.selected_index = *idx;
                self.sync_selected_key();
                return;
            }
        }
    }

    pub fn move_within_board_column(&mut self, delta: isize) {
        let visible = self.visible_items();
        let Some(current) = self.selected_item() else {
            return;
        };
        let col = current.activity.column_index();
        let col_items: Vec<usize> = visible
            .into_iter()
            .filter(|(_, item)| item.activity.column_index() == col)
            .map(|(i, _)| i)
            .collect();
        if col_items.is_empty() {
            return;
        }
        let pos = col_items
            .iter()
            .position(|&i| i == self.selected_index)
            .unwrap_or(0);
        let next = (pos as isize + delta).rem_euclid(col_items.len() as isize) as usize;
        self.selected_index = col_items[next];
        self.sync_selected_key();
    }

    pub fn jump_next_waiting(&mut self) -> Option<usize> {
        let visible = self.visible_items();
        let urgent: Vec<usize> = visible
            .into_iter()
            .filter(|(_, item)| {
                !item.is_self_ui && matches!(item.activity, Activity::Attention | Activity::Waiting)
            })
            .map(|(i, _)| i)
            .collect();
        if urgent.is_empty() {
            return None;
        }
        let next = urgent
            .iter()
            .copied()
            .find(|&i| i > self.selected_index)
            .unwrap_or(urgent[0]);
        self.selected_index = next;
        self.sync_selected_key();
        Some(next)
    }

    pub fn current_tab_spawn_target(&self) -> Option<Uuid> {
        self.current_tab_right_pane_uuid()
            .or_else(|| {
                self.self_key
                    .as_deref()
                    .and_then(|k| Uuid::parse_str(k).ok())
            })
            .or_else(|| {
                self.selected_item()
                    .and_then(|item| Uuid::parse_str(&item.terminal_key).ok())
            })
    }

    pub fn open_spawn_modal(&mut self) {
        let cwd = self
            .selected_item()
            .map(|item| item.cwd.clone())
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")));
        let target = self.current_tab_spawn_target();
        self.modal = Modal::Spawn {
            cwd,
            target_terminal: target,
            disposition: SpawnDisposition::SplitRight,
            choice_index: 0,
        };
    }

    pub fn open_pair_modal(&mut self) {
        let Some(item) = self.selected_item() else {
            self.set_toast("Select a parent terminal first to pair a child agent", true);
            return;
        };
        let parent_key = item.binding_key().to_owned();
        let parent_agent = item.agent;
        let cwd = item.cwd.clone();
        let rec = herd::recommended_child_agent(parent_agent);
        let agent_index = CHILD_AGENT_CHOICES
            .iter()
            .position(|&a| a == rec)
            .unwrap_or(0);
        self.modal = Modal::SpawnChild {
            parent_key,
            parent_agent,
            cwd,
            role: TaskRole::Reviewer,
            agent_index,
            prompt_buffer: String::new(),
        };
    }

    pub fn open_title_modal(&mut self) {
        let Some(item) = self.selected_item() else {
            self.set_toast("No terminal selected", true);
            return;
        };
        if item.window_index.is_none() {
            self.set_toast("Cannot set title override on an offline terminal", true);
            return;
        }
        self.modal = Modal::RenameTitle {
            terminal_key: item.terminal_key.clone(),
            buffer: item.title.clone(),
        };
    }

    pub fn confirm_active_modal(&mut self) -> UiEffect {
        match std::mem::replace(&mut self.modal, Modal::None) {
            Modal::RenameTitle {
                terminal_key,
                buffer,
            } => {
                if let Ok(uuid) = Uuid::parse_str(&terminal_key) {
                    UiEffect::RenameTitle(uuid, buffer.trim().to_owned())
                } else {
                    UiEffect::None
                }
            }
            Modal::Spawn {
                cwd,
                target_terminal,
                disposition,
                choice_index,
            } => {
                let choice = SpawnTargetChoice::ALL[choice_index];
                let state_dir = self.state_dir.as_deref();
                UiEffect::Spawn {
                    target: target_terminal,
                    disposition,
                    cwd,
                    command: choice
                        .command_name()
                        .map(|bin| shell_launch_command(bin, state_dir)),
                }
            }
            Modal::SpawnChild {
                parent_key,
                parent_agent,
                cwd,
                role,
                agent_index,
                prompt_buffer,
            } => {
                let goal = if prompt_buffer.trim().is_empty() {
                    role.default_goal().to_owned()
                } else {
                    prompt_buffer.trim().to_owned()
                };
                UiEffect::SpawnChild {
                    parent_key,
                    parent_agent,
                    child_agent: CHILD_AGENT_CHOICES[agent_index],
                    role,
                    goal,
                    cwd,
                }
            }
            Modal::Help | Modal::Filter | Modal::None => UiEffect::None,
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent, width: u16) -> UiEffect {
        if let Some(effect) = self.handle_modal_key(&key) {
            return effect;
        }
        self.handle_view_key(key, width)
    }

    fn handle_modal_key(&mut self, key: &KeyEvent) -> Option<UiEffect> {
        match &mut self.modal {
            Modal::None => None,
            Modal::Help => {
                if matches!(
                    key,
                    KeyEvent::Escape | KeyEvent::Enter | KeyEvent::Char('?' | 'q')
                ) {
                    self.modal = Modal::None;
                }
                Some(UiEffect::None)
            }
            Modal::Filter => {
                match *key {
                    KeyEvent::Escape | KeyEvent::Enter => {
                        self.modal = Modal::None;
                    }
                    KeyEvent::Backspace => {
                        self.filter_query.pop();
                        self.clamp_selection();
                    }
                    KeyEvent::Char(ch) if !ch.is_control() => {
                        self.filter_query.push(ch);
                        self.clamp_selection();
                    }
                    KeyEvent::CtrlC => self.should_quit = true,
                    _ => {}
                }
                Some(UiEffect::None)
            }
            Modal::RenameTitle { buffer, .. } => {
                match *key {
                    KeyEvent::Escape => {
                        self.modal = Modal::None;
                    }
                    KeyEvent::Enter => {
                        return Some(self.confirm_active_modal());
                    }
                    KeyEvent::Backspace => {
                        buffer.pop();
                    }
                    KeyEvent::Char('\u{15}') => {
                        buffer.clear();
                    }
                    KeyEvent::Char(ch) if !ch.is_control() && buffer.len() < 120 => {
                        buffer.push(ch);
                    }
                    KeyEvent::CtrlC => self.should_quit = true,
                    _ => {}
                }
                Some(UiEffect::None)
            }
            Modal::Spawn {
                disposition,
                choice_index,
                ..
            } => {
                match *key {
                    KeyEvent::Escape | KeyEvent::Char('q') => {
                        self.modal = Modal::None;
                    }
                    KeyEvent::Tab | KeyEvent::Right | KeyEvent::Char('l') => {
                        *disposition = match disposition {
                            SpawnDisposition::SplitRight | SpawnDisposition::SidebarRight => {
                                SpawnDisposition::SplitDown
                            }
                            SpawnDisposition::SplitDown => SpawnDisposition::NewTab,
                            SpawnDisposition::NewTab => SpawnDisposition::NewWindow,
                            SpawnDisposition::NewWindow | SpawnDisposition::SplitLeft => {
                                SpawnDisposition::SplitRight
                            }
                        };
                    }
                    KeyEvent::BackTab | KeyEvent::Left | KeyEvent::Char('h') => {
                        *disposition = match disposition {
                            SpawnDisposition::SplitRight | SpawnDisposition::SidebarRight => {
                                SpawnDisposition::NewWindow
                            }
                            SpawnDisposition::SplitDown => SpawnDisposition::SplitRight,
                            SpawnDisposition::NewTab => SpawnDisposition::SplitDown,
                            SpawnDisposition::NewWindow | SpawnDisposition::SplitLeft => {
                                SpawnDisposition::NewTab
                            }
                        };
                    }
                    KeyEvent::Down | KeyEvent::Char('j') => {
                        *choice_index = (*choice_index + 1) % SpawnTargetChoice::ALL.len();
                    }
                    KeyEvent::Up | KeyEvent::Char('k') => {
                        *choice_index = (*choice_index + SpawnTargetChoice::ALL.len() - 1)
                            % SpawnTargetChoice::ALL.len();
                    }
                    KeyEvent::Char(c @ '1'..='7') => {
                        let idx = (c as usize) - ('1' as usize);
                        if idx < SpawnTargetChoice::ALL.len() {
                            *choice_index = idx;
                        }
                    }
                    KeyEvent::Enter => {
                        return Some(self.confirm_active_modal());
                    }
                    KeyEvent::CtrlC => self.should_quit = true,
                    _ => {}
                }
                Some(UiEffect::None)
            }
            Modal::SpawnChild {
                role,
                agent_index,
                prompt_buffer,
                ..
            } => {
                match *key {
                    KeyEvent::Escape => {
                        self.modal = Modal::None;
                    }
                    KeyEvent::Tab | KeyEvent::Right => {
                        let next = role.next();
                        update_pair_modal_role(role, prompt_buffer, next);
                    }
                    KeyEvent::BackTab | KeyEvent::Left => {
                        let prev = role.prev();
                        update_pair_modal_role(role, prompt_buffer, prev);
                    }
                    KeyEvent::Down => {
                        *agent_index = (*agent_index + 1) % CHILD_AGENT_CHOICES.len();
                    }
                    KeyEvent::Up => {
                        *agent_index = (*agent_index + CHILD_AGENT_CHOICES.len() - 1)
                            % CHILD_AGENT_CHOICES.len();
                    }
                    KeyEvent::Char(c @ '1'..='6') if prompt_buffer.is_empty() => {
                        let idx = (c as usize) - ('1' as usize);
                        if idx < CHILD_AGENT_CHOICES.len() {
                            *agent_index = idx;
                        }
                    }
                    KeyEvent::Backspace => {
                        prompt_buffer.pop();
                    }
                    KeyEvent::Char('\u{15}') => {
                        prompt_buffer.clear();
                    }
                    KeyEvent::Char(ch) if !ch.is_control() && prompt_buffer.len() < 400 => {
                        prompt_buffer.push(ch);
                    }
                    KeyEvent::Enter => {
                        return Some(self.confirm_active_modal());
                    }
                    KeyEvent::CtrlC => self.should_quit = true,
                    _ => {}
                }
                Some(UiEffect::None)
            }
        }
    }

    fn handle_view_key(&mut self, key: KeyEvent, width: u16) -> UiEffect {
        let view = self.effective_view(width);
        match key {
            KeyEvent::CtrlC | KeyEvent::Char('q') => {
                self.should_quit = true;
            }
            KeyEvent::Escape => {
                if !self.filter_query.is_empty() || self.status_filter.is_some() {
                    self.filter_query.clear();
                    self.status_filter = None;
                    self.clamp_selection();
                } else {
                    self.set_toast("Press q or Ctrl+C to quit Ghostback Herd", false);
                }
            }
            KeyEvent::Char('1') => {
                self.auto_sidebar = false;
                self.view = ViewMode::Board;
            }
            KeyEvent::Char('2') => {
                self.auto_sidebar = false;
                self.view = ViewMode::Fleet;
            }
            KeyEvent::Char('3') => {
                self.auto_sidebar = false;
                self.view = ViewMode::Sidebar;
            }
            KeyEvent::Char('v') | KeyEvent::Tab => {
                self.auto_sidebar = false;
                self.view = view.next();
            }
            KeyEvent::Down | KeyEvent::Char('j') => {
                if view == ViewMode::Board {
                    self.move_within_board_column(1);
                } else {
                    self.move_selection_in_view(1, width);
                }
            }
            KeyEvent::Up | KeyEvent::Char('k') => {
                if view == ViewMode::Board {
                    self.move_within_board_column(-1);
                } else {
                    self.move_selection_in_view(-1, width);
                }
            }
            KeyEvent::PageDown => {
                self.move_selection_in_view(5, width);
            }
            KeyEvent::PageUp => {
                self.move_selection_in_view(-5, width);
            }
            KeyEvent::Home => {
                let ordered = self.visual_order(width);
                if let Some(&first) = ordered.first() {
                    self.selected_index = first;
                    self.sync_selected_key();
                }
            }
            KeyEvent::End | KeyEvent::Char('G') => {
                let ordered = self.visual_order(width);
                if let Some(&last) = ordered.last() {
                    self.selected_index = last;
                    self.sync_selected_key();
                }
            }
            KeyEvent::Right | KeyEvent::Char('l') => {
                if view == ViewMode::Board {
                    self.move_board_column(1);
                } else {
                    self.move_selection_in_view(1, width);
                }
            }
            KeyEvent::Left | KeyEvent::Char('h') => {
                if view == ViewMode::Board {
                    self.move_board_column(-1);
                } else {
                    self.move_selection_in_view(-1, width);
                }
            }
            KeyEvent::Enter => {
                return self.focus_or_resume_item(self.selected_index);
            }
            KeyEvent::Char('u' | ' ') => {
                if let Some(idx) = self.jump_next_waiting() {
                    if let Some(item) = self.snapshot.items.get(idx)
                        && item.window_index.is_some()
                        && let Ok(uuid) = Uuid::parse_str(&item.terminal_key)
                    {
                        return UiEffect::Focus(uuid);
                    }
                } else {
                    self.set_toast("No waiting or attention agents right now", false);
                }
            }
            KeyEvent::Char('p') => self.open_pair_modal(),
            KeyEvent::Char('n') => self.open_spawn_modal(),
            KeyEvent::Char('t') => self.open_title_modal(),
            KeyEvent::Char('/') => {
                self.modal = Modal::Filter;
            }
            KeyEvent::Char('?') => {
                self.modal = Modal::Help;
            }
            KeyEvent::Char('a') => {
                self.agents_only = !self.agents_only;
                self.clamp_selection();
                self.set_toast(
                    if self.agents_only {
                        "Filter: Showing agent sessions only"
                    } else {
                        "Filter: Showing all Ghostty terminals & sessions"
                    },
                    false,
                );
            }
            KeyEvent::Char('s') => {
                self.show_saved = !self.show_saved;
                self.clamp_selection();
                self.set_toast(
                    if self.show_saved {
                        format!(
                            "Showing {} saved/retired sessions (press s to hide)",
                            self.snapshot.saved_count
                        )
                    } else {
                        "Hiding historical retired sessions (press s to show)".to_owned()
                    },
                    false,
                );
            }
            KeyEvent::Char('g') => {
                self.sidebar_group = match self.sidebar_group {
                    SidebarGroup::Status => SidebarGroup::Project,
                    SidebarGroup::Project => SidebarGroup::Status,
                };
            }
            KeyEvent::Char('r') => {
                if let Some(item) = self.selected_item() {
                    return UiEffect::Retry(Some(item.binding_key().to_owned()));
                }
            }
            KeyEvent::Char('R') => {
                return UiEffect::Retry(None);
            }
            KeyEvent::Char('w') => {
                self.demo_mode = false;
                return UiEffect::Refresh;
            }
            KeyEvent::Char('D') => {
                self.demo_mode = !self.demo_mode;
                if self.demo_mode {
                    self.update_snapshot(demo_snapshot());
                    self.selected_index = 0;
                    self.sync_selected_key();
                    self.set_toast(
                        "Demo mode enabled (8 sample agents). Press Shift+D or w for live Ghostty.",
                        false,
                    );
                } else {
                    return UiEffect::Refresh;
                }
            }
            KeyEvent::Char('d') => {
                return UiEffect::DockSidebar;
            }
            KeyEvent::Char('c') => {
                if let Some(item) = self.selected_item() {
                    if let Some(cmd) = &item.resume_command {
                        return UiEffect::CopyCommand(cmd.clone());
                    }
                    self.set_toast("Selected terminal has no saved resume command", true);
                }
            }
            KeyEvent::Char('-' | 'z') => {
                if self.selected_item().is_some() {
                    return UiEffect::HideSession(self.selected_index);
                }
            }
            KeyEvent::Char('x') => {
                if self.selected_item().is_some() {
                    return UiEffect::CloseSession(self.selected_index);
                }
            }
            KeyEvent::Char('J') => {
                return self.swap_with_neighbor(1, width);
            }
            KeyEvent::Char('K') => {
                return self.swap_with_neighbor(-1, width);
            }
            KeyEvent::Char('m') => {
                let next = self.grid_layout.next();
                self.grid_layout = next;
                return UiEffect::ApplyGridLayout(next);
            }
            _ => {}
        }
        UiEffect::None
    }

    fn swap_with_neighbor(&mut self, delta: isize, width: u16) -> UiEffect {
        let ordered = self.visual_order(width);
        if ordered.len() < 2 {
            return UiEffect::None;
        }
        let current_pos = ordered
            .iter()
            .position(|&i| i == self.selected_index)
            .unwrap_or(0);
        let len = ordered.len() as isize;
        let neighbor_pos = (current_pos as isize + delta).rem_euclid(len) as usize;
        let src = self.selected_index;
        let dst = ordered[neighbor_pos];
        if src == dst {
            return UiEffect::None;
        }
        UiEffect::SwapSessions(src, dst)
    }

    pub fn handle_mouse(&mut self, ev: MouseEvent, canvas: &Canvas) -> UiEffect {
        match ev.kind {
            MouseKind::Move => {
                self.hover_pos = Some((ev.col, ev.row));
                UiEffect::None
            }
            MouseKind::Drag => {
                self.hover_pos = Some((ev.col, ev.row));
                if let Some(src) = self.drag_source
                    && let Some(HitTarget::SelectCard(dst) | HitTarget::FocusCard(dst)) =
                        canvas.hit_at(ev.col, ev.row)
                    && *dst != src
                {
                    self.drag_active = true;
                }
                UiEffect::None
            }
            MouseKind::LeftUp => {
                self.hover_pos = Some((ev.col, ev.row));
                let src = self.drag_source.take();
                let was_dragging = std::mem::replace(&mut self.drag_active, false);
                if was_dragging
                    && let Some(src_idx) = src
                    && let Some(HitTarget::SelectCard(dst_idx) | HitTarget::FocusCard(dst_idx)) =
                        canvas.hit_at(ev.col, ev.row)
                    && *dst_idx != src_idx
                {
                    let dst = *dst_idx;
                    self.selected_index = dst;
                    self.sync_selected_key();
                    return UiEffect::SwapSessions(src_idx, dst);
                }
                UiEffect::None
            }
            MouseKind::ScrollUp => {
                self.move_selection_in_view(-1, canvas.width);
                UiEffect::None
            }
            MouseKind::ScrollDown => {
                self.move_selection_in_view(1, canvas.width);
                UiEffect::None
            }
            MouseKind::LeftDown => {
                self.hover_pos = Some((ev.col, ev.row));
                let Some(hit) = canvas.hit_at(ev.col, ev.row).cloned() else {
                    self.drag_source = None;
                    self.drag_active = false;
                    return UiEffect::None;
                };
                if let HitTarget::SelectCard(idx) = hit {
                    self.drag_source = Some(idx);
                    self.drag_active = false;
                } else {
                    self.drag_source = None;
                    self.drag_active = false;
                }
                self.apply_hit(hit)
            }
        }
    }

    pub fn apply_hit(&mut self, hit: HitTarget) -> UiEffect {
        match hit {
            HitTarget::SelectCard(idx) => {
                let is_double = self.last_click.is_some_and(|(prev, at)| {
                    prev == idx && at.elapsed() < Duration::from_millis(400)
                });
                self.selected_index = idx;
                self.sync_selected_key();
                self.last_click = Some((idx, Instant::now()));
                if self.view == ViewMode::Sidebar || is_double {
                    return self.focus_or_resume_item(idx);
                }
                UiEffect::None
            }
            HitTarget::FocusCard(idx) => {
                self.selected_index = idx;
                self.sync_selected_key();
                self.focus_or_resume_item(idx)
            }
            HitTarget::HideCard(idx) => {
                self.selected_index = idx;
                self.sync_selected_key();
                UiEffect::HideSession(idx)
            }
            HitTarget::CloseCard(idx) => {
                self.selected_index = idx;
                self.sync_selected_key();
                UiEffect::CloseSession(idx)
            }
            HitTarget::ApplyGrid(layout) => {
                self.grid_layout = layout;
                UiEffect::ApplyGridLayout(layout)
            }
            HitTarget::CycleGrid => {
                let next = self.grid_layout.next();
                self.grid_layout = next;
                UiEffect::ApplyGridLayout(next)
            }
            HitTarget::SetView(v) => {
                self.auto_sidebar = false;
                self.view = v;
                self.clamp_selection();
                UiEffect::None
            }
            HitTarget::FilterStatus(status) => {
                self.status_filter = status;
                self.clamp_selection();
                UiEffect::None
            }
            HitTarget::ToggleAgentsOnly => {
                self.agents_only = !self.agents_only;
                self.clamp_selection();
                UiEffect::None
            }
            HitTarget::ToggleShowSaved => {
                self.show_saved = !self.show_saved;
                self.clamp_selection();
                UiEffect::None
            }
            HitTarget::ToggleSidebarGroup => {
                self.sidebar_group = match self.sidebar_group {
                    SidebarGroup::Status => SidebarGroup::Project,
                    SidebarGroup::Project => SidebarGroup::Status,
                };
                UiEffect::None
            }
            HitTarget::JumpNextWaiting => {
                if let Some(idx) = self.jump_next_waiting()
                    && let Some(item) = self.snapshot.items.get(idx)
                    && item.window_index.is_some()
                    && let Ok(uuid) = Uuid::parse_str(&item.terminal_key)
                {
                    return UiEffect::Focus(uuid);
                }
                UiEffect::None
            }
            HitTarget::OpenSpawnModal => {
                self.open_spawn_modal();
                UiEffect::None
            }
            HitTarget::OpenPairModal => {
                self.open_pair_modal();
                UiEffect::None
            }
            HitTarget::OpenTitleModal => {
                self.open_title_modal();
                UiEffect::None
            }
            HitTarget::OpenFilterBar => {
                self.modal = Modal::Filter;
                UiEffect::None
            }
            HitTarget::ToggleHelp => {
                self.modal = if self.modal == Modal::Help {
                    Modal::None
                } else {
                    Modal::Help
                };
                UiEffect::None
            }
            HitTarget::TriggerRetrySelected => {
                if let Some(item) = self.selected_item() {
                    return UiEffect::Retry(Some(item.binding_key().to_owned()));
                }
                UiEffect::None
            }
            HitTarget::TriggerScan => UiEffect::Refresh,
            HitTarget::TriggerDockSidebar => UiEffect::DockSidebar,
            HitTarget::TriggerCopyCommand => {
                if let Some(item) = self.selected_item()
                    && let Some(cmd) = &item.resume_command
                {
                    return UiEffect::CopyCommand(cmd.clone());
                }
                self.set_toast("No saved resume command for this terminal", true);
                UiEffect::None
            }
            HitTarget::QuickSplitRight => self.quick_spawn(SpawnDisposition::SplitRight),
            HitTarget::QuickSplitDown => self.quick_spawn(SpawnDisposition::SplitDown),
            HitTarget::QuickNewTab => self.quick_spawn(SpawnDisposition::NewTab),
            HitTarget::ModalSelectDisposition(d) => {
                if let Modal::Spawn { disposition, .. } = &mut self.modal {
                    *disposition = d;
                }
                UiEffect::None
            }
            HitTarget::ModalSelectRole(r) => {
                if let Modal::SpawnChild {
                    role,
                    prompt_buffer,
                    ..
                } = &mut self.modal
                {
                    update_pair_modal_role(role, prompt_buffer, r);
                }
                UiEffect::None
            }
            HitTarget::ModalSelectChoice(idx) => {
                match &mut self.modal {
                    Modal::Spawn { choice_index, .. } if idx < SpawnTargetChoice::ALL.len() => {
                        *choice_index = idx;
                    }
                    Modal::SpawnChild { agent_index, .. } if idx < CHILD_AGENT_CHOICES.len() => {
                        *agent_index = idx;
                    }
                    _ => {}
                }
                UiEffect::None
            }
            HitTarget::ModalConfirm => self.confirm_active_modal(),
            HitTarget::ModalCancel => {
                self.modal = Modal::None;
                UiEffect::None
            }
        }
    }

    fn focus_or_resume_item(&mut self, idx: usize) -> UiEffect {
        let Some(item) = self.snapshot.items.get(idx) else {
            return UiEffect::None;
        };
        if item.is_hidden
            && let Some(ref pty_id) = item.pty_id
        {
            return UiEffect::AttachHiddenPty {
                pty_id: pty_id.clone(),
                cwd: item.cwd.clone(),
                title: item.title.clone(),
            };
        }
        if item.window_index.is_some()
            && let Ok(uuid) = Uuid::parse_str(&item.terminal_key)
        {
            return UiEffect::Focus(uuid);
        }
        if let Some(resume_cmd) = item.resume_command.clone() {
            let cwd = item.cwd.clone();
            let target = self.current_tab_spawn_target();
            let state_dir = self.state_dir.as_deref();
            return UiEffect::Spawn {
                target,
                disposition: SpawnDisposition::SplitRight,
                cwd,
                command: Some(shell_launch_command(&resume_cmd, state_dir)),
            };
        }
        self.set_toast("Session is offline and has no saved resume command.", true);
        UiEffect::None
    }

    fn quick_spawn(&self, disposition: SpawnDisposition) -> UiEffect {
        let cwd = self
            .selected_item()
            .map(|item| item.cwd.clone())
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")));
        let target = self.current_tab_spawn_target();
        UiEffect::Spawn {
            target,
            disposition,
            cwd,
            command: None,
        }
    }
}

fn shell_launch_command(agent_bin: &str, explicit_state_dir: Option<&Path>) -> String {
    if let Ok(agent) = Agent::parse(agent_bin) {
        let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("ghostback"));
        let state_dir = explicit_state_dir
            .map(Path::to_path_buf)
            .or_else(|| {
                std::env::var("GHOSTBACK_STATE_DIR")
                    .ok()
                    .filter(|s| !s.is_empty())
                    .map(PathBuf::from)
            })
            .or_else(|| crate::sessions::default_state_dir().ok())
            .unwrap_or_else(|| PathBuf::from("/tmp/ghostback-state"));
        return herd::build_parent_launch_command(
            &exe,
            &state_dir,
            agent,
            &herd::AgentLaunchOptions::default(),
        );
    }
    let shell = std::env::var("SHELL")
        .ok()
        .filter(|s| s.starts_with('/') && !s.chars().any(char::is_control))
        .unwrap_or_else(|| "/bin/zsh".into());
    format!(
        "{} -lic {}",
        shell_quote(&shell),
        shell_quote(&format!("{agent_bin}; exec {}", shell_quote(&shell)))
    )
}
