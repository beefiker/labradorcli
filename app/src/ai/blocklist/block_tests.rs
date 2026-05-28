use super::{CollapsibleElementState, CollapsibleExpansionState};
use crate::ai::agent::LocalCLIToolOutput;
use crate::settings::AISettings;
use crate::test_util::settings::initialize_settings_for_tests;
use settings::Setting;
use labrador_ui::{App, SingletonEntity};

#[test]
fn reasoning_auto_collapses_when_user_has_not_manually_toggled() {
    App::test((), |mut app| async move {
        initialize_settings_for_tests(&mut app);
        let mut state = CollapsibleElementState::default();
        app.update(|ctx| {
            state.finish_reasoning(ctx);
        });

        assert!(matches!(
            state.expansion_state,
            CollapsibleExpansionState::Collapsed
        ));
    });
}

#[test]
fn always_show_thinking_stays_expanded_after_finish() {
    App::test((), |mut app| async move {
        initialize_settings_for_tests(&mut app);
        AISettings::handle(&app).update(&mut app, |settings, ctx| {
            settings
                .thinking_display_mode
                .set_value(crate::settings::ThinkingDisplayMode::AlwaysShow, ctx)
                .unwrap();
        });

        let mut state = CollapsibleElementState::default();
        app.update(|ctx| {
            state.finish_reasoning(ctx);
        });

        assert!(matches!(
            state.expansion_state,
            CollapsibleExpansionState::Expanded {
                is_finished: true,
                scroll_pinned_to_bottom: false
            }
        ));
    });
}

#[test]
fn manual_collapse_while_streaming_stays_collapsed_after_finish() {
    App::test((), |mut app| async move {
        initialize_settings_for_tests(&mut app);
        let mut state = CollapsibleElementState::default();

        state.toggle_expansion();
        app.update(|ctx| {
            state.finish_reasoning(ctx);
        });

        assert!(matches!(
            state.expansion_state,
            CollapsibleExpansionState::Collapsed
        ));
    });
}

#[test]
fn manual_reexpand_while_streaming_stays_expanded_after_finish() {
    App::test((), |mut app| async move {
        initialize_settings_for_tests(&mut app);
        let mut state = CollapsibleElementState::default();

        state.toggle_expansion();
        state.toggle_expansion();
        app.update(|ctx| {
            state.finish_reasoning(ctx);
        });

        assert!(matches!(
            state.expansion_state,
            CollapsibleExpansionState::Expanded {
                is_finished: true,
                scroll_pinned_to_bottom: false
            }
        ));
    });
}

#[test]
fn local_cli_tool_output_expands_when_result_body_arrives() {
    let mut state = CollapsibleElementState {
        expansion_state: CollapsibleExpansionState::Collapsed,
        ..Default::default()
    };

    state.sync_local_cli_tool_output(&LocalCLIToolOutput {
        title: "Running pwd".to_string(),
        body: String::new(),
        is_complete: false,
        is_error: false,
    });

    assert!(matches!(
        state.expansion_state,
        CollapsibleExpansionState::Collapsed
    ));

    state.sync_local_cli_tool_output(&LocalCLIToolOutput {
        title: "Ran pwd".to_string(),
        body: "/tmp".to_string(),
        is_complete: true,
        is_error: false,
    });

    assert!(matches!(
        state.expansion_state,
        CollapsibleExpansionState::Expanded {
            is_finished: true,
            scroll_pinned_to_bottom: false
        }
    ));
}

#[test]
fn local_cli_tool_output_preserves_manual_collapse_after_finish() {
    let output = LocalCLIToolOutput {
        title: "Ran pwd".to_string(),
        body: "/tmp".to_string(),
        is_complete: true,
        is_error: false,
    };
    let mut state = CollapsibleElementState {
        expansion_state: CollapsibleExpansionState::Collapsed,
        ..Default::default()
    };

    state.sync_local_cli_tool_output(&output);
    state.toggle_expansion();
    state.sync_local_cli_tool_output(&output);

    assert!(matches!(
        state.expansion_state,
        CollapsibleExpansionState::Collapsed
    ));
}
