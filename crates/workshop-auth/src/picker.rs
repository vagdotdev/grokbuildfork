//! The connection picker Workshop opens for Login / `/login` / welcome `l` / cold start.
//!
//! UX follows the Blackpen picker export (`internal/subscription-picker-from-xml.md`):
//! a popover with **Models** and **Subscriptions** tabs; Subscriptions is a left
//! rail of Claude, Codex, Cursor (in that order) with one pill each
//! (Detecting / Ready / Sign in) and the rail's models on the right. Models
//! holds Local and bring-your-own-key rows grouped by provider, with the
//! optional xAI card last and never preselected.
//!
//! Only the UX is copied. Detection is presence-only ([`crate::detect`]);
//! no credential of another app is read, and the only way to reach the
//! inherited xAI OIDC flow is the labeled xAI card after an explicit confirm.

use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Widget, Wrap};

use crate::detect::{KeyEnv, Presence, ScanResult, VendorCli};
use crate::methods::{XAI_OPTIONAL_COPY, XAI_OPTIONAL_LABEL};
use crate::xai_opt_in;

/// Picker tabs, in display order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Models,
    Subscriptions,
}

impl Tab {
    pub fn label(self) -> &'static str {
        match self {
            Tab::Models => "Models",
            Tab::Subscriptions => "Subscriptions",
        }
    }

    fn other(self) -> Self {
        match self {
            Tab::Models => Tab::Subscriptions,
            Tab::Subscriptions => Tab::Models,
        }
    }
}

/// Rail status pill.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pill {
    Detecting,
    Ready,
    SignIn,
}

impl Pill {
    pub fn label(self) -> &'static str {
        match self {
            Pill::Detecting => "Detecting",
            Pill::Ready => "Ready",
            Pill::SignIn => "Sign in",
        }
    }
}

/// Connection class shown on every row (plan §3). Never presents a
/// subscription as an API base URL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionClass {
    Local,
    DirectApi,
    AgentAdapter,
}

impl ConnectionClass {
    pub fn label(self) -> &'static str {
        match self {
            ConnectionClass::Local => "Local",
            ConnectionClass::DirectApi => "Direct API",
            ConnectionClass::AgentAdapter => "Agent adapter",
        }
    }
}

/// One row on the Models tab.
#[derive(Debug, Clone)]
pub struct ModelRow {
    pub id: &'static str,
    pub title: &'static str,
    pub group: &'static str,
    pub class: ConnectionClass,
    pub base_url: &'static str,
    pub api_backend: &'static str,
    pub env_var: Option<KeyEnv>,
    pub key_present: bool,
    pub example_model: &'static str,
    pub billing: &'static str,
    pub xai_optional: bool,
}

impl ModelRow {
    /// `[model.<id>]` TOML the user can paste into `~/.workshop/config.toml`.
    /// Uses upstream's existing BYOK fields, so it works today without a
    /// provider manifest. Local servers ignore the bearer, but the sampler
    /// requires a non-empty credential, hence the literal `api_key`.
    pub fn config_snippet(&self) -> String {
        let mut out = format!(
            "[model.{id}]\nmodel = \"{model}\"\nbase_url = \"{url}\"\napi_backend = \"{backend}\"\n",
            id = self.id,
            model = self.example_model,
            url = self.base_url,
            backend = self.api_backend,
        );
        match self.env_var {
            Some(env) => out.push_str(&format!("env_key = \"{}\"\n", env.var())),
            None => out.push_str("api_key = \"local\"\n"),
        }
        out.push_str("context_window = 128000\n");
        out
    }
}

/// One Subscriptions rail.
#[derive(Debug, Clone)]
pub struct Rail {
    pub cli: VendorCli,
    /// `None` while the presence scan has not run (pill: Detecting).
    pub presence: Option<Presence>,
    /// Requires the vendor CLI's own status command (adapter milestone); always
    /// `false` until then, so a found CLI shows "Sign in", never a fake Ready.
    pub authenticated: bool,
    pub models: Vec<String>,
}

impl Rail {
    pub fn pill(&self) -> Pill {
        match (&self.presence, self.authenticated) {
            (None, _) => Pill::Detecting,
            (Some(_), true) => Pill::Ready,
            (Some(_), false) => Pill::SignIn,
        }
    }

    pub fn installed(&self) -> bool {
        matches!(self.presence, Some(Presence::Found(_)))
    }

    /// Empty-state copy, verbatim from the Blackpen `harnessEmptyMessage`.
    pub fn empty_copy(&self) -> String {
        if !self.models.is_empty() {
            return String::new();
        }
        match (self.cli, self.installed(), self.authenticated) {
            (VendorCli::CursorAgent, false, _) => "Cursor runs in the desktop app".to_string(),
            (VendorCli::Claude, false, _) => "Install Claude Code, then sign in".to_string(),
            (VendorCli::Codex, false, _) => "Install Codex, then sign in".to_string(),
            (VendorCli::OpenCode, false, _) => "Install OpenCode, then sign in".to_string(),
            (cli, true, false) => format!("Sign in to see {} models", cli.product()),
            (_, true, true) => "No models for this harness".to_string(),
        }
    }

    /// Blackpen `harnessNeedsConnect`: no button once models exist; Connect when
    /// not ready; Claude/Codex still show Connect when ready-but-empty, Cursor does not.
    pub fn needs_connect(&self) -> bool {
        if !self.models.is_empty() {
            return false;
        }
        if self.pill() != Pill::Ready {
            return true;
        }
        !matches!(self.cli, VendorCli::CursorAgent)
    }
}

/// What the right-hand / detail pane is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Detail {
    Model(usize),
    Rail(usize),
    XaiConfirm,
    XaiEnabledRestart,
}

/// What the caller must do after a key press.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickerOutcome {
    Unchanged,
    Changed,
    /// Close the picker; nothing was chosen.
    Close,
    /// The user confirmed the optional xAI card and the agent already runs
    /// with the xAI issuer: start the labeled interactive flow now.
    StartXaiOptionalLogin,
}

/// Colours the host maps from its theme.
#[derive(Debug, Clone, Copy)]
pub struct PickerStyle {
    pub fg: Color,
    pub dim: Color,
    pub accent: Color,
    pub highlight_bg: Color,
    pub border: Color,
    pub success: Color,
    pub warning: Color,
}

impl Default for PickerStyle {
    fn default() -> Self {
        Self {
            fg: Color::Reset,
            dim: Color::DarkGray,
            accent: Color::Cyan,
            highlight_bg: Color::DarkGray,
            border: Color::Gray,
            success: Color::Green,
            warning: Color::Yellow,
        }
    }
}

/// Picker state. Construct with [`ConnectionPicker::new`], feed keys with
/// [`ConnectionPicker::handle_key`], draw with [`ConnectionPicker::render`].
#[derive(Debug, Clone)]
pub struct ConnectionPicker {
    tab: Tab,
    models: Vec<ModelRow>,
    model_sel: usize,
    rails: Vec<Rail>,
    rail_sel: usize,
    detail: Option<Detail>,
    xai_enabled_at_start: bool,
    xai_enabled_now: bool,
    workshop_home: PathBuf,
    error: Option<String>,
}

const LOCAL_GROUP: &str = "Local";
const BYOK_GROUP: &str = "Bring your own key";
const OPTIONAL_GROUP: &str = "Optional";

fn model_rows(scan: &ScanResult) -> Vec<ModelRow> {
    let local = |id, title, base_url, example_model| ModelRow {
        id,
        title,
        group: LOCAL_GROUP,
        class: ConnectionClass::Local,
        base_url,
        api_backend: "chat_completions",
        env_var: None,
        key_present: false,
        example_model,
        billing: "None (runs on this machine)",
        xai_optional: false,
    };
    let byok = |id, title, base_url, api_backend, env: KeyEnv, example_model, billing| ModelRow {
        id,
        title,
        group: BYOK_GROUP,
        class: ConnectionClass::DirectApi,
        base_url,
        api_backend,
        env_var: Some(env),
        key_present: scan.key_present(env),
        example_model,
        billing,
        xai_optional: false,
    };
    vec![
        local("ollama", "Ollama", "http://127.0.0.1:11434/v1", "llama3.1"),
        local("lmstudio", "LM Studio", "http://127.0.0.1:1234/v1", "local-model"),
        local(
            "llamacpp",
            "llama.cpp / vLLM",
            "http://127.0.0.1:8080/v1",
            "local-model",
        ),
        byok(
            "openai",
            "OpenAI API",
            "https://api.openai.com/v1",
            "responses",
            KeyEnv::OpenAi,
            "gpt-4.1",
            "Your OpenAI Platform account",
        ),
        byok(
            "anthropic",
            "Anthropic API",
            "https://api.anthropic.com/v1",
            "messages",
            KeyEnv::Anthropic,
            "claude-sonnet-4-5",
            "Your Anthropic Console account",
        ),
        byok(
            "openrouter",
            "OpenRouter",
            "https://openrouter.ai/api/v1",
            "chat_completions",
            KeyEnv::OpenRouter,
            "openai/gpt-4.1",
            "Your OpenRouter account",
        ),
        ModelRow {
            id: "custom",
            title: "Custom OpenAI-compatible",
            group: BYOK_GROUP,
            class: ConnectionClass::DirectApi,
            base_url: "https://YOUR-HOST/v1",
            api_backend: "chat_completions",
            env_var: None,
            key_present: false,
            example_model: "your-model",
            billing: "Whatever the endpoint bills",
            xai_optional: false,
        },
        ModelRow {
            id: "xai",
            title: XAI_OPTIONAL_LABEL,
            group: OPTIONAL_GROUP,
            class: ConnectionClass::DirectApi,
            base_url: "https://api.x.ai/v1",
            api_backend: "responses",
            env_var: Some(KeyEnv::Xai),
            key_present: scan.key_present(KeyEnv::Xai),
            example_model: "grok-4",
            billing: "Your xAI account",
            xai_optional: true,
        },
    ]
}

fn rails(scan: &ScanResult) -> Vec<Rail> {
    [VendorCli::Claude, VendorCli::Codex, VendorCli::CursorAgent]
        .iter()
        .map(|cli| Rail {
            cli: *cli,
            presence: Some(scan.cli(*cli)),
            authenticated: false,
            models: Vec::new(),
        })
        .collect()
}

impl ConnectionPicker {
    /// `scan` is a completed presence scan; `workshop_home` is where the xAI
    /// opt-in marker lives.
    pub fn new(scan: &ScanResult, workshop_home: PathBuf) -> Self {
        let xai_enabled_at_start = xai_opt_in::enabled_at_process_start(&workshop_home);
        let xai_enabled_now = xai_opt_in::is_enabled(&workshop_home);
        Self {
            tab: Tab::Models,
            models: model_rows(scan),
            model_sel: 0,
            rails: rails(scan),
            rail_sel: 0,
            detail: None,
            xai_enabled_at_start,
            xai_enabled_now,
            workshop_home,
            error: None,
        }
    }

    pub fn tab(&self) -> Tab {
        self.tab
    }

    pub fn models(&self) -> &[ModelRow] {
        &self.models
    }

    pub fn rails(&self) -> &[Rail] {
        &self.rails
    }

    /// Index of the highlighted Models row.
    pub fn selected_model(&self) -> usize {
        self.model_sel
    }

    /// Index of the highlighted rail.
    pub fn selected_rail(&self) -> usize {
        self.rail_sel
    }

    /// `true` while a detail pane (row, rail, or xAI confirm) is open.
    pub fn detail_open(&self) -> bool {
        self.detail.is_some()
    }

    /// `true` when the xAI card is highlighted (it is never preselected: the
    /// first row is always a Local row).
    pub fn xai_card_selected(&self) -> bool {
        self.tab == Tab::Models
            && self
                .models
                .get(self.model_sel)
                .is_some_and(|row| row.xai_optional)
    }

    pub fn handle_key(&mut self, key: &KeyEvent) -> PickerOutcome {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return PickerOutcome::Close;
        }
        if let Some(detail) = self.detail {
            return self.handle_detail_key(detail, key);
        }
        match key.code {
            KeyCode::Esc => PickerOutcome::Close,
            KeyCode::Tab | KeyCode::BackTab | KeyCode::Left | KeyCode::Right => {
                self.tab = self.tab.other();
                PickerOutcome::Changed
            }
            KeyCode::Char('1') => {
                self.tab = Tab::Models;
                PickerOutcome::Changed
            }
            KeyCode::Char('2') => {
                self.tab = Tab::Subscriptions;
                PickerOutcome::Changed
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_selection(1);
                PickerOutcome::Changed
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_selection(-1);
                PickerOutcome::Changed
            }
            KeyCode::Enter => {
                self.detail = Some(match self.tab {
                    Tab::Models => {
                        if self.xai_card_selected() {
                            Detail::XaiConfirm
                        } else {
                            Detail::Model(self.model_sel)
                        }
                    }
                    Tab::Subscriptions => Detail::Rail(self.rail_sel),
                });
                PickerOutcome::Changed
            }
            _ => PickerOutcome::Unchanged,
        }
    }

    fn handle_detail_key(&mut self, detail: Detail, key: &KeyEvent) -> PickerOutcome {
        match detail {
            Detail::XaiConfirm => match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    if self.xai_enabled_at_start {
                        return PickerOutcome::StartXaiOptionalLogin;
                    }
                    match xai_opt_in::enable(&self.workshop_home) {
                        Ok(()) => {
                            self.xai_enabled_now = true;
                            self.error = None;
                            self.detail = Some(Detail::XaiEnabledRestart);
                        }
                        Err(err) => {
                            self.error = Some(format!("Could not save the xAI opt-in: {err}"));
                            self.detail = None;
                        }
                    }
                    PickerOutcome::Changed
                }
                KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Backspace => {
                    self.detail = None;
                    PickerOutcome::Changed
                }
                _ => PickerOutcome::Unchanged,
            },
            Detail::XaiEnabledRestart | Detail::Model(_) | Detail::Rail(_) => match key.code {
                KeyCode::Esc | KeyCode::Enter | KeyCode::Backspace | KeyCode::Char('q') => {
                    self.detail = None;
                    PickerOutcome::Changed
                }
                _ => PickerOutcome::Unchanged,
            },
        }
    }

    fn move_selection(&mut self, delta: isize) {
        let (sel, len) = match self.tab {
            Tab::Models => (&mut self.model_sel, self.models.len()),
            Tab::Subscriptions => (&mut self.rail_sel, self.rails.len()),
        };
        if len == 0 {
            return;
        }
        let next = (*sel as isize + delta).rem_euclid(len as isize);
        *sel = next as usize;
    }

    // ----------------------------------------------------------------- render

    /// Draw the picker centred in `area`.
    pub fn render(&self, area: Rect, buf: &mut Buffer, style: &PickerStyle) {
        let width = area.width.saturating_sub(4).min(80).max(44);
        let height = area.height.saturating_sub(2).min(24).max(12);
        if area.width < 24 || area.height < 8 {
            return;
        }
        let rect = Rect {
            x: area.x + (area.width.saturating_sub(width)) / 2,
            y: area.y + (area.height.saturating_sub(height)) / 2,
            width: width.min(area.width),
            height: height.min(area.height),
        };
        Clear.render(rect, buf);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(style.border))
            .title(Line::from(vec![
                Span::styled(" Connect ", Style::default().fg(style.fg).add_modifier(Modifier::BOLD)),
                Span::styled(
                    format!("· {} ", workshop_branding::PRODUCT_NAME),
                    Style::default().fg(style.dim),
                ),
            ]));
        let inner = block.inner(rect);
        block.render(rect, buf);
        if inner.height < 4 {
            return;
        }

        self.render_tabs(Rect { height: 1, ..inner }, buf, style);
        let sep_y = inner.y + 1;
        for x in inner.x..inner.x + inner.width {
            if let Some(cell) = buf.cell_mut((x, sep_y)) {
                cell.set_char('─');
                cell.set_style(Style::default().fg(style.border));
            }
        }
        let hints_y = inner.y + inner.height - 1;
        let footer_y = hints_y.saturating_sub(1);
        let body = Rect {
            x: inner.x,
            y: inner.y + 2,
            width: inner.width,
            height: footer_y.saturating_sub(inner.y + 2),
        };
        match self.detail {
            Some(detail) => self.render_detail(detail, body, buf, style),
            None => match self.tab {
                Tab::Models => self.render_models(body, buf, style),
                Tab::Subscriptions => self.render_rails(body, buf, style),
            },
        }
        let footer = if self.detail.is_some() {
            String::new()
        } else {
            match self.tab {
                Tab::Models => "Connect provider: Enter on a row · Manage models: ~/.workshop/config.toml".to_string(),
                Tab::Subscriptions => "Manage models: ~/.workshop/config.toml".to_string(),
            }
        };
        buf.set_string(inner.x + 1, footer_y, truncate(&footer, inner.width.saturating_sub(2)), Style::default().fg(style.dim));
        let hints = if self.detail.is_some() {
            "Esc back"
        } else {
            "↑↓ select · Tab switch tab · Enter details · Esc close (nothing is contacted)"
        };
        buf.set_string(inner.x + 1, hints_y, truncate(hints, inner.width.saturating_sub(2)), Style::default().fg(style.dim));
    }

    fn render_tabs(&self, area: Rect, buf: &mut Buffer, style: &PickerStyle) {
        let mut x = area.x + 1;
        for tab in [Tab::Models, Tab::Subscriptions] {
            let active = tab == self.tab;
            let label = format!(" {} ", tab.label());
            let st = if active {
                Style::default()
                    .fg(style.fg)
                    .bg(style.highlight_bg)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(style.dim)
            };
            buf.set_string(x, area.y, &label, st);
            x += width_of(&label) + 2;
        }
        let right = if self.xai_enabled_now {
            "xAI opt-in: on"
        } else {
            "xAI opt-in: off"
        };
        let rx = (area.x + area.width).saturating_sub(width_of(right) + 1);
        if rx > x {
            buf.set_string(rx, area.y, right, Style::default().fg(style.dim));
        }
    }

    fn render_models(&self, area: Rect, buf: &mut Buffer, style: &PickerStyle) {
        let mut y = area.y;
        let mut last_group = "";
        let class_col = (area.x + area.width).saturating_sub(15);
        for (idx, row) in self.models.iter().enumerate() {
            if y >= area.y + area.height {
                break;
            }
            if row.group != last_group {
                if y + 1 >= area.y + area.height {
                    break;
                }
                buf.set_string(
                    area.x + 1,
                    y,
                    row.group,
                    Style::default().fg(style.dim).add_modifier(Modifier::BOLD),
                );
                last_group = row.group;
                y += 1;
            }
            let selected = idx == self.model_sel;
            let row_style = if selected {
                Style::default().fg(style.fg).bg(style.highlight_bg)
            } else {
                Style::default().fg(style.fg)
            };
            if selected {
                for x in area.x..area.x + area.width {
                    if let Some(cell) = buf.cell_mut((x, y)) {
                        cell.set_style(row_style);
                    }
                }
            }
            let bullet = if selected { "●" } else { "○" };
            buf.set_string(area.x + 2, y, bullet, row_style.fg(style.accent));
            let title_w = 24usize;
            buf.set_string(area.x + 4, y, truncate(row.title, title_w as u16), row_style.add_modifier(Modifier::BOLD));
            let detail = if row.xai_optional {
                XAI_OPTIONAL_COPY.to_string()
            } else if let Some(env) = row.env_var {
                format!(
                    "{} · {}",
                    env.var(),
                    if row.key_present { "set" } else { "not set" }
                )
            } else {
                row.base_url.to_string()
            };
            let detail_x = area.x + 4 + title_w as u16 + 2;
            let detail_end = if row.xai_optional {
                area.x + area.width
            } else {
                class_col
            };
            let detail_w = detail_end.saturating_sub(detail_x + 1);
            let detail_style = if row.xai_optional {
                row_style.fg(style.warning)
            } else if row.key_present {
                row_style.fg(style.success)
            } else {
                row_style.fg(style.dim)
            };
            buf.set_string(detail_x, y, truncate(&detail, detail_w), detail_style);
            if !row.xai_optional {
                buf.set_string(class_col, y, row.class.label(), row_style.fg(style.dim));
            }
            y += 1;
        }
        if let Some(err) = &self.error
            && y < area.y + area.height
        {
            buf.set_string(area.x + 1, y, truncate(err, area.width.saturating_sub(2)), Style::default().fg(style.warning));
        }
    }

    fn render_rails(&self, area: Rect, buf: &mut Buffer, style: &PickerStyle) {
        let rail_w: u16 = 22;
        for (idx, rail) in self.rails.iter().enumerate() {
            let y = area.y + idx as u16;
            if y >= area.y + area.height {
                break;
            }
            let selected = idx == self.rail_sel;
            let row_style = if selected {
                Style::default().fg(style.fg).bg(style.highlight_bg)
            } else {
                Style::default().fg(style.fg)
            };
            if selected {
                for x in area.x..area.x + rail_w {
                    if let Some(cell) = buf.cell_mut((x, y)) {
                        cell.set_style(row_style);
                    }
                }
            }
            buf.set_string(area.x + 1, y, rail.cli.product(), row_style.add_modifier(Modifier::BOLD));
            let pill = rail.pill();
            let pill_style = match pill {
                Pill::Ready => row_style.fg(style.success),
                Pill::Detecting => row_style.fg(style.dim),
                Pill::SignIn => row_style.fg(style.warning),
            };
            let pill_text = format!("[{}]", pill.label());
            buf.set_string(
                (area.x + rail_w).saturating_sub(width_of(&pill_text) + 1),
                y,
                &pill_text,
                pill_style,
            );
        }
        // Divider between rail and pane.
        for y in area.y..area.y + area.height {
            if let Some(cell) = buf.cell_mut((area.x + rail_w, y)) {
                cell.set_char('│');
                cell.set_style(Style::default().fg(style.border));
            }
        }
        let pane = Rect {
            x: area.x + rail_w + 2,
            y: area.y,
            width: area.width.saturating_sub(rail_w + 3),
            height: area.height,
        };
        let Some(rail) = self.rails.get(self.rail_sel) else {
            return;
        };
        let mut lines: Vec<Line> = Vec::new();
        lines.push(Line::from(vec![
            Span::styled(rail.cli.product(), Style::default().fg(style.fg).add_modifier(Modifier::BOLD)),
            Span::styled(
                format!(" · {}", ConnectionClass::AgentAdapter.label()),
                Style::default().fg(style.dim),
            ),
        ]));
        if rail.models.is_empty() {
            lines.push(Line::from(Span::styled(rail.empty_copy(), Style::default().fg(style.fg))));
        } else {
            for model in &rail.models {
                lines.push(Line::from(format!("○ {model}")));
            }
        }
        if let Some(Presence::Found(path)) = &rail.presence {
            lines.push(Line::from(Span::styled(
                format!("Found: {}", path.display()),
                Style::default().fg(style.dim),
            )));
        } else if rail.cli == VendorCli::CursorAgent {
            lines.push(Line::from(Span::styled(
                "Install the Cursor Agent CLI (cursor-agent) to use this subscription here.",
                Style::default().fg(style.dim),
            )));
        }
        if rail.needs_connect() {
            lines.push(Line::from(""));
            lines.push(Line::from(vec![
                Span::styled("[ Connect ]", Style::default().fg(style.accent).add_modifier(Modifier::BOLD)),
                Span::styled("  Enter for details", Style::default().fg(style.dim)),
            ]));
        }
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .render(pane, buf);
    }

    fn render_detail(&self, detail: Detail, area: Rect, buf: &mut Buffer, style: &PickerStyle) {
        let bold = Style::default().fg(style.fg).add_modifier(Modifier::BOLD);
        let dim = Style::default().fg(style.dim);
        let plain = Style::default().fg(style.fg);
        let mut lines: Vec<Line> = Vec::new();
        match detail {
            Detail::Model(idx) => {
                let Some(row) = self.models.get(idx) else {
                    return;
                };
                lines.push(Line::from(vec![
                    Span::styled(row.title, bold),
                    Span::styled(format!(" · {}", row.class.label()), dim),
                ]));
                lines.push(Line::from(Span::styled(
                    format!(
                        "Workshop sends prompts to {} through its own agent loop. Billing: {}.",
                        row.base_url, row.billing
                    ),
                    plain,
                )));
                if let Some(env) = row.env_var {
                    let state = if row.key_present { "set" } else { "not set" };
                    lines.push(Line::from(Span::styled(
                        format!("{}: {state}. Workshop reads it when a session starts and never displays it.", env.var()),
                        if row.key_present { Style::default().fg(style.success) } else { Style::default().fg(style.warning) },
                    )));
                } else {
                    lines.push(Line::from(Span::styled(
                        "No key needed. Start the server first; local servers ignore the placeholder api_key.",
                        plain,
                    )));
                }
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    format!("Add to {} and restart workshop:", workshop_branding::CONFIG_PATH_HINT),
                    plain,
                )));
                for snippet_line in row.config_snippet().lines() {
                    lines.push(Line::from(Span::styled(format!("  {snippet_line}"), Style::default().fg(style.accent))));
                }
                lines.push(Line::from(Span::styled(
                    "Saving the key to the OS keyring from this screen arrives with the provider milestone.",
                    dim,
                )));
            }
            Detail::Rail(idx) => {
                let Some(rail) = self.rails.get(idx) else {
                    return;
                };
                lines.push(Line::from(vec![
                    Span::styled(rail.cli.product(), bold),
                    Span::styled(format!(" · {}", ConnectionClass::AgentAdapter.label()), dim),
                ]));
                lines.push(Line::from(Span::styled(
                    format!(
                        "Workshop runs the official `{}` CLI for you. It never reads that app's credentials, keychain, or auth files.",
                        rail.cli.binary()
                    ),
                    plain,
                )));
                match &rail.presence {
                    Some(Presence::Found(path)) => {
                        lines.push(Line::from(Span::styled(format!("Found: {}", path.display()), Style::default().fg(style.success))));
                        lines.push(Line::from(Span::styled(
                            format!("Sign in: run `{}` in a terminal, then reopen Login.", rail.cli.login_command()),
                            plain,
                        )));
                    }
                    Some(Presence::Missing) => {
                        lines.push(Line::from(Span::styled(rail.empty_copy(), Style::default().fg(style.warning))));
                        if rail.cli == VendorCli::CursorAgent {
                            lines.push(Line::from(Span::styled(
                                "Install the Cursor Agent CLI (cursor-agent); the desktop app alone is not enough.",
                                plain,
                            )));
                        }
                    }
                    None => lines.push(Line::from(Span::styled("Detecting…", dim))),
                }
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    "Delegating tasks to this CLI (the adapter run) arrives in a later Workshop milestone.",
                    dim,
                )));
            }
            Detail::XaiConfirm => {
                lines.push(Line::from(vec![
                    Span::styled(XAI_OPTIONAL_LABEL, bold),
                    Span::styled(" · not required", dim),
                ]));
                lines.push(Line::from(Span::styled(XAI_OPTIONAL_COPY, Style::default().fg(style.warning))));
                lines.push(Line::from(""));
                if self.xai_enabled_at_start {
                    lines.push(Line::from(Span::styled(
                        "Continue opens https://auth.x.ai in your browser and stores an xAI session in ~/.workshop/auth.json.",
                        plain,
                    )));
                } else {
                    lines.push(Line::from(Span::styled(
                        "Continue records your opt-in in ~/.workshop/workshop-connections.toml. Nothing is contacted now; after a restart this card opens https://auth.x.ai.",
                        plain,
                    )));
                }
                lines.push(Line::from(""));
                lines.push(Line::from(vec![
                    Span::styled("y", Style::default().fg(style.accent).add_modifier(Modifier::BOLD)),
                    Span::styled(" continue   ", plain),
                    Span::styled("n", Style::default().fg(style.accent).add_modifier(Modifier::BOLD)),
                    Span::styled(" / Esc back", plain),
                ]));
            }
            Detail::XaiEnabledRestart => {
                lines.push(Line::from(Span::styled("xAI sign-in enabled", bold)));
                lines.push(Line::from(Span::styled(
                    "Restart workshop, open Login, and choose the xAI card again to sign in. Delete ~/.workshop/workshop-connections.toml to revoke.",
                    plain,
                )));
            }
        }
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .render(
                Rect {
                    x: area.x + 1,
                    y: area.y,
                    width: area.width.saturating_sub(2),
                    height: area.height,
                },
                buf,
            );
    }
}

fn width_of(s: &str) -> u16 {
    unicode_width::UnicodeWidthStr::width(s) as u16
}

fn truncate(s: &str, max: u16) -> String {
    let max = max as usize;
    let mut out = String::new();
    let mut w = 0usize;
    for ch in s.chars() {
        let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if w + cw > max {
            break;
        }
        out.push(ch);
        w += cw;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn picker() -> ConnectionPicker {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();
        std::mem::forget(dir);
        ConnectionPicker::new(&ScanResult::default(), home)
    }

    #[test]
    fn opens_on_models_with_a_local_row_selected_and_xai_last() {
        let p = picker();
        assert_eq!(p.tab(), Tab::Models);
        assert!(!p.xai_card_selected(), "xAI must never be preselected");
        let first = p.models().first().unwrap();
        assert_eq!(first.class, ConnectionClass::Local);
        let last = p.models().last().unwrap();
        assert!(last.xai_optional);
        assert_eq!(last.title, XAI_OPTIONAL_LABEL);
        assert_eq!(
            p.models().iter().filter(|r| r.xai_optional).count(),
            1,
            "exactly one optional xAI card"
        );
    }

    #[test]
    fn rails_are_claude_codex_cursor_in_order_with_sign_in_pills() {
        let p = picker();
        let names: Vec<&str> = p.rails().iter().map(|r| r.cli.product()).collect();
        assert_eq!(names, ["Claude", "Codex", "Cursor"]);
        for rail in p.rails() {
            assert_eq!(rail.pill(), Pill::SignIn, "{}", rail.cli.product());
            assert!(rail.needs_connect());
        }
    }

    #[test]
    fn empty_copy_matches_the_blackpen_export() {
        let mut p = picker();
        let copy: Vec<String> = p.rails().iter().map(Rail::empty_copy).collect();
        assert_eq!(
            copy,
            [
                "Install Claude Code, then sign in",
                "Install Codex, then sign in",
                "Cursor runs in the desktop app",
            ]
        );
        for rail in &mut p.rails {
            rail.presence = Some(Presence::Found(PathBuf::from("/usr/local/bin/x")));
        }
        let copy: Vec<String> = p.rails().iter().map(Rail::empty_copy).collect();
        assert_eq!(
            copy,
            [
                "Sign in to see Claude models",
                "Sign in to see Codex models",
                "Sign in to see Cursor models",
            ]
        );
        assert_eq!(p.rails.first().unwrap().pill(), Pill::SignIn);
    }

    #[test]
    fn detecting_pill_when_presence_unknown() {
        let mut p = picker();
        p.rails.first_mut().unwrap().presence = None;
        assert_eq!(p.rails.first().unwrap().pill(), Pill::Detecting);
    }

    #[test]
    fn tab_and_arrows_switch_tabs_and_esc_closes() {
        let mut p = picker();
        assert_eq!(p.handle_key(&key(KeyCode::Tab)), PickerOutcome::Changed);
        assert_eq!(p.tab(), Tab::Subscriptions);
        assert_eq!(p.handle_key(&key(KeyCode::Left)), PickerOutcome::Changed);
        assert_eq!(p.tab(), Tab::Models);
        assert_eq!(p.handle_key(&key(KeyCode::Esc)), PickerOutcome::Close);
    }

    #[test]
    fn xai_card_needs_explicit_confirm_and_never_starts_login_without_prior_opt_in() {
        let mut p = picker();
        let xai_idx = p.models().iter().position(|r| r.xai_optional).unwrap();
        for _ in 0..xai_idx {
            p.handle_key(&key(KeyCode::Down));
        }
        assert!(p.xai_card_selected());
        assert_eq!(p.handle_key(&key(KeyCode::Enter)), PickerOutcome::Changed);
        assert_eq!(p.detail, Some(Detail::XaiConfirm));
        // Declining goes back without touching anything.
        assert_eq!(p.handle_key(&key(KeyCode::Char('n'))), PickerOutcome::Changed);
        assert!(!p.xai_enabled_now);
        assert!(!xai_opt_in::is_enabled(&p.workshop_home));
        // Confirming records the opt-in but cannot start the flow in this process
        // (the agent was built without the xAI issuer).
        p.handle_key(&key(KeyCode::Enter));
        assert_eq!(p.handle_key(&key(KeyCode::Char('y'))), PickerOutcome::Changed);
        assert!(p.xai_enabled_now);
        assert!(xai_opt_in::is_enabled(&p.workshop_home));
        assert_eq!(p.detail, Some(Detail::XaiEnabledRestart));
    }

    #[test]
    fn local_and_byok_rows_open_a_config_detail() {
        let mut p = picker();
        assert_eq!(p.handle_key(&key(KeyCode::Enter)), PickerOutcome::Changed);
        assert_eq!(p.detail, Some(Detail::Model(0)));
        let snippet = p.models().first().unwrap().config_snippet();
        assert!(snippet.contains("[model.ollama]"));
        assert!(snippet.contains("base_url = \"http://127.0.0.1:11434/v1\""));
        assert_eq!(p.handle_key(&key(KeyCode::Esc)), PickerOutcome::Changed);
        assert!(!p.detail_open());
    }

    #[test]
    fn renders_tabs_rails_and_xai_copy_into_a_buffer() {
        let mut p = picker();
        let area = Rect::new(0, 0, 100, 30);
        let mut buf = Buffer::empty(area);
        p.render(area, &mut buf, &PickerStyle::default());
        let text = buffer_text(&buf);
        assert!(text.contains("Models"));
        assert!(text.contains("Subscriptions"));
        assert!(text.contains(XAI_OPTIONAL_LABEL));
        assert!(text.contains("Not required"));
        p.handle_key(&key(KeyCode::Tab));
        let mut buf = Buffer::empty(area);
        p.render(area, &mut buf, &PickerStyle::default());
        let text = buffer_text(&buf);
        for needle in ["Claude", "Codex", "Cursor", "[Sign in]", "Install Claude Code, then sign in"] {
            assert!(text.contains(needle), "missing {needle:?} in\n{text}");
        }
    }

    fn buffer_text(buf: &Buffer) -> String {
        let mut out = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                if let Some(cell) = buf.cell((x, y)) {
                    out.push_str(cell.symbol());
                }
            }
            out.push('\n');
        }
        out
    }
}
