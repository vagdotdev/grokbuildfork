//! Provider manager modal state, input, and rendering.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use zeroize::Zeroizing;

use crate::provider_cmd::{AddProviderRequest, ProviderInfo, ProviderMutation, ProviderSecret};
use crate::theme::Theme;
use crate::views::modal_window::{
    self, ModalSizing, ModalWindowConfig, ModalWindowState, Shortcut,
};

const KNOWN_PROVIDERS: [(&str, &str); 4] = [
    ("openrouter", "OpenRouter"),
    ("openai", "OpenAI"),
    ("anthropic", "Anthropic"),
    ("xai", "xAI"),
];

pub struct ProviderModalState {
    pub window: ModalWindowState,
    pub providers: Option<Vec<ProviderInfo>>,
    pub selected: usize,
    pub mode: ProviderModalMode,
    pub message: Option<String>,
    pub error: Option<String>,
    pending_add: Option<AddProviderRequest>,
}

pub enum ProviderModalMode {
    Browse,
    ChooseProvider { selected: usize },
    AddForm(AddForm),
    ImportConfirm { include_oauth: bool },
    RemoveConfirm { id: String, label: String },
    Busy { label: String },
}

pub struct AddForm {
    provider_id: String,
    provider_label: String,
    model: TextInput,
    secret: SecretInput,
    focused: usize,
}

#[derive(Default)]
struct TextInput {
    text: String,
    cursor: usize,
}

#[derive(Default)]
struct SecretInput {
    text: Zeroizing<String>,
    cursor: usize,
}

pub enum ProviderModalOutcome {
    Changed,
    Unchanged,
    Close,
    Refresh,
    SubmitAdd,
    ImportOpenCode { include_oauth: bool },
    Remove { id: String },
}

impl ProviderModalState {
    pub fn loading() -> Self {
        Self {
            window: ModalWindowState::new(),
            providers: None,
            selected: 0,
            mode: ProviderModalMode::Browse,
            message: None,
            error: None,
            pending_add: None,
        }
    }

    pub fn apply_loaded(&mut self, result: Result<Vec<ProviderInfo>, String>) {
        self.message = None;
        match result {
            Ok(providers) => {
                self.providers = Some(providers);
                self.error = None;
                self.clamp_selection();
            }
            Err(error) => {
                if self.providers.is_none() {
                    self.providers = Some(Vec::new());
                }
                self.error = Some(error);
                self.selected = 0;
            }
        }
        self.mode = ProviderModalMode::Browse;
    }

    pub fn apply_mutation(&mut self, result: Result<ProviderMutation, String>) {
        match result {
            Ok(result) => {
                self.providers = Some(result.providers);
                self.message = Some(result.message);
                self.error = None;
                self.clamp_selection();
            }
            Err(error) => {
                self.message = None;
                self.error = Some(error);
            }
        }
        self.mode = ProviderModalMode::Browse;
    }

    pub fn take_pending_add(&mut self) -> Option<AddProviderRequest> {
        self.pending_add.take()
    }

    pub fn handle_paste(&mut self, text: &str) -> bool {
        let ProviderModalMode::AddForm(form) = &mut self.mode else {
            return false;
        };
        let cleaned: String = text
            .chars()
            .filter(|ch| !matches!(ch, '\r' | '\n'))
            .collect();
        if cleaned.is_empty() {
            return false;
        }
        if form.focused == 0 {
            form.model.insert(&cleaned);
        } else {
            form.secret.insert(&cleaned);
        }
        self.error = None;
        true
    }

    fn clamp_selection(&mut self) {
        self.selected = self.selected.min(
            self.providers
                .as_ref()
                .map_or(0, Vec::len)
                .saturating_sub(1),
        );
    }

    fn start_add(&mut self, selected: usize) {
        let (provider_id, provider_label) = KNOWN_PROVIDERS[selected];
        self.mode = ProviderModalMode::AddForm(AddForm {
            provider_id: provider_id.to_owned(),
            provider_label: provider_label.to_owned(),
            model: TextInput::default(),
            secret: SecretInput::default(),
            focused: 0,
        });
        self.error = None;
        self.message = None;
    }
}

impl TextInput {
    fn insert(&mut self, value: &str) {
        self.text.insert_str(self.cursor, value);
        self.cursor += value.len();
    }

    fn handle_edit_key(&mut self, key: &KeyEvent) -> bool {
        edit_text(&mut self.text, &mut self.cursor, key)
    }
}

impl SecretInput {
    fn insert(&mut self, value: &str) {
        self.text.insert_str(self.cursor, value);
        self.cursor += value.len();
    }

    fn handle_edit_key(&mut self, key: &KeyEvent) -> bool {
        edit_text(&mut self.text, &mut self.cursor, key)
    }

    fn take(&mut self) -> String {
        self.cursor = 0;
        std::mem::take(&mut *self.text)
    }
}

fn edit_text(text: &mut String, cursor: &mut usize, key: &KeyEvent) -> bool {
    match key.code {
        KeyCode::Char(ch) if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT => {
            text.insert(*cursor, ch);
            *cursor += ch.len_utf8();
            true
        }
        KeyCode::Backspace if *cursor > 0 => {
            let previous = text[..*cursor]
                .char_indices()
                .next_back()
                .map_or(0, |(index, _)| index);
            text.drain(previous..*cursor);
            *cursor = previous;
            true
        }
        KeyCode::Delete if *cursor < text.len() => {
            let next = text[*cursor..]
                .char_indices()
                .nth(1)
                .map_or(text.len(), |(index, _)| *cursor + index);
            text.drain(*cursor..next);
            true
        }
        KeyCode::Left if *cursor > 0 => {
            *cursor = text[..*cursor]
                .char_indices()
                .next_back()
                .map_or(0, |(index, _)| index);
            true
        }
        KeyCode::Right if *cursor < text.len() => {
            *cursor = text[*cursor..]
                .char_indices()
                .nth(1)
                .map_or(text.len(), |(index, _)| *cursor + index);
            true
        }
        KeyCode::Home => {
            *cursor = 0;
            true
        }
        KeyCode::End => {
            *cursor = text.len();
            true
        }
        _ => false,
    }
}

pub fn handle_provider_key(state: &mut ProviderModalState, key: &KeyEvent) -> ProviderModalOutcome {
    state.error = state.error.take().filter(|_| key.code == KeyCode::Esc);
    match &mut state.mode {
        ProviderModalMode::Browse => match key.code {
            KeyCode::Esc => ProviderModalOutcome::Close,
            KeyCode::Up | KeyCode::Char('k') => {
                state.selected = state.selected.saturating_sub(1);
                ProviderModalOutcome::Changed
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let max = state
                    .providers
                    .as_ref()
                    .map_or(0, Vec::len)
                    .saturating_sub(1);
                state.selected = (state.selected + 1).min(max);
                ProviderModalOutcome::Changed
            }
            KeyCode::Char('a') => {
                state.mode = ProviderModalMode::ChooseProvider { selected: 0 };
                state.message = None;
                ProviderModalOutcome::Changed
            }
            KeyCode::Char('i') => {
                state.mode = ProviderModalMode::ImportConfirm {
                    include_oauth: false,
                };
                state.message = None;
                ProviderModalOutcome::Changed
            }
            KeyCode::Char('d') => {
                if let Some(provider) = state
                    .providers
                    .as_ref()
                    .and_then(|providers| providers.get(state.selected))
                {
                    state.mode = ProviderModalMode::RemoveConfirm {
                        id: provider.id.clone(),
                        label: provider.label.clone(),
                    };
                }
                ProviderModalOutcome::Changed
            }
            KeyCode::Char('r') => {
                state.mode = ProviderModalMode::Busy {
                    label: "Refreshing providers...".to_owned(),
                };
                state.message = None;
                state.error = None;
                ProviderModalOutcome::Refresh
            }
            _ => ProviderModalOutcome::Unchanged,
        },
        ProviderModalMode::ChooseProvider { selected } => match key.code {
            KeyCode::Esc => {
                state.mode = ProviderModalMode::Browse;
                ProviderModalOutcome::Changed
            }
            KeyCode::Up | KeyCode::Char('k') => {
                *selected = selected.saturating_sub(1);
                ProviderModalOutcome::Changed
            }
            KeyCode::Down | KeyCode::Char('j') => {
                *selected = (*selected + 1).min(KNOWN_PROVIDERS.len() - 1);
                ProviderModalOutcome::Changed
            }
            KeyCode::Enter => {
                let selected = *selected;
                state.start_add(selected);
                ProviderModalOutcome::Changed
            }
            _ => ProviderModalOutcome::Unchanged,
        },
        ProviderModalMode::AddForm(form) => match key.code {
            KeyCode::Esc => {
                state.mode = ProviderModalMode::ChooseProvider { selected: 0 };
                state.error = None;
                ProviderModalOutcome::Changed
            }
            KeyCode::Tab | KeyCode::BackTab => {
                form.focused = 1 - form.focused;
                ProviderModalOutcome::Changed
            }
            KeyCode::Enter => {
                if form.secret.text.trim().is_empty() {
                    state.error = Some("API key is required".to_owned());
                    form.focused = 1;
                    return ProviderModalOutcome::Changed;
                }
                let secret = match ProviderSecret::new(form.secret.take()) {
                    Ok(secret) => secret,
                    Err(error) => {
                        state.error = Some(error.to_string());
                        return ProviderModalOutcome::Changed;
                    }
                };
                let model =
                    (!form.model.text.trim().is_empty()).then(|| form.model.text.trim().to_owned());
                let provider_id = form.provider_id.clone();
                state.pending_add = Some(AddProviderRequest {
                    provider: provider_id.clone(),
                    model,
                    base_url: None,
                    backend: None,
                    auth_scheme: None,
                    context_window: 200_000,
                    make_default: false,
                    secret,
                });
                state.mode = ProviderModalMode::Busy {
                    label: format!("Connecting {provider_id}..."),
                };
                state.error = None;
                ProviderModalOutcome::SubmitAdd
            }
            _ => {
                let changed = if form.focused == 0 {
                    form.model.handle_edit_key(key)
                } else {
                    form.secret.handle_edit_key(key)
                };
                if changed {
                    state.error = None;
                    ProviderModalOutcome::Changed
                } else {
                    ProviderModalOutcome::Unchanged
                }
            }
        },
        ProviderModalMode::ImportConfirm { include_oauth } => match key.code {
            KeyCode::Esc | KeyCode::Char('n') => {
                state.mode = ProviderModalMode::Browse;
                ProviderModalOutcome::Changed
            }
            KeyCode::Char('o') | KeyCode::Char(' ') => {
                *include_oauth = !*include_oauth;
                ProviderModalOutcome::Changed
            }
            KeyCode::Char('y') | KeyCode::Enter => {
                let include_oauth = *include_oauth;
                state.mode = ProviderModalMode::Busy {
                    label: "Importing OpenCode credentials...".to_owned(),
                };
                ProviderModalOutcome::ImportOpenCode { include_oauth }
            }
            _ => ProviderModalOutcome::Unchanged,
        },
        ProviderModalMode::RemoveConfirm { id, .. } => match key.code {
            KeyCode::Esc | KeyCode::Char('n') => {
                state.mode = ProviderModalMode::Browse;
                ProviderModalOutcome::Changed
            }
            KeyCode::Char('y') | KeyCode::Enter => {
                let id = id.clone();
                state.mode = ProviderModalMode::Busy {
                    label: format!("Removing {id}..."),
                };
                ProviderModalOutcome::Remove { id }
            }
            _ => ProviderModalOutcome::Unchanged,
        },
        ProviderModalMode::Busy { .. } => match key.code {
            KeyCode::Esc => ProviderModalOutcome::Close,
            _ => ProviderModalOutcome::Changed,
        },
    }
}

pub fn render_provider_modal(
    buf: &mut Buffer,
    area: Rect,
    state: &mut ProviderModalState,
    compact: bool,
    theme: &Theme,
) {
    let shortcuts = shortcuts_for(&state.mode);
    let config = ModalWindowConfig {
        title: "Providers",
        tabs: None,
        shortcuts: &shortcuts,
        sizing: ModalSizing {
            width_pct: 0.72,
            max_width: 112,
            min_width: 58,
            v_margin: 4,
            h_pad: 2,
            v_pad: 1,
            footer_lines: 3,
        }
        .with_compact(compact),
        fold_info: None,
    };
    let Some(content) =
        modal_window::render_modal_window(buf, area, &mut state.window, &config, theme)
    else {
        return;
    };
    render_content(buf, content.content, state, theme);
}

fn shortcuts_for(mode: &ProviderModalMode) -> Vec<Shortcut<'static>> {
    let labels: &[&str] = match mode {
        ProviderModalMode::Browse => &[
            "Up/Down nav",
            "a add",
            "i import",
            "d remove",
            "r refresh",
            "Esc close",
        ],
        ProviderModalMode::ChooseProvider { .. } => &["Up/Down nav", "Enter choose", "Esc back"],
        ProviderModalMode::AddForm(_) => &["Tab field", "Enter connect", "Esc back"],
        ProviderModalMode::ImportConfirm { .. } => &["o OAuth", "y/Enter import", "n/Esc cancel"],
        ProviderModalMode::RemoveConfirm { .. } => &["y/Enter remove", "n/Esc cancel"],
        ProviderModalMode::Busy { .. } => &["Esc close"],
    };
    labels
        .iter()
        .map(|label| Shortcut {
            label,
            clickable: false,
            id: 0,
        })
        .collect()
}

fn render_content(buf: &mut Buffer, area: Rect, state: &ProviderModalState, theme: &Theme) {
    if area.height == 0 {
        return;
    }
    let normal = Style::default().fg(theme.text_primary).bg(theme.bg_base);
    let dim = Style::default().fg(theme.text_secondary).bg(theme.bg_base);
    let selected = Style::default()
        .fg(theme.text_primary)
        .bg(theme.bg_highlight)
        .add_modifier(Modifier::BOLD);
    let error = Style::default().fg(theme.accent_error).bg(theme.bg_base);
    let success = Style::default().fg(theme.accent_success).bg(theme.bg_base);
    let mut y = area.y;

    match &state.mode {
        ProviderModalMode::Browse => {
            let storage_notice = if cfg!(target_os = "macos") {
                "Credentials are stored in macOS Keychain. Config contains references only."
            } else {
                "Secure provider storage is currently available on macOS only."
            };
            set_line(buf, area, y, storage_notice, dim);
            y += 2;
            if state.providers.is_none() {
                set_line(buf, area, y, "Loading providers...", normal);
            } else if state.providers.as_ref().is_some_and(Vec::is_empty) {
                set_line(
                    buf,
                    area,
                    y,
                    "No providers connected. Press a to add or i to import OpenCode.",
                    normal,
                );
            } else if let Some(providers) = &state.providers {
                set_line(
                    buf,
                    area,
                    y,
                    "  PROVIDER               STATUS                           SOURCE       MODEL",
                    dim,
                );
                y += 1;
                for (index, provider) in providers.iter().enumerate() {
                    if y >= area.bottom() {
                        break;
                    }
                    let label = if provider.label.eq_ignore_ascii_case(&provider.id) {
                        provider.label.clone()
                    } else {
                        format!("{} ({})", provider.label, provider.id)
                    };
                    let row = format!(
                        "  {:<22} {:<32} {:<12} {}",
                        label,
                        provider.status.as_str(),
                        provider.source,
                        provider.model.as_deref().unwrap_or("-")
                    );
                    set_line(
                        buf,
                        area,
                        y,
                        &row,
                        if index == state.selected {
                            selected
                        } else {
                            normal
                        },
                    );
                    y += 1;
                }
            }
        }
        ProviderModalMode::ChooseProvider { selected: index } => {
            set_line(buf, area, y, "Choose an API-key provider", normal);
            y += 2;
            for (row, (id, label)) in KNOWN_PROVIDERS.iter().enumerate() {
                set_line(
                    buf,
                    area,
                    y,
                    &format!("  {:<16} {id}", label),
                    if row == *index { selected } else { normal },
                );
                y += 1;
            }
        }
        ProviderModalMode::AddForm(form) => {
            set_line(
                buf,
                area,
                y,
                &format!("Connect {} ({})", form.provider_label, form.provider_id),
                normal,
            );
            y += 2;
            let model = if form.model.text.is_empty() {
                "<optional: provider model id>"
            } else {
                &form.model.text
            };
            set_line(
                buf,
                area,
                y,
                &format!("Model ID  {model}"),
                if form.focused == 0 { selected } else { normal },
            );
            y += 2;
            let masked = if form.secret.text.is_empty() {
                "<required: API key>".to_owned()
            } else {
                "*".repeat(form.secret.text.chars().count())
            };
            set_line(
                buf,
                area,
                y,
                &format!("API key   {masked}"),
                if form.focused == 1 { selected } else { normal },
            );
            y += 2;
            set_line(
                buf,
                area,
                y,
                "The API key is never rendered or written to config.",
                dim,
            );
        }
        ProviderModalMode::ImportConfirm { include_oauth } => {
            set_line(
                buf,
                area,
                y,
                "Import credentials from ~/.local/share/opencode/auth.json?",
                normal,
            );
            y += 2;
            set_line(
                buf,
                area,
                y,
                "API records will be copied into macOS Keychain.",
                dim,
            );
            y += 1;
            set_line(
                buf,
                area,
                y,
                if *include_oauth {
                    "[x] Include OAuth records (adapter-pending)"
                } else {
                    "[ ] Include OAuth records (adapter-pending)"
                },
                normal,
            );
            y += 2;
            set_line(
                buf,
                area,
                y,
                "OAuth records are vaulted only; they cannot make requests until an adapter is enabled.",
                dim,
            );
        }
        ProviderModalMode::RemoveConfirm { id, label } => {
            set_line(buf, area, y, &format!("Remove {label} ({id})?"), normal);
            y += 2;
            set_line(
                buf,
                area,
                y,
                "This removes its Keychain credential and only a model entry marked as provider-manager-owned.",
                dim,
            );
            y += 1;
            set_line(
                buf,
                area,
                y,
                "Manually authored [model.*] entries are never deleted.",
                dim,
            );
        }
        ProviderModalMode::Busy { label } => set_line(buf, area, y, label, normal),
    }

    if let Some(message) = &state.message
        && area.height >= 2
    {
        set_line(buf, area, area.bottom() - 2, message, success);
    }
    if let Some(error_message) = &state.error
        && area.height >= 1
    {
        set_line(buf, area, area.bottom() - 1, error_message, error);
    }
}

fn set_line(buf: &mut Buffer, area: Rect, y: u16, text: &str, style: Style) {
    if y < area.bottom() {
        buf.set_stringn(area.x, y, text, area.width as usize, style);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn add_flow_moves_through_choice_form_and_busy() {
        let mut state = ProviderModalState::loading();
        state.apply_loaded(Ok(Vec::new()));
        assert!(matches!(
            handle_provider_key(&mut state, &key(KeyCode::Char('a'))),
            ProviderModalOutcome::Changed
        ));
        assert!(matches!(
            state.mode,
            ProviderModalMode::ChooseProvider { .. }
        ));
        handle_provider_key(&mut state, &key(KeyCode::Enter));
        assert!(matches!(state.mode, ProviderModalMode::AddForm(_)));
        handle_provider_key(&mut state, &key(KeyCode::Tab));
        state.handle_paste("super-secret");
        assert!(matches!(
            handle_provider_key(&mut state, &key(KeyCode::Enter)),
            ProviderModalOutcome::SubmitAdd
        ));
        assert!(matches!(state.mode, ProviderModalMode::Busy { .. }));
        assert!(state.take_pending_add().is_some());
    }

    #[test]
    fn escape_steps_back_before_closing() {
        let mut state = ProviderModalState::loading();
        state.apply_loaded(Ok(Vec::new()));
        handle_provider_key(&mut state, &key(KeyCode::Char('a')));
        handle_provider_key(&mut state, &key(KeyCode::Enter));
        assert!(matches!(
            handle_provider_key(&mut state, &key(KeyCode::Esc)),
            ProviderModalOutcome::Changed
        ));
        assert!(matches!(
            state.mode,
            ProviderModalMode::ChooseProvider { .. }
        ));
        handle_provider_key(&mut state, &key(KeyCode::Esc));
        assert!(matches!(state.mode, ProviderModalMode::Browse));
        assert!(matches!(
            handle_provider_key(&mut state, &key(KeyCode::Esc)),
            ProviderModalOutcome::Close
        ));
    }
}
