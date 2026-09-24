//! Tests for login, logout, account switching, and auth-code dispatchers.

use super::*;

#[test]
fn cta_mcps_loaded_needs_auth_opens_modal_and_seeds() {
    use crate::app::agent_view::CtaPhase;
    use crate::views::extensions_modal::{ExtensionsTab, TabDataState};
    use crate::views::mcps_modal::{McpSectionId, McpServerDisplayStatus, section_key};
    let mut app = test_app_with_agent();
    app.team_id = Some("team-uuid".into());
    let id = AgentId(0);
    app.agents.get_mut(&id).unwrap().plugin_cta.phase = CtaPhase::AwaitingMcps {
        name: "figma".into(),
    };
    let servers = vec![
        cta_mcp_server("grok_com_managed", None, McpServerDisplayStatus::Ready),
        cta_mcp_server("local-srv", None, McpServerDisplayStatus::Ready),
        cta_mcp_server("other-srv", Some("slack"), McpServerDisplayStatus::Ready),
        cta_mcp_server(
            "figma-srv",
            Some("figma"),
            McpServerDisplayStatus::NeedsAuth,
        ),
    ];
    let effects = dispatch(
        Action::TaskComplete(TaskResult::PluginCtaMcpsLoaded {
            agent_id: id,
            plugin_name: "figma".into(),
            result: Ok(servers),
        }),
        &mut app,
    );
    // The CTA is finished; the modal owns the flow from here
    assert_eq!(test_agent(&app, id).plugin_cta.phase, CtaPhase::Hidden);
    // Modal opened to the MCP Servers tab.
    let modal = test_agent(&app, id)
        .extensions_modal
        .as_ref()
        .expect("extensions modal should be open");
    assert_eq!(modal.active_tab, ExtensionsTab::McpServers);
    // Session team id seeded so the Managed subtitle deep link matches Ctrl+O.
    assert_eq!(modal.session_team_id.as_deref(), Some("team-uuid"));
    // MCP tab seeded directly from the read we already have (no flash).
    match &modal.mcps_data {
        TabDataState::Loaded(servers) => assert_eq!(servers.len(), 4),
        other => panic!("expected mcps_data Loaded, got {other:?}"),
    }
    // Managed, Local, and other plugins collapsed; only the target plugin expanded
    let collapsed = &modal.mcps_collapsed_sections;
    assert!(collapsed.contains(&section_key(&McpSectionId::Managed)));
    assert!(collapsed.contains(&section_key(&McpSectionId::Local)));
    assert!(collapsed.contains(&section_key(&McpSectionId::Plugin("slack".into()))));
    assert!(!collapsed.contains(&section_key(&McpSectionId::Plugin("figma".into()))));
    assert!(modal.mcps_section_collapse_initialized);
    // Emits the same full set of tab fetches as a manual open so no tab is stuck Loading, plus the candidate refresh
    assert_eq!(
        effects
            .iter()
            .filter(|e| matches!(e, Effect::FetchHooksList { .. }))
            .count(),
        1
    );
    assert_eq!(
        effects
            .iter()
            .filter(|e| matches!(e, Effect::FetchPluginsList { .. }))
            .count(),
        1
    );
    assert_eq!(
        effects
            .iter()
            .filter(|e| matches!(e, Effect::FetchMarketplaceList { .. }))
            .count(),
        1
    );
    assert_eq!(
        effects
            .iter()
            .filter(|e| matches!(e, Effect::FetchMcpsList { .. }))
            .count(),
        1
    );
    assert_eq!(
        effects
            .iter()
            .filter(|e| matches!(e, Effect::FetchSkillsList { .. }))
            .count(),
        1
    );
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::FetchPluginCtaCatalog { .. }))
    );
}

#[test]
fn cta_mcps_loaded_no_needs_auth_terminal_sets_installed() {
    use crate::app::agent_view::CtaPhase;
    use crate::views::mcps_modal::McpServerDisplayStatus;
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    {
        let cta = &mut app.agents.get_mut(&id).unwrap().plugin_cta;
        cta.phase = CtaPhase::AwaitingMcps {
            name: "figma".into(),
        };
        cta.expects_mcp = true;
    }
    // The plugin server is present and Ready (terminal, no auth), so the CTA settles now
    let servers = vec![cta_mcp_server(
        "figma-srv",
        Some("figma"),
        McpServerDisplayStatus::Ready,
    )];
    let effects = dispatch(
        Action::TaskComplete(TaskResult::PluginCtaMcpsLoaded {
            agent_id: id,
            plugin_name: "figma".into(),
            result: Ok(servers),
        }),
        &mut app,
    );
    assert_eq!(
        test_agent(&app, id).plugin_cta.phase,
        CtaPhase::Installed {
            name: "figma".into()
        }
    );
    assert!(test_agent(&app, id).extensions_modal.is_none());
    // No modal repopulation; settling emits the auto-dismiss timer and the candidate refresh, and never re-fetches the MCP list
    assert!(
        !effects
            .iter()
            .any(|e| matches!(e, Effect::FetchMcpsList { .. }))
    );
    assert!(
        !effects
            .iter()
            .any(|e| matches!(e, Effect::RetryPluginCtaMcps { .. }))
    );
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::DismissCtaInstalled { .. }))
    );
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::FetchPluginCtaCatalog { .. }))
    );
}

#[test]
fn cta_mcps_loaded_later_needs_auth_opens_handoff() {
    use crate::app::agent_view::CtaPhase;
    use crate::views::mcps_modal::McpServerDisplayStatus;
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    {
        let cta = &mut app.agents.get_mut(&id).unwrap().plugin_cta;
        cta.phase = CtaPhase::AwaitingMcps {
            name: "figma".into(),
        };
        cta.expects_mcp = true;
        // Several polls already elapsed before the server reached NeedsAuth.
        cta.mcp_attempt = 5;
    }
    let effects = dispatch(
        Action::TaskComplete(TaskResult::PluginCtaMcpsLoaded {
            agent_id: id,
            plugin_name: "figma".into(),
            result: Ok(vec![cta_mcp_server(
                "figma-srv",
                Some("figma"),
                McpServerDisplayStatus::NeedsAuth,
            )]),
        }),
        &mut app,
    );
    // NeedsAuth is terminal: the modal opens immediately even mid-poll
    assert_eq!(test_agent(&app, id).plugin_cta.phase, CtaPhase::Hidden);
    assert!(test_agent(&app, id).extensions_modal.is_some());
    assert!(
        !effects
            .iter()
            .any(|e| matches!(e, Effect::RetryPluginCtaMcps { .. }))
    );
}

/// A bash command typed while a turn is running goes straight to the server: an Effect and an optimistic echo, no local queue entry.
#[test]
fn bash_while_running_is_server_authoritative() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    app.agents.get_mut(&id).unwrap().session.state = AgentState::TurnRunning;

    let effects = dispatch(Action::SendBashCommand("ls -la".into()), &mut app);
    let Some(effect) = effects.first() else {
        panic!("expected an effect, got {effects:?}");
    };
    let pid = match effect {
        Effect::SendBashCommand {
            command, prompt_id, ..
        } => {
            assert_eq!(command, "ls -la");
            prompt_id.clone()
        }
        other => panic!("expected immediate SendBashCommand, got {other:?}"),
    };
    assert_eq!(test_agent(&app, id).session.queue_len(), 0);
    // Optimistic echo present with kind="bash".
    let q = app
        .shared_prompt_queue("test-session")
        .expect("echo present");
    assert_eq!(q.len(), 1);
    let Some(front) = q.first() else {
        panic!("expected queue echo: {q:?}");
    };
    assert_eq!(front.id, pid);
    assert_eq!(front.kind, "bash");
    assert_eq!(front.text, "ls -la");
}

#[test]
fn auth_complete_triggers_bundle_status_fetch() {
    let mut app = test_app();
    app.auth_state = AuthState::Authenticating {
        request_seq: 1,
        handle: None,
        auth_url: None,
        mode: AuthMode::Pending,
    };

    let effects = dispatch(
        Action::TaskComplete(TaskResult::AuthComplete {
            request_seq: 1,
            meta: None,
        }),
        &mut app,
    );

    assert!(matches!(app.auth_state, AuthState::Done));
    // The pager only refreshes the on-disk catalog snapshot; the bundle download runs inside the shell after auth
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::FetchBundleStatus))
    );
}

#[test]
fn auth_complete_with_deferred_load_also_fetches_status() {
    let mut app = test_app();
    app.auth_state = AuthState::Authenticating {
        request_seq: 1,
        handle: None,
        auth_url: None,
        mode: AuthMode::Pending,
    };
    app.deferred_startup.session =
        Some(crate::app::session_startup::DeferredSessionStartup::Load {
            session_id: "test-session".into(),
            session_cwd: None,
            chat_kind: false,
        });

    let effects = dispatch(
        Action::TaskComplete(TaskResult::AuthComplete {
            request_seq: 1,
            meta: None,
        }),
        &mut app,
    );

    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::FetchBundleStatus))
    );
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::LoadSession { .. }))
    );
    assert!(app.deferred_startup.session.is_none());
}

/// `/login` from the welcome screen (startup, logged out) must not stash a return view; the normal login-then-load flow is preserved.
#[test]
fn login_from_welcome_does_not_stash_return_view() {
    let mut app = test_app();
    assert_eq!(app.active_view, ActiveView::Welcome);

    dispatch(Action::Login, &mut app);

    assert_eq!(app.active_view, ActiveView::Welcome);
    assert_eq!(app.auth_return_view, None);
}

/// Compact-auth recovery: the prompt is held across an auto-compact 401, stashed on PromptResponse, and resubmitted on a mid-session AuthComplete.
#[test]
fn e2e_compact_auth_failure_holds_prompt_and_resubmits_after_login() {
    use crate::app::acp_handler::apply_session_event_for_test;
    use crate::app::agent::{AgentState, InFlightPrompt};
    use crate::scrollback::EntryId;
    use crate::scrollback::block::RenderBlock;
    use xai_grok_shell::extensions::notification::{RetryState, SessionUpdate as XaiSessionUpdate};

    let mut app = test_app_with_agent();
    let id = AgentId(0);
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.session.state = AgentState::TurnRunning;
        agent.turn_started_at = Some(std::time::Instant::now());
        agent.session.session_id = Some(acp::SessionId::new("sess-compact-auth-e2e"));
        agent.session.current_prompt_id = Some("prompt-1".into());
        agent.session.in_flight_prompt = Some(InFlightPrompt {
            text: "please continue after login".into(),
            images: Vec::new(),
            scrollback_entry: EntryId::new(1),
            combined_scrollback_entries: Vec::new(),
            chip_elements: Vec::new(),
        });

        apply_session_event_for_test(
            &XaiSessionUpdate::AutoCompactStarted {
                tokens_used: 180_000,
                context_window: 200_000,
                percentage: 90,
                reason: "threshold".into(),
            },
            &mut agent.session,
            &mut agent.scrollback,
        );
        assert!(
            agent.session.in_flight_prompt.is_none(),
            "cancel rewind must still be blocked mid-compact"
        );
        assert_eq!(
            agent
                .session
                .compact_held_prompt
                .as_ref()
                .map(|p| p.text.as_str()),
            Some("please continue after login"),
            "must hold the prompt text for reauth auto-resubmit"
        );

        apply_session_event_for_test(
            &XaiSessionUpdate::AutoCompactFailed {
                error: "authentication problem — re-authenticate using /login and retry.".into(),
            },
            &mut agent.session,
            &mut agent.scrollback,
        );
        assert!(agent.session.compact_held_prompt.is_some());

        apply_session_event_for_test(
            &XaiSessionUpdate::RetryState(RetryState::Failed {
                error_type: "auth".into(),
                message: "Unauthorized (401): compaction failed".into(),
            }),
            &mut agent.session,
            &mut agent.scrollback,
        );
        let has_reauth = (0..agent.scrollback.len()).any(|i| {
            matches!(
                agent.scrollback.entry(i).map(|e| &e.block),
                Some(RenderBlock::SessionEvent(ev))
                    if matches!(ev.event, SessionEvent::ReAuthRequired)
            )
        });
        assert!(has_reauth, "RetryState auth must show ReAuthRequired");
    }

    dispatch(
        Action::TaskComplete(TaskResult::PromptResponse {
            agent_id: id,
            result: Err("Unauthorized (401)".to_string()),
            http_status: Some(401),
            prompt_id: Some("prompt-1".into()),
        }),
        &mut app,
    );
    assert_eq!(
        test_agent(&app, id)
            .reauth_stashed_prompt
            .as_ref()
            .map(|p| p.text.as_str()),
        Some("please continue after login"),
        "PromptResponse must stash the compact-held prompt for AuthComplete"
    );

    start_login_flow(&mut app);
    let seq = authenticating_seq(&app);
    let effects = dispatch(
        Action::TaskComplete(TaskResult::AuthComplete {
            request_seq: seq,
            meta: None,
        }),
        &mut app,
    );
    assert!(
        test_agent(&app, id).reauth_stashed_prompt.is_none(),
        "stash consumed on AuthComplete"
    );
    assert!(
        effects.iter().any(|e| matches!(
            e,
            Effect::SendPrompt { .. } | Effect::SendPromptBlocks { .. }
        )),
        "AuthComplete must resubmit the prompt so compact runs again with valid auth, got: {effects:?}"
    );
}

/// Without `compact_held_prompt`, clearing `in_flight_prompt` when compact starts leaves nothing for reauth to stash.
#[test]
fn pre_fix_compact_start_without_hold_cannot_stash_for_reauth() {
    use crate::app::agent::AgentState;
    use crate::scrollback::block::RenderBlock;

    let mut app = test_app_with_agent();
    let id = AgentId(0);
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.session.state = AgentState::TurnRunning;
        agent.turn_started_at = Some(std::time::Instant::now());
        agent.session.session_id = Some(acp::SessionId::new("sess-pre-fix"));
        agent.session.current_prompt_id = Some("p1".into());
        agent.session.in_flight_prompt = None;
        agent.session.compact_held_prompt = None;
        agent
            .scrollback
            .push_block(RenderBlock::session_event(SessionEvent::ReAuthRequired));
    }
    dispatch(
        Action::TaskComplete(TaskResult::PromptResponse {
            agent_id: id,
            result: Err("Unauthorized (401)".to_string()),
            http_status: Some(401),
            prompt_id: Some("p1".into()),
        }),
        &mut app,
    );
    assert!(
        test_agent(&app, id).reauth_stashed_prompt.is_none(),
        "without compact_held / in_flight, reauth cannot stash — the pre-fix bug"
    );
}

/// A second auth-failed turn with no rewindable prompt (`in_flight_prompt == None`) must not clobber the stash from an earlier 401.
#[test]
fn second_auth_failure_does_not_clobber_reauth_stash() {
    use crate::scrollback::block::RenderBlock;
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.reauth_stashed_prompt = Some(crate::app::agent::InFlightPrompt {
            text: "first prompt".into(),
            images: Vec::new(),
            scrollback_entry: crate::scrollback::EntryId::new(0),
            combined_scrollback_entries: Vec::new(),
            chip_elements: Vec::new(),
        });
        agent
            .scrollback
            .push_block(RenderBlock::session_event(SessionEvent::ReAuthRequired));
        agent.session.state = AgentState::TurnRunning;
        agent.turn_started_at = Some(std::time::Instant::now());
        agent.session.in_flight_prompt = None;
    }

    dispatch(
        Action::TaskComplete(TaskResult::PromptResponse {
            agent_id: id,
            result: Err("Unauthorized (401)".to_string()),
            http_status: Some(401),
            prompt_id: None,
        }),
        &mut app,
    );

    assert_eq!(
        test_agent(&app, id)
            .reauth_stashed_prompt
            .as_ref()
            .map(|prompt| prompt.text.as_str()),
        Some("first prompt"),
        "a None in_flight_prompt must not wipe an earlier stash"
    );
}

/// Cancelling a mid-session re-auth drops the stashed prompt so it is not silently resubmitted on a later, unrelated login.
#[test]
fn cancel_login_drops_reauth_stashed_prompt() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    app.agents.get_mut(&id).unwrap().reauth_stashed_prompt =
        Some(crate::app::agent::InFlightPrompt {
            text: "stale".into(),
            images: Vec::new(),
            scrollback_entry: crate::scrollback::EntryId::new(0),
            combined_scrollback_entries: Vec::new(),
            chip_elements: Vec::new(),
        });

    dispatch(Action::Login, &mut app);
    dispatch(Action::CancelLogin, &mut app);

    assert!(
        test_agent(&app, id).reauth_stashed_prompt.is_none(),
        "cancelling re-auth must drop the stashed prompt"
    );
}

/// Cancelling a mid-session re-auth strips the stale `ReAuthRequired` prompt from scrollback.
/// A later `PromptResponse` can then no longer re-detect it and re-stash the prompt for silent resubmission.
#[test]
fn cancel_login_strips_reauth_prompt_from_scrollback() {
    use crate::scrollback::block::RenderBlock;
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.reauth_stashed_prompt = Some(crate::app::agent::InFlightPrompt {
            text: "stale".into(),
            images: Vec::new(),
            scrollback_entry: crate::scrollback::EntryId::new(0),
            combined_scrollback_entries: Vec::new(),
            chip_elements: Vec::new(),
        });
        agent
            .scrollback
            .push_block(RenderBlock::session_event(SessionEvent::ReAuthRequired));
    }

    dispatch(Action::Login, &mut app);
    dispatch(Action::CancelLogin, &mut app);

    let sb = &test_agent(&app, id).scrollback;
    let has_reauth = (0..sb.len()).any(|i| {
        matches!(
            sb.entry(i).map(|e| &e.block),
            Some(RenderBlock::SessionEvent(ev)) if matches!(ev.event, SessionEvent::ReAuthRequired)
        )
    });
    assert!(
        !has_reauth,
        "cancelling re-auth must strip the stale re-auth prompt from scrollback"
    );
}

/// Empty `auth_methods` (Workshop cold start, or a `preferred_method` pin that is unavailable) must not invent
/// `grok.com` or start an OIDC flow the agent did not advertise: Login opens the connection picker instead.
#[test]
fn login_with_empty_auth_methods_opens_picker_and_fails_closed() {
    let mut app = test_app_with_agent();
    app.auth_methods.clear();
    app.login_method_id = None;

    let effects = dispatch(Action::Login, &mut app);

    // Opening the picker loads its rows/rails asynchronously (`WorkshopLoadPicker`); that is a data
    // probe, never an auth flow. The invariant is that Login alone starts no `Authenticate`.
    assert!(
        !effects
            .iter()
            .any(|e| matches!(e, Effect::Authenticate { .. })),
        "must not start Authenticate without an advertised method, got {effects:?}"
    );
    assert!(
        effects
            .iter()
            .all(|e| matches!(e, Effect::WorkshopLoadPicker)),
        "Login only loads the picker, got {effects:?}"
    );
    assert!(
        app.connection_picker.is_some(),
        "Login must open the connection picker"
    );
    assert_eq!(
        app.active_view,
        ActiveView::Agent(AgentId(0)),
        "the picker is an overlay: the session stays up behind it"
    );
    assert_eq!(
        app.auth_return_view,
        Some(ActiveView::Agent(AgentId(0))),
        "closing the picker returns to the session"
    );
    assert!(
        !matches!(app.auth_state, AuthState::Authenticating { .. }),
        "no login flow may start from Login alone, got {:?}",
        app.auth_state
    );
    assert!(app.login_method_id.is_none());

    // Esc closes the picker and returns to the session; still nothing was sent.
    let effects = dispatch(
        Action::ConnectionPicker(workshop_auth::PickerInput::Back),
        &mut app,
    );
    assert!(effects.is_empty());
    assert!(app.connection_picker.is_none());
    assert_eq!(app.active_view, ActiveView::Agent(AgentId(0)));
}

/// First run (cold start, nothing connected): no picker, the OpenCode engine's default model is
/// the active connection, and the only effect is the in-process activation (model reload +
/// anonymous auth) — never `Authenticate`, never a data probe. `AuthComplete` then hands the user
/// the focused home composer.
#[test]
fn first_run_lands_in_the_composer_with_the_engine_default_and_no_picker() {
    let mut app = test_app();
    app.auth_methods.clear();
    app.login_method_id = None;
    app.auth_state = AuthState::Pending { error: None };
    app.welcome_prompt_focused = false;

    let effects = dispatch(Action::WorkshopFirstRun, &mut app);

    assert!(app.connection_picker.is_none(), "first run shows no picker");
    let crate::app::workshop::WorkshopConnection::Engine { model } = &app.workshop_connection
    else {
        panic!(
            "expected the engine connection, got {:?}",
            app.workshop_connection
        );
    };
    assert!(model.is_default, "the engine's own default model is active");
    assert_eq!(
        app.workshop_connection.composer_label().as_deref(),
        Some("Big Pickle"),
        "the composer names the model only"
    );
    assert!(
        app.workshop_first_launch,
        "the first launch carries the doors hint"
    );
    assert_eq!(effects.len(), 1, "exactly the activation, got {effects:?}");
    assert!(
        matches!(
            effects.first(),
            Some(Effect::WorkshopActivateModel { session: None, .. })
        ),
        "first run activates the placeholder session in-process, got {effects:?}"
    );
    let AuthState::Authenticating { request_seq, .. } = app.auth_state else {
        panic!("activation in flight, got {:?}", app.auth_state);
    };

    let effects = dispatch(
        Action::TaskComplete(TaskResult::AuthComplete {
            request_seq,
            meta: None,
        }),
        &mut app,
    );
    assert!(matches!(app.auth_state, AuthState::Done));
    assert!(
        app.welcome_prompt_focused,
        "the composer is ready to type into"
    );
    assert!(app.connection_picker.is_none());
    assert!(
        !effects
            .iter()
            .any(|e| matches!(e, Effect::Authenticate { .. })),
        "no login flow ever starts on a first run, got {effects:?}"
    );
}

/// `/model` opens the picker on the active model, `/auth` (and `/login`) the same picker on the
/// Subscriptions section; while open the other command just moves the selection. The active
/// engine row is preselected.
#[test]
fn model_and_auth_open_one_picker_and_move_while_open() {
    use workshop_auth::{PickerFocus, RowKind};
    let models = || PickerFocus::Models {
        filter: String::new(),
    };
    let mut app = test_app();
    app.workshop_connection = crate::app::workshop::first_run_connection();
    let effects = dispatch(Action::OpenConnectionPicker(models()), &mut app);
    // `/model` loads the cached rows at once and, because the user asked for the model lists,
    // queues a refresh from their live sources behind that load (cache age respected; `r` forces).
    assert!(
        matches!(effects.as_slice(), [Effect::WorkshopLoadPicker]),
        "got {effects:?}"
    );
    let picker = app.connection_picker.as_ref().expect("picker open");
    assert_eq!(picker.title(), "Models");
    assert!(
        picker.refresh_pending && !picker.refresh_in_flight,
        "the overlay says the lists are refreshing; the fetch waits for the cached load"
    );
    assert!(
        picker.selected_row().is_some_and(
            |r| matches!(&r.kind, RowKind::Engine(m) if m.is_default) && picker.is_active(&r)
        ),
        "the active engine model is the highlighted row"
    );
    let cached = || workshop_auth::PickerSnapshot {
        rows: workshop_auth::models_rows(
            &workshop_providers::Catalog::builtin(),
            |_| false,
            &[],
            &[],
        ),
        ..workshop_auth::PickerSnapshot::default()
    };
    let effects = dispatch(
        Action::TaskComplete(TaskResult::WorkshopPickerLoaded(cached())),
        &mut app,
    );
    assert!(
        matches!(
            effects.as_slice(),
            [Effect::WorkshopRefreshCatalogs {
                force: false,
                engine: None
            }]
        ),
        "the cached load starts the live refresh, got {effects:?}"
    );
    let picker = app.connection_picker.as_ref().expect("picker open");
    assert!(picker.refresh_pending && picker.refresh_in_flight);
    assert!(
        picker.selected_row().is_some_and(|r| picker.is_active(&r)),
        "the cached load keeps the active row highlighted"
    );
    // A second cached snapshot does not start a second refresh.
    let effects = dispatch(
        Action::TaskComplete(TaskResult::WorkshopPickerLoaded(cached())),
        &mut app,
    );
    assert!(effects.is_empty(), "got {effects:?}");
    assert!(dispatch(Action::Login, &mut app).is_empty());
    let picker = app.connection_picker.as_ref().unwrap();
    assert_eq!(picker.title(), "Models", "one picker, one title");
    assert!(
        picker.selected_row().is_some_and(|r| r.is_vendor()),
        "/auth lands on the first subscription row"
    );
    // `/model` on an already open picker: back to the active model and refresh right away.
    let effects = dispatch(Action::OpenConnectionPicker(models()), &mut app);
    let picker = app.connection_picker.as_ref().unwrap();
    assert!(picker.selected_row().is_some_and(|r| picker.is_active(&r)));
    assert!(
        matches!(
            effects.as_slice(),
            [Effect::WorkshopRefreshCatalogs { force: false, .. }]
        ),
        "got {effects:?}"
    );
    // `/model <text>` opens with the text already in the filter.
    app.connection_picker = None;
    dispatch(
        Action::OpenConnectionPicker(PickerFocus::Models {
            filter: "pickle".into(),
        }),
        &mut app,
    );
    assert_eq!(app.connection_picker.as_ref().unwrap().filter, "pickle");
}

/// Zero egress before the user acts: `Login` (first run's picker path, `l`, `/login`) and `/auth`
/// only load the cached rows — no live-catalog refresh, no `Authenticate`. The refresh belongs to
/// `/model` and to the picker's `r`, which fetches regardless of the cache age and re-reads the
/// engine list only from an engine that is already up.
#[test]
fn login_and_auth_never_refresh_catalogs_but_model_and_r_do() {
    use workshop_auth::{PickerFocus, PickerInput};
    let no_refresh = |effects: &[Effect]| {
        !effects
            .iter()
            .any(|e| matches!(e, Effect::WorkshopRefreshCatalogs { .. }))
    };
    let mut app = test_app_with_agent();
    app.auth_methods.clear();
    app.login_method_id = None;
    let effects = dispatch(Action::Login, &mut app);
    assert!(
        no_refresh(&effects),
        "Login fetches nothing, got {effects:?}"
    );
    assert!(!app.connection_picker.as_ref().unwrap().refresh_pending);
    // `/auth` while the picker is open: still nothing.
    let effects = dispatch(
        Action::OpenConnectionPicker(PickerFocus::Subscriptions),
        &mut app,
    );
    assert!(
        no_refresh(&effects),
        "/auth fetches nothing, got {effects:?}"
    );
    app.connection_picker = None;
    let effects = dispatch(
        Action::OpenConnectionPicker(PickerFocus::Subscriptions),
        &mut app,
    );
    assert!(
        matches!(effects.as_slice(), [Effect::WorkshopLoadPicker]),
        "a fresh /auth only loads the cached rows, got {effects:?}"
    );
    // `r` in the picker: a forced refresh (plus the reload it ends with).
    let effects = dispatch(Action::ConnectionPicker(PickerInput::Refresh), &mut app);
    assert!(
        matches!(
            effects.as_slice(),
            [Effect::WorkshopRefreshCatalogs {
                force: true,
                engine: None
            }]
        ),
        "got {effects:?}"
    );
    let picker = app.connection_picker.as_ref().unwrap();
    assert!(picker.refresh_pending && picker.loading);
    // A live snapshot clears the pending flag; a cached one does not.
    dispatch(
        Action::TaskComplete(TaskResult::WorkshopPickerLoaded(
            workshop_auth::PickerSnapshot::default(),
        )),
        &mut app,
    );
    assert!(app.connection_picker.as_ref().unwrap().refresh_pending);
    dispatch(
        Action::TaskComplete(TaskResult::WorkshopPickerLoaded(
            workshop_auth::PickerSnapshot {
                live: true,
                ..workshop_auth::PickerSnapshot::default()
            },
        )),
        &mut app,
    );
    assert!(!app.connection_picker.as_ref().unwrap().refresh_pending);
}

/// A load that leaves a signed-in rail `Loading models…` (nothing cached yet: `/auth`, after a
/// sign-in) asks the CLIs for their models, once; a `/model` refresh already queued covers it, and
/// a rail that lists its models needs nothing.
#[test]
fn a_loading_rail_after_a_load_queues_one_rail_models_refresh() {
    use workshop_auth::{PickerFocus, PickerSnapshot};
    use workshop_detect::{Pill, Rail, RailModels, RailState, SubscriptionModels};
    let rails = |state: RailModels| -> Vec<RailState> {
        Rail::ALL
            .iter()
            .map(|r| RailState {
                pill: Pill::Ready,
                installed: true,
                subscription: state.clone(),
                ..RailState::detecting(*r)
            })
            .collect()
    };
    let loaded = |state: RailModels| {
        Action::TaskComplete(TaskResult::WorkshopPickerLoaded(PickerSnapshot {
            rails: rails(state),
            ..PickerSnapshot::default()
        }))
    };
    let mut app = test_app();
    dispatch(
        Action::OpenConnectionPicker(PickerFocus::Subscriptions),
        &mut app,
    );
    let effects = dispatch(loaded(RailModels::Loading), &mut app);
    assert!(
        matches!(effects.as_slice(), [Effect::WorkshopRefreshRailModels]),
        "got {effects:?}"
    );
    let listed = RailModels::Listed {
        list: SubscriptionModels {
            rail: Rail::Claude,
            models: Vec::new(),
            account: None,
            documented_aliases: false,
            fetched_at_secs: 0,
        },
        cached: false,
        error: None,
    };
    assert!(dispatch(loaded(listed), &mut app).is_empty());
    let failed = RailModels::Failed {
        reason: "timed out".into(),
    };
    assert!(
        dispatch(loaded(failed), &mut app).is_empty(),
        "a failed rail waits for the user"
    );
    // Enter on it ("Couldn't load models — press Enter to retry") asks the CLIs again, and only
    // them: no hosted-list refresh. (`/auth` left the selection on the Claude row.)
    let effects = dispatch(
        Action::ConnectionPicker(workshop_auth::PickerInput::Enter),
        &mut app,
    );
    assert!(
        matches!(effects.as_slice(), [Effect::WorkshopRefreshRailModels]),
        "got {effects:?}"
    );

    // `/model`: the queued live refresh asks the CLIs itself; no second probe.
    app.connection_picker = None;
    dispatch(
        Action::OpenConnectionPicker(PickerFocus::Models {
            filter: String::new(),
        }),
        &mut app,
    );
    let effects = dispatch(loaded(RailModels::Loading), &mut app);
    assert!(
        matches!(effects.as_slice(), [Effect::WorkshopRefreshCatalogs { .. }]),
        "got {effects:?}"
    );
}

/// The OpenCode model could not start: Workshop falls back *silently*. The keyless pool's model is
/// activated as the shell's, the connection the user has stays what it is (the next launch tries
/// it again), the composer names the model that will answer, and the prompt goes out again once
/// the activation completes — with no notice, no "Kilo", no "fallback".
#[test]
fn engine_unavailable_falls_back_silently_and_resends_the_prompt() {
    use crate::scrollback::block::RenderBlock;
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    app.active_view = ActiveView::Agent(id);
    app.workshop_connection = crate::app::workshop::first_run_connection();
    let entry = app
        .agents
        .get_mut(&id)
        .unwrap()
        .scrollback
        .push_block(RenderBlock::user_prompt("hello"));
    app.workshop_turn_prompt_entry = Some(entry);
    app.workshop_turn_agent = Some(id);
    let system_blocks = |app: &AppView| -> Vec<String> {
        let agent = test_agent(app, id);
        (0..agent.scrollback.len())
            .filter_map(|i| agent.scrollback.entry(i))
            .filter_map(|e| match &e.block {
                RenderBlock::System(b) => Some(b.text.clone()),
                _ => None,
            })
            .collect()
    };

    let effects = dispatch(
        Action::WorkshopEngineUnavailable {
            agent_id: id,
            reason: "installer failed: offline".into(),
            text: "hello".into(),
        },
        &mut app,
    );

    assert!(
        app.workshop_connection.is_engine(),
        "the user's connection is untouched; only this process routes through the fallback"
    );
    assert_eq!(
        app.workshop_fallback.as_deref(),
        Some("Nemotron 3 Super"),
        "the composer names the answering model, plainly"
    );
    assert_eq!(app.workshop_label().as_deref(), Some("Nemotron 3 Super"));
    assert!(
        test_agent(&app, id).scrollback.index_of_id(entry).is_none(),
        "the first attempt's bubble is dropped; the resend paints it once"
    );
    assert!(
        matches!(
            effects.as_slice(),
            [Effect::WorkshopActivateModel { session: Some((sid, _)), model_id, .. }]
                if *sid == id && model_id.starts_with("kilo")
        ),
        "switches the open session to the fallback model, got {effects:?}"
    );
    assert_eq!(
        app.workshop_resend.as_ref().map(|(_, t)| t.as_str()),
        Some("hello")
    );
    assert!(
        system_blocks(&app).is_empty(),
        "nothing is said about the switch: {:?}",
        system_blocks(&app)
    );
    let AuthState::Authenticating { request_seq, .. } = app.auth_state else {
        panic!("activation in flight, got {:?}", app.auth_state);
    };

    dispatch(
        Action::TaskComplete(TaskResult::AuthComplete {
            request_seq,
            meta: None,
        }),
        &mut app,
    );
    assert!(app.workshop_resend.is_none(), "the resend was consumed");
    let said = system_blocks(&app).join("\n").to_ascii_lowercase();
    for word in ["kilo", "fallback", "engine", "unavailable", "opencode"] {
        assert!(
            !said.contains(word),
            "no plumbing on screen ({word:?}): {said}"
        );
    }
    assert_eq!(app.active_view, ActiveView::Agent(id));
    assert!(
        test_agent(&app, id).workshop_model_label.as_deref() == Some("Nemotron 3 Super"),
        "the composer label is the answering model: {:?}",
        test_agent(&app, id).workshop_model_label
    );
}

/// Enter on a non-xAI row (a model, a vendor, `API keys`, a connect row) opens a sub-menu or
/// setup details and never emits `Authenticate`.
#[test]
fn picker_non_xai_cards_never_authenticate() {
    use workshop_auth::{PickerInput, XAI_ROW_ID};
    let mut app = test_app();
    dispatch(Action::Login, &mut app);
    let n = app.connection_picker.as_ref().unwrap().visible_rows().len();
    for i in 0..n {
        let picker = app.connection_picker.as_mut().unwrap();
        picker.submenu = None;
        picker.selected = i;
        let id = picker.selected_row().map(|r| r.id()).unwrap_or_default();
        if id == XAI_ROW_ID {
            continue;
        }
        let effects = dispatch(Action::ConnectionPicker(PickerInput::Enter), &mut app);
        assert!(
            !effects
                .iter()
                .any(|e| matches!(e, Effect::Authenticate { .. })),
            "{id}: Enter must not authenticate"
        );
        // Picking a model activates it in-process (`WorkshopActivateModel`, the anonymous
        // session); that is the only thing allowed to leave `Authenticating` behind.
        if matches!(app.auth_state, AuthState::Authenticating { .. }) {
            assert!(
                effects
                    .iter()
                    .any(|e| matches!(e, Effect::WorkshopActivateModel { .. })),
                "{id}: only a model activation may be in flight, got {effects:?}"
            );
            app.auth_state = AuthState::Done;
        }
        if app.connection_picker.is_none() {
            dispatch(Action::Login, &mut app);
        }
    }
}

/// The optional xAI card is the only path to the inherited flow, and it needs two explicit Enters.
#[test]
fn picker_xai_card_requires_two_enters_and_sets_opt_in() {
    let mut app = test_app();
    let effects = start_login_flow(&mut app);
    assert!(
        matches!(app.auth_state, AuthState::Authenticating { .. }),
        "second Enter on the xAI card starts the flow"
    );
    let auth = effects
        .iter()
        .find(|e| matches!(e, Effect::Authenticate { .. }))
        .expect("Authenticate effect");
    if let Effect::Authenticate {
        xai_opt_in,
        method_id,
        force_interactive,
        ..
    } = auth
    {
        assert!(*xai_opt_in, "xAI card must carry the explicit opt-in");
        assert!(*force_interactive);
        assert_eq!(method_id.0.as_ref(), "grok.com");
    }
    assert!(
        app.connection_picker.is_none(),
        "picker closes when the flow starts"
    );
}

/// Picker opened by `/auth` with every vendor installed and signed out (Connect shown), Claude
/// selected.
fn picker_with_signed_out_rails(app: &mut AppView) {
    use workshop_auth::PickerSnapshot;
    use workshop_detect::{Pill, Rail, RailState};
    dispatch(Action::Login, app);
    let picker = app.connection_picker.as_mut().unwrap();
    let mut rows = picker.rows.clone();
    rows.extend(picker.connect_rows.clone());
    picker.apply_snapshot(PickerSnapshot {
        rows,
        rails: Rail::ALL
            .iter()
            .map(|r| RailState {
                pill: Pill::SignIn,
                installed: true,
                show_connect: true,
                ..RailState::detecting(*r)
            })
            .collect(),
        ..PickerSnapshot::default()
    });
    let picker = app.connection_picker.as_ref().unwrap();
    assert_eq!(picker.title(), "Models");
    assert_eq!(
        picker.selected_row().map(|r| r.title()),
        Some("Claude".into()),
        "/auth lands on the Claude row"
    );
}

/// The state Connect's Enter leaves behind (`PickerOutcome::RailConnect`): the vendor login is
/// queued for the event loop. Set directly so the test never probes this machine's PATH for a
/// real CLI (`rail_login_argv` does).
fn queue_rail_connect(app: &mut AppView) -> workshop_detect::Rail {
    let picker = app.connection_picker.as_ref().unwrap();
    let Some(workshop_auth::RowKind::Vendor(rail)) = picker.selected_row().map(|r| r.kind) else {
        panic!("a vendor row is selected");
    };
    app.pending_workshop_login = Some((rail, vec!["claude".into(), "auth".into(), "login".into()]));
    rail
}

fn selected_title(app: &AppView) -> Option<String> {
    app.connection_picker
        .as_ref()
        .and_then(|p| p.selected_row())
        .map(|r| r.title())
}

/// After the vendor login returns, ↑/↓ move between the vendor rows again and the picker
/// re-probes them.
#[test]
fn picker_rail_login_done_restores_rail_navigation() {
    use workshop_auth::PickerInput;
    use workshop_detect::process::InteractiveExit;
    let mut app = test_app();
    picker_with_signed_out_rails(&mut app);
    let rail = queue_rail_connect(&mut app);
    app.pending_workshop_login.take();

    let effects = dispatch(
        Action::TaskComplete(TaskResult::WorkshopLoginTerminalDone {
            rail,
            exit: InteractiveExit::Success,
        }),
        &mut app,
    );
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::WorkshopLoadPicker)),
        "a finished login re-probes the rails, got {effects:?}"
    );
    assert_eq!(selected_title(&app).as_deref(), Some("Claude"));
    dispatch(Action::ConnectionPicker(PickerInput::Down), &mut app);
    assert_eq!(
        selected_title(&app).as_deref(),
        Some("Codex"),
        "Down moves to the next vendor right after the login returns"
    );
}

/// Ctrl+C in the terminal ends the vendor login only: the picker reports the cancellation, keeps
/// the vendor rows as they were (no re-probe) and is navigable again.
#[test]
fn picker_rail_login_interrupted_reports_cancel_without_reprobe() {
    use workshop_auth::PickerInput;
    use workshop_detect::process::InteractiveExit;
    let mut app = test_app();
    picker_with_signed_out_rails(&mut app);
    let rail = queue_rail_connect(&mut app);
    app.pending_workshop_login.take();

    let effects = dispatch(
        Action::TaskComplete(TaskResult::WorkshopLoginTerminalDone {
            rail,
            exit: InteractiveExit::Interrupted,
        }),
        &mut app,
    );
    assert!(
        effects.is_empty(),
        "a cancelled login is not re-probed, got {effects:?}"
    );
    let picker = app.connection_picker.as_ref().unwrap();
    assert!(
        picker
            .status
            .as_deref()
            .is_some_and(|s| s.contains("sign-in cancelled")),
        "status: {:?}",
        picker.status
    );
    assert!(!picker.loading);
    dispatch(Action::ConnectionPicker(PickerInput::Down), &mut app);
    assert_eq!(selected_title(&app).as_deref(), Some("Codex"));
}

/// Puts the app in `Authenticating` with a live task's abort handle installed, as the event loop would.
/// Returns the task's JoinHandle and the seq.
/// Callers assert the task actually gets aborted (`unwrap_err().is_cancelled()`), not merely that the handle slot was cleared.
fn install_live_auth_task(
    app: &mut AppView,
    rt: &tokio::runtime::Runtime,
) -> (tokio::task::JoinHandle<()>, u64) {
    start_login_flow(app);
    let task = rt.spawn(std::future::pending::<()>());
    match &mut app.auth_state {
        AuthState::Authenticating {
            handle,
            request_seq,
            ..
        } => {
            *handle = Some(task.abort_handle());
            (task, *request_seq)
        }
        other => panic!("expected Authenticating after Login, got {other:?}"),
    }
}

fn test_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime")
}

/// A second `/login` while already authenticating must abort the prior auth task and bump the seq.
/// Single-flight: never two device-code requests running at once.
#[test]
fn login_while_authenticating_aborts_prior_task() {
    let rt = test_runtime();
    let mut app = test_app_with_agent();
    let (prior_task, first_seq) = install_live_auth_task(&mut app, &rt);

    let effects = start_login_flow(&mut app);

    rt.block_on(async {
        assert!(
            prior_task.await.unwrap_err().is_cancelled(),
            "prior auth task must be aborted"
        );
    });
    match &app.auth_state {
        AuthState::Authenticating { request_seq, .. } => {
            assert!(
                *request_seq > first_seq,
                "re-login must bump request_seq for single-flight"
            );
        }
        other => panic!("expected Authenticating after re-Login, got {other:?}"),
    }
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::Authenticate { .. })),
        "re-login must emit a new Authenticate"
    );
}

/// A stale `AuthComplete` (its abort lost the race because the task had already finished) must not complete the new attempt.
/// The request-seq guard is the only protection here.
#[test]
fn stale_auth_complete_after_relogin_is_ignored() {
    let mut app = test_app_with_agent();
    start_login_flow(&mut app);
    let first_seq = match &app.auth_state {
        AuthState::Authenticating { request_seq, .. } => *request_seq,
        other => panic!("expected Authenticating after Login, got {other:?}"),
    };
    start_login_flow(&mut app); // re-login bumps to seq2

    dispatch(
        Action::TaskComplete(TaskResult::AuthComplete {
            request_seq: first_seq,
            meta: None,
        }),
        &mut app,
    );

    match &app.auth_state {
        AuthState::Authenticating { request_seq, .. } => {
            assert!(
                *request_seq > first_seq,
                "stale AuthComplete must leave the new attempt authenticating"
            );
        }
        other => panic!("stale AuthComplete must be ignored, got {other:?}"),
    }
}

/// Switch-account while authenticating goes through the same single-flight abort as `/login` (sibling entry point).
#[test]
fn switch_account_while_authenticating_aborts_prior_task() {
    let rt = test_runtime();
    let mut app = test_app_with_agent();
    let (prior_task, first_seq) = install_live_auth_task(&mut app, &rt);

    dispatch(Action::SwitchAccount, &mut app);

    rt.block_on(async {
        assert!(
            prior_task.await.unwrap_err().is_cancelled(),
            "prior auth task must be aborted on switch-account"
        );
    });
    match &app.auth_state {
        AuthState::Authenticating { request_seq, .. } => {
            assert!(*request_seq > first_seq, "switch must bump request_seq");
        }
        other => panic!("expected Authenticating after SwitchAccount, got {other:?}"),
    }
}

/// Cancelling a mid-session login aborts the in-flight auth task (not just restores the view) so a retry cannot race a still-polling prior attempt.
#[test]
fn cancel_login_aborts_prior_task() {
    let rt = test_runtime();
    let mut app = test_app_with_agent();
    // Login from a session view stashes `auth_return_view`, making CancelLogin live.
    let (prior_task, _) = install_live_auth_task(&mut app, &rt);

    dispatch(Action::CancelLogin, &mut app);

    rt.block_on(async {
        assert!(
            prior_task.await.unwrap_err().is_cancelled(),
            "cancel must abort the in-flight auth task"
        );
    });
}

/// Cancelling a mid-session login returns to the session rather than quitting the app, and clears the stashed view and auth state.
#[test]
fn cancel_login_restores_view() {
    let mut app = test_app_with_agent();
    start_login_flow(&mut app);
    assert_eq!(app.active_view, ActiveView::Welcome);
    let prior_seq = match &app.auth_state {
        AuthState::Authenticating { request_seq, .. } => *request_seq,
        other => panic!("expected Authenticating after Login, got {other:?}"),
    };

    let effects = dispatch(Action::CancelLogin, &mut app);

    assert!(
        matches!(
            effects.as_slice(),
            [Effect::CancelAuth { request_seq }] if *request_seq == prior_seq
        ),
        "cancel must tell the shell to stop the in-flight auth poll for this attempt"
    );
    assert_eq!(app.active_view, ActiveView::Agent(AgentId(0)));
    assert_eq!(app.auth_return_view, None);
    assert!(matches!(app.auth_state, AuthState::Done));
}

/// `CancelLogin` outside a mid-session login is a no-op (must not move off the welcome screen or panic).
#[test]
fn cancel_login_noop_without_stashed_view() {
    let mut app = test_app();
    let effects = dispatch(Action::CancelLogin, &mut app);
    assert!(effects.is_empty());
    assert_eq!(app.active_view, ActiveView::Welcome);
    assert_eq!(app.auth_return_view, None);
}

#[test]
fn auth_complete_extracts_show_resolved_model_from_meta() {
    let mut app = test_app();
    app.auth_state = AuthState::Authenticating {
        request_seq: 1,
        handle: None,
        auth_url: None,
        mode: AuthMode::Pending,
    };
    assert!(app.show_resolved_model);

    dispatch(
        Action::TaskComplete(TaskResult::AuthComplete {
            request_seq: 1,
            meta: Some(serde_json::json!({ "show_resolved_model": false })),
        }),
        &mut app,
    );

    assert!(!app.show_resolved_model);
}

#[test]
fn auth_complete_preserves_show_resolved_model_when_absent() {
    let mut app = test_app();
    app.show_resolved_model = false;
    app.auth_state = AuthState::Authenticating {
        request_seq: 1,
        handle: None,
        auth_url: None,
        mode: AuthMode::Pending,
    };

    dispatch(
        Action::TaskComplete(TaskResult::AuthComplete {
            request_seq: 1,
            meta: Some(serde_json::to_value(xai_grok_login::AuthMeta::default()).unwrap()),
        }),
        &mut app,
    );

    assert!(!app.show_resolved_model);
}
