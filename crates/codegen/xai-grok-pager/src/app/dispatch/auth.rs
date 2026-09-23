//! Login, logout, account switching, and auth-code submission dispatchers.

use super::ctx::{restore_auth_return_view, show_welcome};
use super::queue::{maybe_drain_queue, note_peek_page_flip};
use super::router::dispatch;
use super::session::lifecycle::{clear_startup_actions, drain_startup_actions};
use crate::app::actions::{Action, Effect};
use crate::app::agent::AgentId;
use crate::app::agent_view::AgentView;
use crate::app::app_view::{ActiveView, AppView, AuthMode, AuthState};
use crate::scrollback::block::RenderBlock;
use crate::scrollback::blocks::SessionEvent;
use agent_client_protocol as acp;

/// `/logout`: ask the shell to clear auth, then return to the login screen.
pub(super) fn dispatch_logout(_app: &mut AppView) -> Vec<Effect> {
    vec![Effect::Logout]
}

/// Ensure `login_method_id` is populated from stored auth methods.
/// On the eager-auth path (cached token) `login_method_id` is never set, because the user skipped the login screen.
/// Does **not** invent `grok.com` when no interactive method is advertised (`preferred_method=api_key` with no key leaves `auth_methods` empty).
pub(super) fn ensure_login_method(app: &mut AppView) {
    if app.login_method_id.is_some() {
        return;
    }
    let (label, method_id, start_mode) =
        crate::acp::find_interactive_login_method(&app.auth_methods);
    if let Some(id) = method_id {
        app.login_label = label;
        app.login_method_id = Some(id);
        app.auth_start_mode = match start_mode {
            crate::acp::AuthStartMode::Pending => AuthMode::Pending,
            crate::acp::AuthStartMode::Command => AuthMode::Command,
        };
    }
    // No interactive method: leave login_method_id unset (fail-closed).
}

/// Error when no interactive login method is available (empty auth_methods, e.g. `preferred_method=api_key` with no credentials).
/// When the list is empty, prefer the shell's `PREFERRED_API_KEY_UNAVAILABLE` copy.
fn no_login_method_error(app: &AppView) -> String {
    if app.auth_methods.is_empty() {
        xai_grok_shell::agent::auth_method::PREFERRED_API_KEY_UNAVAILABLE.to_string()
    } else {
        "No login method available".to_string()
    }
}

/// Abort any in-flight Authenticate/SwitchAccount task *and* its URL poll (single-flight).
/// A new login must not stack device-code mints or let a stale poll steal the successor's URL.
/// No-op when not authenticating or when the abort handles have not been installed yet.
fn abort_prior_auth(app: &mut AppView) {
    if let AuthState::Authenticating {
        handle,
        request_seq,
        ..
    } = &mut app.auth_state
        && let Some(h) = handle.take()
    {
        tracing::debug!(
            request_seq,
            "aborting prior in-flight auth task for single-flight"
        );
        h.abort();
    }
    if let Some((seq, h)) = app.auth_url_poll_handle.take() {
        tracing::debug!(
            request_seq = seq,
            "aborting prior auth URL poll for single-flight"
        );
        h.abort();
    }
}

/// Log out, then start a new login flow in a single sequential task.
pub(super) fn dispatch_switch_account(app: &mut AppView) -> Vec<Effect> {
    ensure_login_method(app);

    let Some(method_id) = app.login_method_id.clone() else {
        app.auth_state = AuthState::Pending {
            error: Some(no_login_method_error(app)),
        };
        return vec![];
    };

    abort_prior_auth(app);

    let request_seq = app.next_auth_request_seq;
    app.next_auth_request_seq += 1;
    app.auth_code_input.reset();
    app.auth_state = AuthState::Authenticating {
        request_seq,
        handle: None,
        auth_url: None,
        mode: app.auth_start_mode,
    };

    vec![
        Effect::SwitchAccount {
            request_seq,
            method_id,
            use_oauth: app.auth_use_oauth,
        },
        Effect::PollAuthUrl { request_seq },
    ]
}

/// Scan the trailing run of session-event / system blocks for a [`SessionEvent::ReAuthRequired`] prompt.
/// Used by the `PromptResponse` handler to suppress the redundant "Turn failed" block after a 401.
/// The re-auth prompt is pushed by the `RetryState` handler, which runs first.
pub(super) fn scrollback_has_recent_reauth_prompt(
    scrollback: &crate::scrollback::state::ScrollbackState,
) -> bool {
    trailing_session_events(scrollback).any(|(_, ev)| matches!(ev, SessionEvent::ReAuthRequired))
}

/// True if the trailing run of session/system blocks has a terminal context-overflow block ([`SessionEvent::ContextTooLarge`] or `CompactionFailed`).
/// Lets `PromptResponse` suppress the redundant `TurnFailed`, mirroring reauth.
pub(super) fn scrollback_has_recent_context_too_large(
    scrollback: &crate::scrollback::state::ScrollbackState,
) -> bool {
    trailing_session_events(scrollback).any(|(_, ev)| {
        matches!(
            ev,
            SessionEvent::ContextTooLarge | SessionEvent::CompactionFailed { .. }
        )
    })
}

pub(crate) fn scrollback_has_recent_disk_full(
    scrollback: &crate::scrollback::state::ScrollbackState,
) -> bool {
    trailing_session_events(scrollback).any(|(_, ev)| matches!(ev, SessionEvent::DiskFull))
}

/// True if the trailing run already has a dedicated terminal error banner that replaces `TurnFailed`.
/// `CompactionFailed` is deliberately excluded: it can appear mid-turn.
/// On the reconcile/viewer paths a stale one must not swallow the only banner an unrelated error gets.
pub(in crate::app) fn scrollback_has_recent_error_banner(
    scrollback: &crate::scrollback::state::ScrollbackState,
) -> bool {
    trailing_session_events(scrollback).any(|(_, ev)| {
        matches!(
            ev,
            SessionEvent::ReAuthRequired
                | SessionEvent::ContextTooLarge
                | SessionEvent::DiskFull
                | SessionEvent::RequestFailed { .. }
        )
    })
}

/// True if the trailing run already has a formatted [`SessionEvent::RequestFailed`] banner.
/// Lets `PromptResponse` skip the redundant `TurnFailed`.
/// Deliberately does not match `RetryFailed`.
pub(super) fn scrollback_has_recent_request_failed(
    scrollback: &crate::scrollback::state::ScrollbackState,
) -> bool {
    trailing_session_events(scrollback)
        .any(|(_, ev)| matches!(ev, SessionEvent::RequestFailed { .. }))
}

/// The trailing run of session events, newest first: yields `(index, event)` for each session-event block at the tail of the scrollback.
/// It skips interleaved system messages and stops at the first substantive block.
/// Banners for the finishing turn live in this run; they were pushed just before its `PromptResponse` arrived.
pub(super) fn trailing_session_events(
    scrollback: &crate::scrollback::state::ScrollbackState,
) -> impl Iterator<Item = (usize, &SessionEvent)> {
    use crate::scrollback::block::RenderBlock;
    (0..scrollback.len())
        .rev()
        .map(|idx| (idx, scrollback.entry(idx).map(|e| &e.block)))
        .take_while(|(_, block)| {
            matches!(
                block,
                Some(RenderBlock::SessionEvent(_) | RenderBlock::System(_))
            )
        })
        .filter_map(|(idx, block)| match block {
            Some(RenderBlock::SessionEvent(ev)) => Some((idx, &ev.event)),
            _ => None,
        })
}

/// Strip the trailing run of auth-error blocks (the `ReAuthRequired` prompt plus any stale `RetryFailed` / `TurnFailed`) from an agent's scrollback.
/// Called after a successful mid-session re-auth so the prompt disappears once the user returns to the session.
/// Mirrors how the credit-limit upsell strips its stale blocks.
pub(super) fn strip_trailing_auth_error_blocks(agent: &mut AgentView) {
    let to_remove: Vec<usize> = trailing_session_events(&agent.scrollback)
        .filter(|(_, ev)| {
            matches!(
                ev,
                SessionEvent::ReAuthRequired
                    | SessionEvent::RequestFailed { .. }
                    | SessionEvent::RetryFailed { .. }
                    | SessionEvent::TurnFailed { .. }
            )
        })
        .map(|(idx, _)| idx)
        .collect();
    for idx in to_remove {
        agent.scrollback.remove_from(idx);
    }
}

/// Login. Triggered by pressing 'l' on an auth-pending welcome screen, by `/login` and `/auth`.
///
/// Workshop: this opens the **Subscriptions** view of the connection picker and never sends an
/// `AuthenticateRequest` by itself (gate:no-xai, Gate 2). The inherited interactive session login
/// runs only from the picker's labeled optional xAI card (`dispatch_connection_picker` →
/// `start_optional_xai_login`). A first run never comes here: it activates the OpenCode engine's
/// default model directly (`dispatch_workshop_first_run`).
pub(super) fn dispatch_login(app: &mut AppView) -> Vec<Effect> {
    dispatch_open_connection_picker(app, workshop_auth::PickerTab::Subscriptions)
}

/// Open the connection picker overlay on `tab` (`/model` → Models, `/auth` → Subscriptions).
/// Idempotent while already open. The welcome and agent views paint the overlay themselves (an
/// agent keeps its transcript visible around it); any other view stashes itself in
/// `auth_return_view` and switches to `Welcome`. Esc restores the caller's view either way.
pub(super) fn dispatch_open_connection_picker(
    app: &mut AppView,
    tab: workshop_auth::PickerTab,
) -> Vec<Effect> {
    match app.active_view {
        ActiveView::Welcome => {}
        ActiveView::Agent(_) => app.auth_return_view = Some(app.active_view),
        _ => {
            app.auth_return_view = Some(app.active_view);
            show_welcome(app);
        }
    }
    // A login attempt in flight (e.g. the optional xAI flow) is abandoned when the picker reopens.
    if matches!(app.auth_state, AuthState::Authenticating { .. }) {
        abort_prior_auth(app);
        app.auth_state = AuthState::Pending { error: None };
    }
    match app.connection_picker.as_mut() {
        Some(picker) => {
            picker.tab = tab;
            vec![]
        }
        None => {
            app.connection_picker = Some(
                workshop_auth::PickerState::new()
                    .with_tab(tab)
                    .with_active(app.workshop_connection.active_row_id()),
            );
            // Rows and rails load asynchronously: loopback local-server probe, the cached model
            // lists, and the official CLI detection (child processes on the blocking pool). No
            // auth files, no network.
            vec![Effect::WorkshopLoadPicker]
        }
    }
}

/// `/model`: the Models view, and — because the user asked for the model lists — a live refresh.
/// A freshly opened picker shows its cached rows first; the refresh is queued behind that load
/// (`WorkshopPickerLoaded` starts it) so the live rows always land last. A picker that is already
/// open refreshes right away.
pub(super) fn dispatch_open_models_view(app: &mut AppView) -> Vec<Effect> {
    let effects = dispatch_open_connection_picker(app, workshop_auth::PickerTab::Models);
    let freshly_opened = effects
        .iter()
        .any(|e| matches!(e, Effect::WorkshopLoadPicker));
    if freshly_opened {
        if let Some(picker) = app.connection_picker.as_mut() {
            picker.refresh_pending = true;
        }
        return effects;
    }
    dispatch_refresh_catalogs(app, false)
}

/// Refresh the model lists from their live sources now (the user acted: `/model`, `r`, or a
/// launch with an active connection). The engine list is re-read only from an engine that is
/// already up; nothing here installs or starts one. An open picker keeps its cached rows
/// meanwhile and shows `refreshing lists…` until the live snapshot lands.
pub(super) fn dispatch_refresh_catalogs(app: &mut AppView, force: bool) -> Vec<Effect> {
    if let Some(picker) = app.connection_picker.as_mut() {
        picker.refresh_pending = true;
        picker.refresh_in_flight = true;
    }
    vec![Effect::WorkshopRefreshCatalogs {
        force,
        engine: app.workshop_engine.clone(),
    }]
}

/// Make `conn` the active connection and remember it for the next launch. A silent fallback that
/// was carrying the session ends here: the user chose.
fn set_workshop_connection(app: &mut AppView, conn: crate::app::workshop::WorkshopConnection) {
    crate::app::workshop::save_active_connection(&conn);
    app.workshop_connection = conn;
    app.workshop_fallback = None;
}

/// First run (nothing connected yet): land in the composer with the OpenCode engine's default free
/// model active — no picker, no network until the first message (`opencode` installs itself
/// then). The placeholder shell model + anonymous session are established in-process exactly as
/// selecting the row in `/model` would; `/model` and `/auth` remain the only doors afterwards.
pub(super) fn dispatch_workshop_first_run(app: &mut AppView) -> Vec<Effect> {
    app.workshop_first_launch = true;
    set_workshop_connection(app, crate::app::workshop::first_run_connection());
    match crate::app::workshop::activate_placeholder_session() {
        Ok(key) => start_workshop_activation(app, key),
        Err(e) => {
            app.auth_state = AuthState::Pending {
                error: Some(format!("Could not write config.toml: {e}")),
            };
            vec![]
        }
    }
}

/// The OpenCode model could not start for a turn (offline, installer failed, `opencode serve`
/// down): fall back *silently* to the keyless community pool — activate its model as the shell's,
/// resend the prompt once the switch has completed (`handle_auth_complete` drains
/// `workshop_resend`), and let the composer name the model that answers. The cause goes to the
/// log; the user sees a failure line only if this fallback cannot even be set up.
pub(super) fn dispatch_workshop_engine_unavailable(
    app: &mut AppView,
    agent_id: AgentId,
    reason: String,
    text: String,
) -> Vec<Effect> {
    crate::app::workshop::log_failure_cause(&reason);
    let failed = |app: &mut AppView, cause: String| {
        crate::app::workshop::log_failure_cause(&format!("fallback unavailable: {cause}"));
        let line = crate::app::workshop::failure_line(&app.workshop_model_name());
        if let Some(agent) = app.agents.get_mut(&agent_id) {
            agent.scrollback.push_block(RenderBlock::system_error(line));
            agent.workshop_retry_prompt = Some(text.clone());
        }
        Vec::new()
    };
    let Some(kilo) = crate::app::workshop::kilo_fallback_model() else {
        return failed(app, "no fallback model in the catalog".into());
    };
    let plan = match crate::app::workshop::activate_catalog_model(&kilo) {
        Ok(plan) => plan,
        Err(e) => return failed(app, e),
    };
    if let Some(agent) = app.agents.get_mut(&agent_id) {
        // The first attempt's bubble is re-rendered by the resend; drop it so the prompt shows once.
        if let Some(entry) = app.workshop_turn_prompt_entry.take() {
            agent.scrollback.remove_entry(entry);
        }
    }
    crate::app::workshop::export_env(&plan.env);
    // The connection stays what the user has (the next launch tries it again); only this
    // process routes through the fallback, and the composer names the model that answers.
    app.workshop_fallback = Some(workshop_auth::plain_model_name(&plan.display_name));
    app.workshop_resend = Some((agent_id, text));
    app.auth_return_view = Some(ActiveView::Agent(agent_id));
    start_workshop_activation(app, plan.key)
}

/// Close the picker and return to the view it was opened from.
fn close_connection_picker(app: &mut AppView) {
    app.connection_picker = None;
    if let Some(return_view) = app.auth_return_view.take() {
        restore_auth_return_view(app, return_view);
    }
}

/// Route a key press to the open connection picker (Workshop).
pub(super) fn dispatch_connection_picker(
    app: &mut AppView,
    input: workshop_auth::PickerInput,
) -> Vec<Effect> {
    use workshop_auth::PickerOutcome;
    let Some(picker) = app.connection_picker.as_mut() else {
        return vec![];
    };
    match picker.handle(input) {
        PickerOutcome::Changed => vec![],
        PickerOutcome::Refresh => {
            // `r`: re-probe local servers and rails, and fetch every model list again regardless
            // of its cache age.
            picker.loading = true;
            picker.set_status("Refreshing…");
            dispatch_refresh_catalogs(app, true)
        }
        PickerOutcome::Close => {
            // Before any connection is configured the welcome screen stays on the (auth-pending)
            // home, never on a browser.
            close_connection_picker(app);
            vec![]
        }
        PickerOutcome::StartOptionalXaiLogin => {
            app.connection_picker = None;
            start_optional_xai_login(app)
        }
        PickerOutcome::SelectCatalog(model) => {
            match crate::app::workshop::activate_catalog_model(&model) {
                Ok(plan) => {
                    crate::app::workshop::export_env(&plan.env);
                    picker.set_status(format!(
                        "Connecting {} ({})…",
                        plan.display_name, plan.base_url
                    ));
                    set_workshop_connection(app, crate::app::workshop::WorkshopConnection::Shell);
                    start_workshop_activation(app, plan.key)
                }
                Err(e) => {
                    picker.set_status(format!("Could not activate: {e}"));
                    vec![]
                }
            }
        }
        PickerOutcome::ConnectProvider(provider_id) => {
            if provider_id == "openrouter" {
                picker
                    .set_status("Opening OpenRouter sign-in in your browser (loopback callback)…");
                vec![Effect::WorkshopOpenRouterSignIn]
            } else {
                let label = crate::app::workshop::key_prompt_label(&provider_id);
                picker.begin_key_entry(&provider_id, &label);
                vec![]
            }
        }
        PickerOutcome::SaveKey { provider_id, key } => {
            match crate::app::workshop::save_provider_key(&provider_id, &key) {
                Ok(backend) => {
                    picker.set_status(format!("Saved {provider_id} key to {backend}. Refreshing…"));
                    picker.loading = true;
                    vec![Effect::WorkshopLoadPicker]
                }
                Err(e) => {
                    picker.set_status(format!("Could not save key: {e}"));
                    vec![]
                }
            }
        }
        PickerOutcome::SelectEngine(model) => {
            set_workshop_connection(
                app,
                crate::app::workshop::WorkshopConnection::Engine { model },
            );
            // Engine/Adapter turns bypass the shell model, but the shell still needs an auth method
            // to open an ACP session (the agent view that renders the streamed turn). Establish the
            // same keyless/anonymous session Direct/Local uses; the placeholder model is never hit.
            finish_workshop_adapter_selection(app)
        }
        PickerOutcome::RailConnect(rail) => {
            let argv = crate::app::workshop::rail_login_argv(rail);
            picker.set_status(format!(
                "Running `{}` in your terminal; Workshop re-probes when it exits.",
                argv.join(" ")
            ));
            // The event loop suspends the TUI and runs the vendor login attached to the terminal.
            app.pending_workshop_login = Some((rail, argv));
            vec![]
        }
        PickerOutcome::SelectRailModel(rail, model) => {
            set_workshop_connection(
                app,
                crate::app::workshop::WorkshopConnection::Adapter { rail, model },
            );
            finish_workshop_adapter_selection(app)
        }
    }
}

/// An Engine/Adapter connection routes turns through workshop-adapters, not the shell model — but
/// the shell still needs a model + auth method to open the ACP session that backs the agent view.
/// Write a keyless placeholder model and reuse the same anonymous activation Direct/Local uses, so
/// the session is created and the streamed turn has a visible home; `dispatch_workshop_turn` then
/// intercepts prompts before they reach the placeholder. The composer label is stamped from
/// `workshop_connection` at session creation (`configure_agent_composer`) and here.
fn finish_workshop_adapter_selection(app: &mut AppView) -> Vec<Effect> {
    match crate::app::workshop::activate_placeholder_session() {
        Ok(key) => start_workshop_activation(app, key),
        Err(e) => {
            if let Some(picker) = app.connection_picker.as_mut() {
                picker.set_status(format!("Could not connect: {e}"));
            }
            vec![]
        }
    }
}

/// After config.toml gained `[model.<key>]`: reload the shell's model list, authenticate with the
/// non-interactive `xai.api_key` method (the anonymous sentinel or a real key counts), and switch
/// the active session. Completion arrives as `AuthComplete` / `AuthFailed` for `request_seq`.
fn start_workshop_activation(app: &mut AppView, model_id: String) -> Vec<Effect> {
    // Stamp the composer label: `None` for Direct/Local (Shell) → the shell model name shows; the
    // model's name for Engine/Adapter; the answering model's name while the fallback carries it.
    let label = app.workshop_label();
    for agent in app.agents.values_mut() {
        agent.workshop_model_label = label.clone();
    }
    abort_prior_auth(app);
    let request_seq = app.next_auth_request_seq;
    app.next_auth_request_seq += 1;
    app.auth_state = AuthState::Authenticating {
        request_seq,
        handle: None,
        auth_url: None,
        mode: AuthMode::Pending,
    };
    let session = match app.auth_return_view {
        Some(ActiveView::Agent(id)) => app
            .agents
            .get(&id)
            .and_then(|a| a.session.session_id.clone())
            .map(|sid| (id, sid)),
        _ => None,
    };
    vec![Effect::WorkshopActivateModel {
        request_seq,
        model_id,
        session,
    }]
}

/// The only path to the inherited xAI OIDC flow: the user selected the labeled optional xAI card
/// (and confirmed). Sends `grok.com` with `workshop_xai_opt_in`, which the shell requires before it
/// attaches the xAI OAuth2 provider when none is configured.
fn start_optional_xai_login(app: &mut AppView) -> Vec<Effect> {
    let method_id = app.login_method_id.clone().unwrap_or_else(|| {
        acp::AuthMethodId::new(xai_grok_shell::agent::auth_method::GROK_COM_METHOD_ID)
    });
    app.login_label = Some("xAI (optional)".to_owned());
    // The browser / device-code screen is drawn by the welcome view; a picker opened over a
    // session already stashed that session in `auth_return_view`.
    if !matches!(app.active_view, ActiveView::Welcome) {
        show_welcome(app);
    }

    abort_prior_auth(app);

    let request_seq = app.next_auth_request_seq;
    app.next_auth_request_seq += 1;
    app.auth_code_input.reset();
    app.auth_state = AuthState::Authenticating {
        request_seq,
        handle: None,
        auth_url: None,
        mode: app.auth_start_mode,
    };

    vec![
        Effect::Authenticate {
            request_seq,
            method_id,
            use_oauth: app.auth_use_oauth,
            force_interactive: true,
            xai_opt_in: true,
        },
        Effect::PollAuthUrl { request_seq },
    ]
}

/// Only meaningful when `auth_return_view` is set (a mid-session `/login` or 401 re-auth prompt).
/// Aborts the in-flight auth task and tells the shell to cancel its device/loopback flow so a retry does not race a still-polling prior mint.
/// Bump the seq so a fresh login does not collide with a late `AuthComplete`/`AuthFailed`.
pub(super) fn dispatch_cancel_login(app: &mut AppView) -> Vec<Effect> {
    let Some(return_view) = app.auth_return_view.take() else {
        return vec![];
    };
    // Capture the attempt's request_seq before abort clears Authenticating, so the shell cancel is scoped to this attempt only
    // A delayed RPC must not cancel a fast re-login
    let cancel_seq = match &app.auth_state {
        AuthState::Authenticating { request_seq, .. } => Some(*request_seq),
        _ => None,
    };
    abort_prior_auth(app);
    app.next_auth_request_seq += 1;
    app.auth_state = AuthState::Done;
    app.auth_show_raw_url = false;
    app.auth_code_input.reset();
    // Workshop: a picker opened mid-session closes with the login it hosted.
    app.connection_picker = None;
    restore_auth_return_view(app, return_view);
    // This runs on all agents because the login may have been started from the dashboard
    // Clearing the stash alone is not enough
    // A leftover `ReAuthRequired` block would let a later `PromptResponse` re-detect it via `scrollback_has_recent_reauth_prompt`
    for agent in app.agents.values_mut() {
        agent.reauth_stashed_prompt = None;
        strip_trailing_auth_error_blocks(agent);
    }
    // Ask the shell to cancel its in-flight interactive auth (device poll / loopback wait)
    // Fire-and-forget: UI state is already restored
    match cancel_seq {
        Some(request_seq) => vec![Effect::CancelAuth { request_seq }],
        None => vec![],
    }
}

/// User submitted a manually-pasted auth token in loopback mode.
pub(super) fn dispatch_submit_auth_code(app: &mut AppView, code: String) -> Vec<Effect> {
    let request_seq = match &app.auth_state {
        AuthState::Authenticating { request_seq, .. } => *request_seq,
        _ => return vec![],
    };

    vec![Effect::SubmitAuthCode { request_seq, code }]
}

// TaskResult handlers.

pub(super) fn handle_auth_complete(
    app: &mut AppView,
    request_seq: u64,
    meta: Option<serde_json::Value>,
) -> Vec<Effect> {
    if let AuthState::Authenticating {
        request_seq: current_seq,
        ..
    } = &app.auth_state
        && *current_seq == request_seq
    {
        if let Some(meta_val) = meta.as_ref()
            && let Ok(auth_meta) =
                serde_json::from_value::<xai_grok_login::AuthMeta>(meta_val.clone())
        {
            app.apply_auth_meta(&auth_meta);
        }

        app.auth_state = AuthState::Done;
        app.auth_show_raw_url = false;
        app.welcome_prompt_focused = !app.is_access_blocked();
        app.auth_code_input.reset();

        // Workshop: a connection activation started from the picker has now reloaded the model and
        // authenticated (Direct/Local) or established the anonymous session (Engine/Adapter); close
        // the picker and hand the user back to the home prompt. The composer label is stamped at
        // session creation from `workshop_connection` (see `configure_agent_composer`).
        if app.connection_picker.take().is_some() {
            let label = app.workshop_label().unwrap_or_else(|| "model".to_owned());
            app.show_toast(&format!("Connected: {label}"));
        }

        // Mid-session re-auth (`/login` or a 401 prompt): restore the view the user was on instead of running the startup load-session flow
        // The session state lives in `app.agents`, independent of `active_view`, so it is preserved across the auth detour
        if let Some(return_view) = app.auth_return_view.take() {
            restore_auth_return_view(app, return_view);
            // Mid-session re-auth returns to the existing session, not the startup flow
            // Discard any deferred startup stash rather than leaving it to fire later
            // One example: an incidental `Ctrl+N` pressed during /login that the chokepoint deferred
            clear_startup_actions(app);
            // Re-auth succeeded: hide the now-stale re-auth prompt (and any trailing error blocks) so the user returns to a clean session
            // Mirrors how the credit-limit upsell strips its stale blocks
            // Auth is global, so handle every agent (the login may have been started from the dashboard, not the agent that 401'd)
            let mut retry_effects = Vec::new();
            let mut page_flips = Vec::new();
            // Workshop: the prompt whose OpenCode turn could not start goes out again on the
            // fallback model that has just been activated — silently; the composer already
            // names the model that will answer (`dispatch_workshop_engine_unavailable`).
            if let Some((id, text)) = app.workshop_resend.take()
                && let Some(agent) = app.agents.get_mut(&id)
            {
                agent
                    .session
                    .enqueue_entry(text, crate::app::agent::QueueEntryKind::Prompt);
                let drain = maybe_drain_queue(agent, &mut app.pending_image_notices);
                retry_effects.extend(drain.effects);
                page_flips.push((agent.session.id, drain.page_flip_entry));
            }
            for agent in app.agents.values_mut() {
                strip_trailing_auth_error_blocks(agent);
                // Auto-resubmit the prompt that failed on the expired login so the user doesn't have to retype it
                // The user couldn't have queued another prompt during the auth detour, so a plain front-enqueue and drain is safe
                if let Some(prompt) = agent.reauth_stashed_prompt.take() {
                    agent.scrollback.push_block(RenderBlock::system(
                        "Re-authenticated. Retrying\u{2026}".to_string(),
                    ));
                    agent.session.enqueue_in_flight_prompt_front(prompt);
                    let drain = maybe_drain_queue(agent, &mut app.pending_image_notices);
                    retry_effects.extend(drain.effects);
                    page_flips.push((agent.session.id, drain.page_flip_entry));
                }
            }
            for (id, page_flip_entry) in page_flips {
                note_peek_page_flip(app, id, page_flip_entry);
            }
            let mut effects = dispatch(Action::RequestBundleStatus, app);
            if app.usage_visible {
                effects.push(Effect::FetchAppBilling { nonce: 0 });
            }
            effects.extend(retry_effects);
            return effects;
        }

        // Request bundle status only; the shell auto-syncs after auth
        let mut effects = dispatch(Action::RequestBundleStatus, app);

        // Start auto-checking subscription if gated.
        // Check immediately (don't wait 5s) then schedule the timer.
        if !app.has_access() {
            app.paywall_check_started = Some(std::time::Instant::now());
            effects.push(Effect::CheckSubscription { verify: None });
            effects.push(Effect::SchedulePaywallCheck);
        }
        // Fetch billing so the welcome screen can show a credit warning.
        if app.usage_visible {
            effects.push(Effect::FetchAppBilling { nonce: 0 });
        }
        // Fetch changelog (mirrors startup path for interactive login).
        effects.push(Effect::FetchChangelog);

        // ZDR-blocked users stay on the welcome screen; discard any deferred startup (they cannot start a session)
        if app.is_zdr_blocked() {
            clear_startup_actions(app);
            return effects;
        }

        // Replay deferred session startup once both gates are open
        // If trust is still Pending its question renders next and its answer drains instead
        // The trust handlers use the same predicate, so the deferred startup runs exactly once after whichever gate resolves last
        if app.session_startup_allowed() {
            effects.extend(drain_startup_actions(app));
        }
        return effects;
    }
    vec![]
}

pub(super) fn handle_auth_url_ready(
    app: &mut AppView,
    request_seq: u64,
    auth_url: Option<String>,
    external: bool,
    mode: Option<String>,
) -> Vec<Effect> {
    if let AuthState::Authenticating {
        request_seq: current_seq,
        auth_url: current_url,
        mode: current_mode,
        ..
    } = &mut app.auth_state
        && *current_seq == request_seq
    {
        *current_url = auth_url;
        // Prefer `mode`; fall back to `external` for older agents
        // An old-agent device login lands on Loopback (harmless paste box; the background poll still completes)
        *current_mode = match mode.as_deref() {
            Some("device") => AuthMode::Device,
            Some("command") => AuthMode::Command,
            Some("loopback") => AuthMode::Loopback,
            _ if external => AuthMode::Command,
            _ => AuthMode::Loopback,
        };
    }
    vec![]
}

pub(super) fn handle_mcp_auth_trigger_done(
    app: &mut AppView,
    agent_id: AgentId,
    server_name: String,
    result: Result<crate::app::actions::McpAuthTriggerOutcome, String>,
) -> Vec<Effect> {
    let Some(agent) = app.agents.get_mut(&agent_id) else {
        return vec![];
    };
    if let Some(ref mut modal) = agent.extensions_modal {
        modal.pending_action = None;
        modal.pending_entry_index = None;
        match result {
            Ok(crate::app::actions::McpAuthTriggerOutcome::Authenticated) => {}
            Ok(crate::app::actions::McpAuthTriggerOutcome::SetupRequired(setup)) => {
                let setup_values = match &modal.mcps_data {
                    crate::views::extensions_modal::TabDataState::Loaded(servers) => servers
                        .iter()
                        .find(|server| server.name == server_name)
                        .map(|server| server.setup_values.clone())
                        .unwrap_or_default(),
                    _ => std::collections::HashMap::new(),
                };
                if let Some(form) = crate::views::extensions_modal::McpSetupFormState::from_setup(
                    server_name.clone(),
                    setup,
                    setup_values,
                ) {
                    modal.mcp_setup = Some(form);
                } else {
                    modal.modal_message =
                        Some(crate::views::extensions_modal::ModalMessage::Error(
                            format!("{server_name}: setup schema is not supported in this UI"),
                        ));
                }
                return vec![];
            }
            Err(e) => {
                let msg = if e.starts_with("To authenticate") {
                    format!("{server_name}: {e}")
                } else if e.contains(&server_name) {
                    format!("Auth failed: {e}")
                } else {
                    format!("{server_name} auth failed: {e}")
                };
                modal.modal_message =
                    Some(crate::views::extensions_modal::ModalMessage::Error(msg));
                if let Some(session_id) = agent.session.session_id.clone() {
                    return vec![Effect::FetchMcpsList {
                        agent_id,
                        session_id,
                        cache: false,
                    }];
                }
                return vec![];
            }
        }
    }
    // No toast on success: the row transition from the FetchMcpsList refresh below is the confirmation
    let Some(session_id) = agent.session.session_id.clone() else {
        return vec![];
    };
    vec![Effect::FetchMcpsList {
        agent_id,
        session_id,
        cache: false,
    }]
}

pub(super) fn handle_mcp_setup_submit_done(
    app: &mut AppView,
    agent_id: AgentId,
    server_name: String,
    result: Result<(), String>,
) -> Vec<Effect> {
    let Some(agent) = app.agents.get_mut(&agent_id) else {
        return vec![];
    };
    if let Some(ref mut modal) = agent.extensions_modal {
        if let Err(e) = result {
            modal.pending_action = None;
            modal.pending_entry_index = None;
            modal.modal_message = Some(crate::views::extensions_modal::ModalMessage::Error(
                format!("{server_name} setup failed: {e}"),
            ));
            return vec![];
        }
        modal.pending_action = Some(format!("Authenticating {server_name}..."));
        modal.pending_entry_index = None;
    }
    let Some(session_id) = agent.session.session_id.clone() else {
        if let Some(ref mut modal) = agent.extensions_modal {
            modal.pending_action = None;
            modal.modal_message = Some(crate::views::extensions_modal::ModalMessage::Error(
                format!("{server_name}: no active session for authentication"),
            ));
        }
        return vec![];
    };
    vec![Effect::McpAuthTrigger {
        agent_id,
        session_id,
        server_name,
    }]
}
