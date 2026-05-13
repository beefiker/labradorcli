use crate::ai::agent::api::ServerConversationToken;
use crate::ai::blocklist::SerializedBlockListItem;
use crate::appearance::Appearance;
use crate::auth::auth_manager::{AuthManager, AuthManagerEvent};
use crate::auth::auth_state::AuthState;
use crate::auth::AuthStateProvider;
use crate::autoupdate::{AutoupdateState, AutoupdateStateEvent};
use crate::experiments::{BlockOnboarding, Experiment};
use crate::interval_timer::IntervalTimer;
use crate::launch_configs::launch_config;
use crate::linear::LinearIssueWork;
use crate::settings::apply_onboarding_settings;
use crate::settings::AISettings;
use crate::workspace::tab_settings::TabSettings;
use onboarding::{
    AgentOnboardingEvent, AgentOnboardingView, OnboardingIntention, SelectedSettings,
};

use crate::features::FeatureFlag;
use crate::persistence::ModelEvent;
use crate::report_if_error;
use crate::server::server_api::auth::UserAuthenticationError;
use crate::server::server_api::ServerApiProvider;
use crate::server::telemetry::LaunchConfigUiLocation;
use crate::settings::QuakeModeSettings;
use crate::settings::ThemeSettings;
use crate::settings_view::flags;
use crate::settings_view::mcp_servers_page::MCPServersSettingsPage;
use crate::settings_view::SettingsSection;
use crate::terminal::available_shells::AvailableShell;
use crate::terminal::general_settings::GeneralSettings;
use crate::terminal::keys_settings::KeysSettings;
use crate::terminal::shell::ShellType;
use crate::terminal::view::cell_size_and_padding;
use crate::themes::onboarding_theme_picker_themes;
use crate::themes::theme::{AnsiColorIdentifier, Blend, Fill, ThemeKind, WarpThemeConfig};
use crate::uri::OpenMCPSettingsArgs;
use crate::util::bindings::{self, is_binding_pty_compliant};
use crate::util::traffic_lights::{traffic_light_data, TrafficLightData, TrafficLightMouseStates};
use crate::view_components::DismissibleToast;
use crate::window_settings::WindowSettings;
use crate::workspace::WorkspaceAction;
use crate::workspaces::team_tester::TeamTesterStatus;
use crate::workspaces::update_manager::TeamUpdateManager;
use crate::workspaces::user_workspaces::{UserWorkspaces, UserWorkspacesEvent};
use crate::{
    app_state::{AppState, PaneUuid, WindowSnapshot},
    autoupdate::{RequestType, UpdateReady},
    pane_group::{NewTerminalOptions, PanesLayout},
        server::{server_api::ServerTime, telemetry::TelemetryEvent},
    UpdateQuakeModeEventArg,
};
use crate::{
    channel::{Channel, ChannelState},
    server::server_api::ServerApi,
    workspace::{view::OnboardingTutorial, PaneViewLocator, Workspace},
};
use crate::{GlobalResourceHandles, GlobalResourceHandlesProvider};
use anyhow::Result;
use cfg_if::cfg_if;
use itertools::Itertools;
use lazy_static::lazy_static;
use parking_lot::Mutex;
use pathfinder_color::ColorU;
use pathfinder_geometry::rect::RectF;
use pathfinder_geometry::vector::{vec2f, Vector2F};
use serde::{Deserialize, Serialize};
use session_sharing_protocol::common::SessionId;
use settings::Setting as _;
use std::path::Path;
use std::sync::mpsc::SyncSender;
use std::sync::Arc;
use std::time::Duration;
use std::{collections::HashMap, path::PathBuf};
use url::Url;
use warp_core::context_flag::ContextFlag;
use warp_core::user_preferences::GetUserPreferences as _;
use warpui::assets::asset_cache::AssetSource;
use warpui::clipboard::ClipboardContent;
use warpui::fonts::Weight;
use warpui::keymap::{EditableBinding, FixedBinding, Keystroke};
use warpui::text_layout::TextAlignment;
use warpui::windowing::WindowManager;

use crate::ai::llms::{LLMPreferences, LLMPreferencesEvent};
use crate::ai::onboarding::{
    apply_free_tier_default_model_override, build_onboarding_models, current_onboarding_auth_state,
};

use ui_components::{button, Component as _, Options as _};
use warpui::elements::{
    Align, Border, CacheOption, ChildAnchor, ConstrainedBox, Container, CornerRadius,
    CrossAxisAlignment, DispatchEventResult, EventHandler, Fill as ElementFill, Flex,
    FormattedTextElement, Hoverable, Image, MainAxisAlignment, MainAxisSize, MouseStateHandle,
    OffsetPositioning, ParentAnchor, ParentElement, ParentOffsetBounds, Point, Radius, Stack,
};
use warpui::rendering::OnGPUDeviceSelected;
use warpui::{id, AddWindowOptions, DisplayId, SingletonEntity};
use warpui::{
    platform::{Cursor, WindowBounds, WindowStyle},
    presenter::ChildView,
    AfterLayoutContext, AppContext, Element, Entity, EntityId, EventContext, LayoutContext,
    PaintContext, SizeConstraint, TypedActionView, View, ViewContext, ViewHandle, WindowId,
};
use warpui::{FocusContext, NextNewWindowsHasThisWindowsBoundsUponClose};

const WINDOW_TITLE: &str = "Dwarf";

lazy_static! {
    static ref FALLBACK_WINDOW_SIZE: Vector2F = vec2f(800.0, 600.0);
    static ref QUAKE_STATE: Arc<Mutex<Option<QuakeModeState>>> = Arc::new(Mutex::new(None));
}

/// This is the color of the border wrapping the whole window.
///
/// On MacOS, this is drawn for us by the OS. On other platforms, we must draw it ourselves. Note
/// that this is hard-coded for the default Dark theme. This is because it is only used by the
/// AuthView and OnboardingSurveyModal which do not respect the chosen theme. So, do not use this for Views
/// which respect themes.
pub(crate) fn unthemed_window_border() -> Border {
    if cfg!(all(not(target_os = "macos"), not(target_family = "wasm"))) {
        // The 15% blend of fg into bg is the "ui surface" color.
        Border::all(1.).with_border_fill(Fill::black().blend(&Fill::white().with_opacity(15)))
    } else {
        Border::all(1.).with_border_fill(Fill::black().with_opacity(0))
    }
}

#[derive(Debug, Clone)]
enum WindowState {
    /// Quake mode window is open and visible on the screen.
    Open,
    /// Quake mode window is opening but has not become the key window yet.
    /// This happens when the app is not focused when the quake mode window
    /// is opened.
    PendingOpen,
    /// Quake mode window is open but hidden away from the screen.
    /// In this state, toggling quake mode will show the hidden window rather
    /// than creating a new one.
    Hidden,
}

#[derive(Debug, Clone)]
pub struct QuakeModeState {
    /// State of the opened quake mode window.
    window_state: WindowState,
    window_id: WindowId,
    /// ID of the active screen when we last positioned the quake mode window.
    /// Note that this is not necessarily the screen quake mode lives in if user
    /// set a specific pinned screen.
    active_display_id: DisplayId,
}

/// Configuration for the new quake mode window including the active screen id and the window bound.
struct QuakeModeFrameConfig {
    display_id: DisplayId,
    window_bounds: RectF,
}

/// Trigger of a potential quake window move.
#[derive(Debug)]
enum QuakeModeMoveTrigger {
    /// The screen configuration changed (plug / unplug monitor). We need
    /// to reposition quake mode as it might be in an invalid position.
    ScreenConfigurationChange,
    /// User set "active screen" as the screen to pin to. In this case,
    /// we will attempt to move the quake window if the active screen dimension
    /// changed. If it hasn't change, we will keep the window as is to avoid
    /// meaningless resizing.
    ActiveScreenSetting,
}

#[derive(
    Debug,
    Clone,
    Copy,
    Hash,
    Eq,
    PartialEq,
    Deserialize,
    Serialize,
    Default,
    schemars::JsonSchema,
    settings_value::SettingsValue,
)]
#[schemars(
    description = "Screen edge to pin the hotkey window to.",
    rename_all = "snake_case"
)]
pub enum QuakeModePinPosition {
    #[default]
    Top,
    Bottom,
    Left,
    Right,
}

pub struct OpenFromRestoredArg {
    pub app_state: Option<AppState>,
}

pub struct OpenLaunchConfigArg {
    pub launch_config: launch_config::LaunchConfig,
    pub ui_location: LaunchConfigUiLocation,

    /// Tries to open the launch config into the active window, if any.
    ///
    /// Currently, this is only supported by single-window launch configs
    /// and will open the window tabs into the existing window when true.
    pub open_in_active_window: bool,
}

pub struct OpenPath {
    pub path: PathBuf,
}

// Arguments for actions that run a command that should start a subshell.
pub struct SubshellCommandArg {
    pub command: String,
    pub shell_type: Option<ShellType>,
}

// Arguments for creating an ambient agent environment.
pub struct CreateEnvironmentArg {
    pub repos: Vec<String>,
}

/// Arguments for the immediate tab detach action dispatched during drag.
/// This contains minimal info needed to identify which tab to detach.
pub struct DetachTabImmediateArg {
    /// Index of the tab to detach
    pub tab_index: usize,
    /// Pre-calculated window position for the new window (in screen coordinates).
    /// This is calculated to position the window so the mouse is in the tab bar region.
    pub window_position: Option<Vector2F>,
    /// Source window ID - the window containing the tab to detach.
    /// We need this because the active window might be the preview window.
    pub source_window_id: WindowId,
}

/// Pre-gathered information for creating a transferred window.
/// This is used when the caller already has access to the workspace (e.g., from within a view method)
/// and cannot rely on workspace lookup (which fails during view updates).
pub struct TabTransferInfo {
    pub transferred_tab: crate::workspace::view::TransferredTab,
    pub window_size: Vector2F,
    pub window_position: Vector2F,
    pub source_window_id: WindowId,
}

impl CreateEnvironmentArg {
    /// Formats the `/create-environment` slash command invocation.
    pub fn to_query(&self) -> String {
        // Filter repos to accept either valid URLs or POSIX portable pathnames for security.
        //
        // Note: we also allow *absolute* POSIX paths (e.g., /Users/me/repo) as long as every
        // component is portable. This is important for local indexed repos.
        let safe_repos = self
            .repos
            .iter()
            .filter(|repo| {
                // Accept valid URLs (e.g., https://github.com/user/repo)
                Url::parse(repo).is_ok()
                    // Or valid POSIX portable pathnames (e.g., user/repo)
                    || warp_util::path::is_posix_portable_pathname(repo)
                    // Or absolute POSIX paths with portable components (e.g., /Users/me/repo)
                    || repo
                        .strip_prefix('/')
                        .is_some_and(warp_util::path::is_posix_portable_pathname)
            })
            .join(" ");

        if safe_repos.is_empty() {
            // Include a trailing space to trigger slash command syntax highlighting and ghost text.
            "/create-environment ".to_string()
        } else {
            format!("/create-environment {}", safe_repos)
        }
    }
}

pub fn init(app: &mut AppContext) {
    app.register_binding_validator::<RootView>(is_binding_pty_compliant);

    app.add_global_action("root_view:open_from_restored", open_from_restored);
    app.add_global_action("root_view:open_new", open_new);
    app.add_global_action("root_view:open_new_with_shell", open_new_with_shell);
    app.add_global_action("root_view:open_new_from_path", |arg, ctx| {
        let _ = open_new_from_path(arg, ctx);
    });
    app.add_global_action(
        "root_view:open_new_tab_insert_subshell_command_and_bootstrap_if_supported",
        open_new_tab_insert_subshell_command_and_bootstrap_if_supported,
    );
    app.add_global_action("root_view:open_launch_config", open_launch_config);
    app.add_global_action("root_view:send_feedback", send_feedback);
    app.add_global_action("root_view:detach_tab_immediate", |arg, ctx| {
        let _ = detach_tab_with_transfer(arg, ctx);
    });
    app.add_global_action(
        "root_view:toggle_quake_mode_window",
        toggle_quake_mode_window,
    );
    app.add_global_action(
        "root_view:show_or_hide_non_quake_mode_windows",
        show_or_hide_non_quake_mode_windows,
    );
    app.add_global_action("root_view:update_quake_mode_state", update_quake_mode_state);
    app.add_global_action(
        "root_view:move_quake_mode_window_from_screen_change",
        move_quake_mode_window_from_screen_change,
    );
    #[cfg(feature = "voice_input")]
    app.add_global_action("root_view:abort_voice_input", abort_voice_input);
    #[cfg(feature = "voice_input")]
    app.add_action(
        "root_view:maybe_stop_active_voice_input",
        RootView::maybe_stop_active_voice_input,
    );
    app.add_action("root_view:log_out", RootView::log_out);
    app.add_action(
        "root_view:add_session_at_path",
        RootView::add_session_at_path,
    );
    app.add_action(
        "root_view:handle_team_intent_link_action",
        RootView::handle_team_intent_link_action,
    );
    app.add_action(
        "root_view:handle_notification_click",
        RootView::handle_notification_click,
    );
    app.add_action(
        "root_view:handle_pane_navigation_event",
        RootView::focus_pane,
    );
    app.add_action("root_view:close_window", RootView::close_window);
    app.add_action("root_view:minimize_window", RootView::minimize_window);
    app.add_action(
        "root_view:toggle_maximize_window",
        RootView::toggle_maximize_window,
    );
    app.add_action("root_view:toggle_fullscreen", RootView::toggle_fullscreen);

    if FeatureFlag::ViewingSharedSessions.is_enabled() {
        app.add_global_action(
            "root_view:join_shared_session",
            open_shared_session_as_viewer,
        );
        app.add_action(
            "root_view:join_shared_session_in_existing_window",
            RootView::join_shared_session_in_existing_window,
        );
    }

    app.add_global_action(
        "root_view:open_conversation_viewer",
        open_conversation_viewer,
    );
    app.add_action(
        "root_view:open_cloud_conversation_in_existing_window",
        RootView::open_cloud_conversation_in_existing_window,
    );

    app.add_global_action("root_view:create_environment", create_environment);
    app.add_global_action(
        "root_view:create_environment_and_run",
        create_environment_and_run,
    );
    app.add_action(
        "root_view:create_environment_in_existing_window",
        RootView::create_environment_in_existing_window,
    );
    app.add_action(
        "root_view:create_environment_in_existing_window_and_run",
        RootView::create_environment_in_existing_window_and_run,
    );
    app.add_global_action(
        "root_view:open_settings_page_in_new_window",
        open_settings_page_in_new_window,
    );
    app.add_action(
        "root_view:open_settings_page_in_existing_window",
        RootView::open_settings_page_in_existing_window,
    );

    app.add_global_action(
        "root_view:open_mcp_settings_in_new_window",
        open_mcp_settings_in_new_window,
    );
    app.add_action(
        "root_view:open_mcp_settings_in_existing_window",
        RootView::open_mcp_settings_in_existing_window,
    );

    app.add_global_action(
        "root_view:open_codex_in_new_window",
        open_codex_in_new_window,
    );
    app.add_action(
        "root_view:open_codex_in_existing_window",
        RootView::open_codex_in_existing_window,
    );

    app.add_global_action(
        "root_view:open_linear_issue_work_in_new_window",
        open_linear_issue_work_in_new_window,
    );
    app.add_action(
        "root_view:open_linear_issue_work_in_existing_window",
        RootView::open_linear_issue_work_in_existing_window,
    );

    app.add_action("root_view:add_file_pane", RootView::add_file_pane);

    app.register_fixed_bindings([
        FixedBinding::empty(
            "Hide All Windows",
            RootViewAction::ShowOrHideNonQuakeModeWindows,
            id!("RootView") & id!(flags::ACTIVATION_HOTKEY_FLAG),
        ),
        FixedBinding::empty(
            "Show Dedicated Hotkey Window",
            RootViewAction::ToggleQuakeModeWindow,
            id!("RootView")
                & id!(flags::QUAKE_MODE_ENABLED_CONTEXT_FLAG)
                & !id!(flags::QUAKE_WINDOW_OPEN_FLAG),
        ),
        FixedBinding::empty(
            "Hide Dedicated Hotkey Window",
            RootViewAction::ToggleQuakeModeWindow,
            id!("RootView")
                & id!(flags::QUAKE_MODE_ENABLED_CONTEXT_FLAG)
                & id!(flags::QUAKE_WINDOW_OPEN_FLAG),
        ),
    ]);

    app.register_editable_bindings([
        // Register a binding to toggle fullscreen on Linux and Windows.
        EditableBinding::new(
            "root_view:toggle_fullscreen",
            "Toggle fullscreen",
            RootViewAction::ToggleFullscreen,
        )
        .with_group(bindings::BindingGroup::Navigation.as_str())
        .with_context_predicate(id!("RootView"))
        .with_linux_or_windows_key_binding("f11"),
        // Debug binding for onboarding state
        EditableBinding::new(
            "root_view:enter_onboarding_state",
            "[Debug] Enter Onboarding State",
            RootViewAction::DebugEnterOnboardingState,
        )
        .with_group(bindings::BindingGroup::Settings.as_str())
        .with_context_predicate(id!("RootView"))
        .with_key_binding("shift-f12")
        .with_enabled(|| {
            FeatureFlag::AgentOnboarding.is_enabled() && ChannelState::enable_debug_features()
        }),
    ])
}

fn maybe_register_global_window_shortcuts(
    global_resource_handles: GlobalResourceHandles,
    ctx: &mut AppContext,
) {
    // let keys_settings = KeysSettings::handle(ctx).as_ref(ctx);
    if let Some(key) = KeysSettings::as_ref(ctx)
        .quake_mode_settings
        .keybinding
        .clone()
        .filter(|_| *KeysSettings::as_ref(ctx).quake_mode_enabled)
    {
        ctx.register_global_shortcut(
            key.clone(),
            "root_view:toggle_quake_mode_window",
            global_resource_handles,
        );
    }

    if let Some(key) = KeysSettings::as_ref(ctx)
        .activation_hotkey_keybinding
        .clone()
        .filter(|_| *KeysSettings::as_ref(ctx).activation_hotkey_enabled)
    {
        ctx.register_global_shortcut(
            key.clone(),
            "root_view:show_or_hide_non_quake_mode_windows",
            (),
        )
    }
}

/// Find the root [`Workspace`] view for the active window.
fn active_workspace(ctx: &mut AppContext) -> Option<ViewHandle<Workspace>> {
    let window_id = ctx.windows().active_window()?;
    ctx.views_of_type::<Workspace>(window_id)
        .and_then(|views| views.first().cloned())
}

/// Find the root [`Workspace`] view for a specific window.
pub fn workspace_for_window(
    window_id: WindowId,
    ctx: &mut AppContext,
) -> Option<ViewHandle<Workspace>> {
    ctx.views_of_type::<Workspace>(window_id)
        .and_then(|views| views.first().cloned())
}

fn open_launch_config(arg: &OpenLaunchConfigArg, ctx: &mut AppContext) {
    let active_window_workspace = active_workspace(ctx);
    if arg.launch_config.windows.is_empty() {
        open_new(&(), ctx);
    } else if arg.open_in_active_window
        && arg.launch_config.windows.len() == 1
        && active_window_workspace.is_some()
    {
        active_window_workspace
            .expect("already checked if there is a workspace for the active window")
            .update(ctx, |workspace, ctx| {
                workspace.open_launch_config_window(arg.launch_config.windows[0].clone(), ctx)
            });
    } else {
        let mut active_index = None;
        for (idx, window_template) in arg.launch_config.windows.iter().enumerate() {
            if arg
                .launch_config
                .active_window_index
                .map(|window_idx| window_idx == idx)
                .unwrap_or(false)
            {
                active_index = Some(idx);
            } else {
                open_new_with_workspace_source(
                    NewWorkspaceSource::FromTemplate {
                        window_template: window_template.clone(),
                    },
                    ctx,
                );
            }
        }

        if let Some(idx) = active_index {
            let window_template = arg
                .launch_config
                .windows
                .get(idx)
                .expect("Window should exist at idx");

            open_new_with_workspace_source(
                NewWorkspaceSource::FromTemplate {
                    window_template: window_template.clone(),
                },
                ctx,
            );
        }
    }

}

fn send_feedback(_: &(), ctx: &mut AppContext) {
    if let Some(workspace) = active_workspace(ctx) {
        workspace.update(ctx, |workspace, ctx| {
            workspace.handle_action(&WorkspaceAction::SendFeedback, ctx);
        });
    } else {
        ctx.open_url(&crate::util::links::feedback_form_url());
    }
}

/// Handler for tab detachment using the transferable views framework.
/// Instead of extracting and recreating views, this transfers the PaneGroup view tree directly.
/// Returns the new window ID if successful.
pub fn detach_tab_with_transfer(
    arg: &DetachTabImmediateArg,
    ctx: &mut AppContext,
) -> Option<WindowId> {
    let Some(source_workspace) = workspace_for_window(arg.source_window_id, ctx) else {
        log::warn!(
            "No workspace found for source window {:?}",
            arg.source_window_id
        );
        return None;
    };

    let transferred_tab = source_workspace.read(ctx, |workspace, ctx| {
        workspace.get_tab_transfer_info(arg.tab_index, ctx)
    })?;

    let window_size = ctx
        .windows()
        .platform_window(arg.source_window_id)
        .map(|window| window.as_ctx().size())
        .unwrap_or(*FALLBACK_WINDOW_SIZE);

    let window_position = arg.window_position.unwrap_or_default();

    let info = TabTransferInfo {
        transferred_tab,
        window_size,
        window_position,
        source_window_id: arg.source_window_id,
    };

    let (new_window_id, _transferred_view_ids) = create_transferred_window(info, false, ctx);

    source_workspace.update(ctx, |workspace, ctx| {
        workspace.remove_tab_without_undo(arg.tab_index, ctx);
    });

    Some(new_window_id)
}

/// Creates a new window with the transferred pane group.
/// This function takes pre-gathered TabTransferInfo, allowing it to be called
/// from within a view method where workspace lookup would fail.
///
/// If `for_drag` is true, the window is created without stealing focus (for drag preview).
///
/// Returns the new window ID and the list of transferred view entity IDs.
/// The transferred view IDs are needed by `tab_drag::on_tab_drag` to track which
/// views must follow the tab during subsequent handoff/reverse-handoff cycles.
pub fn create_transferred_window(
    info: TabTransferInfo,
    for_drag: bool,
    ctx: &mut AppContext,
) -> (WindowId, Vec<EntityId>) {
    let global_resource_handles = GlobalResourceHandlesProvider::handle(ctx)
        .as_ref(ctx)
        .get()
        .clone();
    let window_settings = WindowSettings::handle(ctx).as_ref(ctx);

    let window_bounds =
        WindowBounds::ExactPosition(RectF::new(info.window_position, info.window_size));

    let window_style = if for_drag {
        WindowStyle::PositionedNoFocus
    } else {
        WindowStyle::Normal
    };

    let (new_window_id, _) = ctx.add_window(
        AddWindowOptions {
            window_style,
            window_bounds,
            title: Some(WINDOW_TITLE.to_owned()),
            background_blur_radius_pixels: Some(*window_settings.background_blur_radius),
            background_blur_texture: *window_settings.background_blur_texture,
            on_gpu_driver_selected: on_gpu_driver_selected_callback(),
            ..Default::default()
        },
        |ctx| {
            let mut view = RootView::new(
                global_resource_handles.clone(),
                NewWorkspaceSource::TransferredTab {
                    tab_color: info.transferred_tab.color,
                    custom_title: info.transferred_tab.custom_title.clone(),
                    left_panel_open: info.transferred_tab.left_panel_open,
                    vertical_tabs_panel_open: info.transferred_tab.vertical_tabs_panel_open,
                    right_panel_open: info.transferred_tab.right_panel_open,
                    is_right_panel_maximized: info.transferred_tab.is_right_panel_maximized,
                    for_drag_preview: for_drag,
                },
                ctx,
            );
            if !for_drag {
                view.focus(ctx);
            }
            view
        },
    );

    let pane_group_id = info.transferred_tab.pane_group.id();
    let transferred_view_ids =
        ctx.transfer_view_tree_to_window(pane_group_id, info.source_window_id, new_window_id);

    if let Some(new_workspace) = workspace_for_window(new_window_id, ctx) {
        new_workspace.update(ctx, |workspace, ctx| {
            workspace.adopt_transferred_pane_group(info.transferred_tab.pane_group.clone(), ctx);
        });
    } else {
        log::warn!("Failed to find workspace in newly created window {new_window_id:?}");
    }
    (new_window_id, transferred_view_ids)
}

#[cfg(feature = "crash_reporting")]
fn on_gpu_driver_selected_callback() -> Option<Box<OnGPUDeviceSelected>> {
    Some(Box::new(|gpu_device_info| {
        crate::crash_reporting::set_gpu_device_info(gpu_device_info)
    }))
}

#[cfg(not(feature = "crash_reporting"))]
fn on_gpu_driver_selected_callback() -> Option<Box<OnGPUDeviceSelected>> {
    None
}

fn open_from_restored(arg: &OpenFromRestoredArg, ctx: &mut AppContext) {
    let global_resource_handles = GlobalResourceHandlesProvider::as_ref(ctx).get().clone();
    IntervalTimer::handle(ctx).update(ctx, |timer, _| {
        timer.mark_interval_end("HANDLING_OPEN_ACTION");
    });

    if let Some(app_state) = &arg.app_state {
        maybe_register_global_window_shortcuts(global_resource_handles.clone(), ctx);

        let (background_blur_radius_pixels, background_blur_texture) = {
            let window_settings = WindowSettings::as_ref(ctx);
            (
                Some(*window_settings.background_blur_radius),
                *window_settings.background_blur_texture,
            )
        };

        // Check whether user has enabled session restoration.
        if *GeneralSettings::as_ref(ctx).restore_session {
            let mut active_index = None;
            let mut normal_window_count = 0;
            for (idx, window) in app_state.windows.iter().enumerate() {
                // If this window is a quake window, hide it by default.
                if window.quake_mode {
                    // If this is Windows, skip restoring the quake window. Creating a hidden window
                    // is not supported on Windows. We can't have the quake window visible on
                    // startup or else it will get mistaken for a normal window.
                    if cfg!(windows) {
                        continue;
                    }
                    let frame_args = quake_mode_config(
                        &KeysSettings::as_ref(ctx)
                            .quake_mode_settings
                            .value()
                            .clone(),
                        ctx,
                    );

                    let (id, _) = ctx.add_window(
                        AddWindowOptions {
                            window_style: WindowStyle::Pin,
                            window_bounds: WindowBounds::ExactPosition(frame_args.window_bounds),
                            title: Some("Warp".to_owned()),
                            fullscreen_state: window.fullscreen_state,
                            background_blur_radius_pixels,
                            background_blur_texture,
                            // Don't use the quake window for positioning new windows.
                            anchor_new_windows_from_closed_position:
                                NextNewWindowsHasThisWindowsBoundsUponClose::No,
                            on_gpu_driver_selected: on_gpu_driver_selected_callback(),
                            window_instance: Some(ChannelState::app_id().to_string() + "-hotkey"),
                        },
                        |ctx| {
                            let mut view = RootView::new(
                                global_resource_handles.clone(),
                                NewWorkspaceSource::Restored {
                                    window_snapshot: window.clone(),
                                    block_lists: app_state.block_lists.clone(),
                                },
                                ctx,
                            );
                            view.focus(ctx);
                            view
                        },
                    );
                    ctx.windows().hide_window(id);

                    let mut quake_mode_state = QUAKE_STATE.lock();
                    *quake_mode_state = Some(QuakeModeState {
                        window_state: WindowState::Hidden,
                        window_id: id,
                        active_display_id: frame_args.display_id,
                    });
                } else {
                    normal_window_count += 1;
                    if app_state
                        .active_window_index
                        .map(|window_idx| window_idx == idx)
                        .unwrap_or(false)
                    {
                        active_index = Some(idx);
                    } else {
                        ctx.add_window(
                            AddWindowOptions {
                                window_bounds: WindowBounds::new(window.bounds),
                                title: Some("Warp".to_owned()),
                                fullscreen_state: window.fullscreen_state,
                                background_blur_radius_pixels,
                                background_blur_texture,
                                on_gpu_driver_selected: on_gpu_driver_selected_callback(),
                                ..Default::default()
                            },
                            |ctx| {
                                let mut view = RootView::new(
                                    global_resource_handles.clone(),
                                    NewWorkspaceSource::Restored {
                                        window_snapshot: window.clone(),
                                        block_lists: app_state.block_lists.clone(),
                                    },
                                    ctx,
                                );
                                view.focus(ctx);
                                view
                            },
                        );
                    }
                }
            }

            // If only the quake mode window was restored (which starts hidden), create a new normal
            // window so that something visible is created on startup.
            if normal_window_count == 0 {
                let window_settings = WindowSettings::as_ref(ctx);
                let options = default_window_options(window_settings, ctx);
                ctx.add_window(options, |ctx| {
                    let mut view = RootView::new(
                        global_resource_handles.clone(),
                        NewWorkspaceSource::Empty {
                            previous_active_window: None,
                            shell: None,
                        },
                        ctx,
                    );
                    view.focus(ctx);
                    view
                });
            }

            // Create the active window last to make sure it is focused on startup.
            if let Some(idx) = active_index {
                let window = app_state
                    .windows
                    .get(idx)
                    .expect("Window should exist at idx");
                ctx.add_window(
                    AddWindowOptions {
                        window_bounds: WindowBounds::new(window.bounds),
                        title: Some("Warp".to_owned()),
                        fullscreen_state: window.fullscreen_state,
                        background_blur_radius_pixels,
                        background_blur_texture,
                        on_gpu_driver_selected: on_gpu_driver_selected_callback(),
                        ..Default::default()
                    },
                    |ctx| {
                        let mut view = RootView::new(
                            global_resource_handles,
                            NewWorkspaceSource::Restored {
                                window_snapshot: window.clone(),
                                block_lists: app_state.block_lists.clone(),
                            },
                            ctx,
                        );
                        view.focus(ctx);
                        view
                    },
                );
            }
        }
    }
}

fn path_if_directory(path: &Path) -> Option<&Path> {
    path.is_dir().then_some(path)
}

/// Opens a new window with the workspace configured according to `source`. Returns the
/// newly-opened window ID and a handle to the root view in that window.
///
/// This is the canonical way to open a new Warp window - all other entrypoints should delegate to
/// it if possible.
pub(crate) fn open_new_with_workspace_source(
    source: NewWorkspaceSource,
    ctx: &mut AppContext,
) -> (WindowId, ViewHandle<RootView>) {
    let global_resource_handles = GlobalResourceHandlesProvider::as_ref(ctx).get().clone();
    let window_settings = WindowSettings::as_ref(ctx);
    let options = default_window_options(window_settings, ctx);
    ctx.add_window(options, |ctx| {
        let mut view = RootView::new(global_resource_handles, source, ctx);
        view.focus(ctx);
        view
    })
}

pub(crate) fn open_new_from_path(
    arg: &OpenPath,
    ctx: &mut AppContext,
) -> (WindowId, ViewHandle<RootView>) {
    open_new_with_workspace_source(
        NewWorkspaceSource::Session {
            options: Box::new(
                NewTerminalOptions::default()
                    .with_initial_directory_opt(path_if_directory(&arg.path).map(Into::into)),
            ),
        },
        ctx,
    )
}

/// Opens a new window and tries to join session identified by the session ID.
fn open_shared_session_as_viewer(session_id: &SessionId, ctx: &mut AppContext) {
    open_new_with_workspace_source(
        NewWorkspaceSource::SharedSessionAsViewer {
            session_id: *session_id,
        },
        ctx,
    );
}

/// Opens a new window to view a persisted view-only cloud conversation.
/// The conversation data is loaded via GraphQL API.
fn open_conversation_viewer(conversation_id: &ServerConversationToken, ctx: &mut AppContext) {
    // Trigger the workspace loading mechanism by dispatching the LoadConversationData event
    // This will open a new window with a loading state, fetch data via GraphQL, and display it
    open_new_with_workspace_source(
        NewWorkspaceSource::FromCloudConversationId {
            conversation_id: conversation_id.clone(),
        },
        ctx,
    );
}

/// Opens a new window and starts the guided `/create-environment` setup flow.
fn create_environment(arg: &CreateEnvironmentArg, ctx: &mut AppContext) {
    let _ = (arg, ctx);
    log::info!("Ignoring create-environment action; Dwarf does not expose cloud environments");
}

/// Opens a new window and starts the guided `/create-environment` setup flow immediately.
fn create_environment_and_run(arg: &CreateEnvironmentArg, ctx: &mut AppContext) {
    let _ = (arg, ctx);
    log::info!("Ignoring create-environment action; Dwarf does not expose cloud environments");
}
fn open_settings_page_in_new_window(section: &SettingsSection, ctx: &mut AppContext) {
    let root_handle = open_new_window_get_handles(None, ctx).1;
    root_handle.update(ctx, |root_view, ctx| {
        if let AuthOnboardingState::Terminal(workspace_view_handle) =
            &root_view.auth_onboarding_state
        {
            let window_id = ctx.window_id();
            ctx.dispatch_typed_action_for_view(
                window_id,
                workspace_view_handle.id(),
                &WorkspaceAction::ShowSettingsPage(*section),
            );
        }
    });
}

/// MCP servers need to wait for initial load to complete, so we have this action in addition
/// to the general-purpose [`open_settings_page_in_new_window`].
fn open_mcp_settings_in_new_window(args: &OpenMCPSettingsArgs, ctx: &mut AppContext) {
    let autoinstall = args.autoinstall.clone();
    let root_handle = open_new_window_get_handles(None, ctx).1;
    root_handle.update(ctx, |root_view, ctx| {
        if let AuthOnboardingState::Terminal(workspace_view_handle) =
            &root_view.auth_onboarding_state
        {
            workspace_view_handle.update(ctx, |workspace, ctx| {
                workspace.open_mcp_servers_page(
                    MCPServersSettingsPage::List,
                    autoinstall.as_deref(),
                    ctx,
                )
            });
        }
    });
}

/// Opens a new window and shows the Codex modal.
fn open_codex_in_new_window(_: &(), ctx: &mut AppContext) {
    let root_handle = open_new_window_get_handles(None, ctx).1;
    root_handle.update(ctx, |root_view, ctx| {
        if let AuthOnboardingState::Terminal(workspace_view_handle) =
            &root_view.auth_onboarding_state
        {
            workspace_view_handle.update(ctx, |workspace, ctx| {
                workspace.open_codex_modal(ctx);
            });
        }
    });
}

/// Opens a new window and enters agent view with the Linear issue work prompt.
fn open_linear_issue_work_in_new_window(args: &LinearIssueWork, ctx: &mut AppContext) {
    let (_, root_handle) = open_new_window_get_handles(None, ctx);
    let args = args.clone();
    root_handle.update(ctx, |root_view, ctx| {
        if let AuthOnboardingState::Terminal(workspace_view_handle) =
            &root_view.auth_onboarding_state
        {
            workspace_view_handle.update(ctx, |workspace, ctx| {
                workspace.open_linear_issue_work(&args, ctx);
            });
        }
    });
}

fn display_object_missing_error_in_window(window_id: WindowId, ctx: &mut AppContext) {
    crate::workspace::ToastStack::handle(ctx).update(ctx, |toast_stack, ctx| {
        let toast = DismissibleToast::error(String::from("Resource not found or access denied"));
        toast_stack.add_ephemeral_toast(toast, window_id, ctx);
    });
}

/// Creates a new window and returns its [`WindowId`] and root view's [`ViewHandle`].
pub(crate) fn open_new_window_get_handles(
    shell: Option<AvailableShell>,
    ctx: &mut AppContext,
) -> (WindowId, ViewHandle<RootView>) {
    let active_window_id = ctx.windows().active_window();
    open_new_with_workspace_source(
        NewWorkspaceSource::Empty {
            previous_active_window: active_window_id,
            shell,
        },
        ctx,
    )
}

/// Opens a new window.
fn open_new(_: &(), ctx: &mut AppContext) {
    open_new_window_get_handles(None, ctx);
}

/// Opens a new window with a specific shell
fn open_new_with_shell(shell: &Option<AvailableShell>, ctx: &mut AppContext) {
    open_new_window_get_handles(shell.to_owned(), ctx);
}

/// Global action that performs a few steps:
/// 1. Open a new tab, or open a window if there is none.
/// 2. Set the terminal input buffer to a command that should open a subshell
/// 3. Set a flag that we should automatically bootstrap that subshell if its we can boostrap its
/// [`ShellType`].
fn open_new_tab_insert_subshell_command_and_bootstrap_if_supported(
    arg: &SubshellCommandArg,
    ctx: &mut AppContext,
) {
    let root_view_handle: Option<ViewHandle<RootView>> = ctx
        .windows()
        .frontmost_window_id()
        .and_then(|window_id| ctx.root_view(window_id));

    let root_view_handle = match root_view_handle {
        Some(root_view_handle) => {
            root_view_handle.update(ctx, |root_view, ctx| {
                if let AuthOnboardingState::Terminal(workspace_view_handle) =
                    &root_view.auth_onboarding_state
                {
                    workspace_view_handle.update(ctx, |workspace, ctx| {
                        workspace.add_terminal_tab(false /* hide_homepage */, ctx);
                    });
                }
            });
            root_view_handle
        }
        None => open_new_window_get_handles(None, ctx).1,
    };

    root_view_handle.update(ctx, |root_view, ctx| {
        root_view.insert_subshell_command_and_bootstrap_if_supported(arg, ctx);
    });
}

/// Returns the common configuration for a new "regular" window (not Quake Mode).
fn default_window_options(window_settings: &WindowSettings, ctx: &AppContext) -> AddWindowOptions {
    let (inherited_bounds, window_style) = ctx.next_window_bounds_and_style();
    let next_bounds =
        bounds_for_opening_at_custom_window_size(inherited_bounds, window_settings, ctx);

    AddWindowOptions {
        window_style,
        window_bounds: next_bounds,
        title: Some("Warp".to_owned()),
        background_blur_radius_pixels: Some(*window_settings.background_blur_radius),
        background_blur_texture: *window_settings.background_blur_texture,
        on_gpu_driver_selected: on_gpu_driver_selected_callback(),
        ..Default::default()
    }
}

/// Returns the bounds to open the next window at taking into account whether
/// the user has configured their settings to open windows at a custom size
/// and whether that feature is flagged on.
fn bounds_for_opening_at_custom_window_size(
    bounds: WindowBounds,
    window_settings: &WindowSettings,
    app: &AppContext,
) -> WindowBounds {
    if *window_settings.open_windows_at_custom_size.value() {
        let font_cache = app.font_cache();
        let appearance = Appearance::as_ref(app);

        let cell_size_and_padding = cell_size_and_padding(
            font_cache,
            appearance.monospace_font_family(),
            appearance.monospace_font_size(),
            appearance.ui_builder().line_height_ratio(),
        );
        let window_size = vec2f(
            *window_settings.new_windows_num_columns.value() as f32
                * cell_size_and_padding.cell_width_px.as_f32()
                + 2. * cell_size_and_padding.padding_x_px.as_f32(),
            *window_settings.new_windows_num_rows.value() as f32
                * cell_size_and_padding.cell_height_px.as_f32()
                + 2. * cell_size_and_padding.padding_y_px.as_f32(),
        );

        match bounds {
            WindowBounds::ExactPosition(rect) => {
                WindowBounds::ExactPosition(RectF::new(rect.origin(), window_size))
            }
            WindowBounds::ExactSize(_) | WindowBounds::Default => {
                WindowBounds::ExactSize(window_size)
            }
        }
    } else {
        bounds
    }
}

pub fn quake_mode_window_is_open() -> bool {
    let quake_mode_state = QUAKE_STATE.lock();

    quake_mode_state
        .as_ref()
        .map(|state| {
            matches!(
                state.window_state,
                WindowState::Open | WindowState::PendingOpen
            )
        })
        .unwrap_or_default()
}

pub fn quake_mode_window_id() -> Option<WindowId> {
    let quake_mode_state = QUAKE_STATE.lock();

    quake_mode_state.as_ref().map(|state| state.window_id)
}

pub fn set_quake_mode(new_state: Option<QuakeModeState>) {
    let mut quake_mode_state = QUAKE_STATE.lock();
    *quake_mode_state = new_state;
}

fn move_quake_mode_window_from_screen_change(settings: &QuakeModeSettings, ctx: &mut AppContext) {
    fit_quake_mode_window_within_active_screen(
        settings,
        QuakeModeMoveTrigger::ScreenConfigurationChange,
        ctx,
    )
}

/// If there exists a quake window, mutate its size and position, i.e. its bounds, to match the
/// bounds specified by the [`QuakeModeSettings`].
pub fn update_quake_window_bounds(quake_settings: &QuakeModeSettings, ctx: &mut AppContext) {
    let config = quake_mode_config(quake_settings, ctx);
    let Some(ref state) = *QUAKE_STATE.lock() else {
        return;
    };
    ctx.windows()
        .set_window_bounds(state.window_id, config.window_bounds);
}

/// Move Quake Mode window to the active screen if it is already open or hidden.
fn fit_quake_mode_window_within_active_screen(
    settings: &QuakeModeSettings,
    trigger: QuakeModeMoveTrigger,
    ctx: &mut AppContext,
) {
    let mut quake_mode_state = QUAKE_STATE.lock();

    if let Some(state) = quake_mode_state.as_mut() {
        let active_id = ctx.windows().active_display_id();

        // When there is no screen config and active screen id change, we don't need to reposition
        // the quake mode window as its position should still be valid.
        if matches!(trigger, QuakeModeMoveTrigger::ActiveScreenSetting)
            && active_id == state.active_display_id
        {
            return;
        }

        let window_bound = settings.resolve_quake_mode_bounds(ctx);
        ctx.windows()
            .set_window_bounds(state.window_id, window_bound);
        state.active_display_id = active_id;
    }
}

fn update_quake_mode_state(arg: &UpdateQuakeModeEventArg, ctx: &mut AppContext) {
    if !KeysSettings::as_ref(ctx)
        .quake_mode_settings
        .hide_window_when_unfocused
    {
        return;
    }

    {
        let mut quake_mode_state = QUAKE_STATE.lock();

        if let Some(state) = quake_mode_state.as_mut() {
            state.window_state = match state.window_state {
                WindowState::PendingOpen => WindowState::Open,
                WindowState::Open => {
                    if arg.active_window_id.is_some_and(|id| id == state.window_id) {
                        WindowState::Open
                    } else {
                        ctx.windows().hide_window(state.window_id);
                        WindowState::Hidden
                    }
                }
                WindowState::Hidden => WindowState::Hidden,
            }
        }
    }
}

// Configuration of the next positioning of the quake mode window.
fn quake_mode_config(settings: &QuakeModeSettings, ctx: &mut AppContext) -> QuakeModeFrameConfig {
    QuakeModeFrameConfig {
        display_id: ctx.windows().active_display_id(),
        window_bounds: settings.resolve_quake_mode_bounds(ctx),
    }
}

fn get_quake_mode_state(ctx: &mut AppContext) -> Option<QuakeModeState> {
    let quake_mode_state = QUAKE_STATE.lock();

    match quake_mode_state.as_ref() {
        Some(state) if ctx.is_window_open(state.window_id) => Some(state.clone()),
        _ => None,
    }
}

fn toggle_quake_mode_window(global_resource_handles: &GlobalResourceHandles, ctx: &mut AppContext) {
    // Get the current state of quake mode.
    let state = get_quake_mode_state(ctx);
    match state {
        None => {

            let config = quake_mode_config(
                &KeysSettings::as_ref(ctx)
                    .quake_mode_settings
                    .value()
                    .clone(),
                ctx,
            );

            let window_settings = WindowSettings::as_ref(ctx);

            let active_window_id = ctx.windows().active_window();
            let (id, _) = ctx.add_window(
                AddWindowOptions {
                    window_style: WindowStyle::Pin,
                    window_bounds: WindowBounds::ExactPosition(config.window_bounds),
                    title: Some("Warp".to_owned()),
                    background_blur_radius_pixels: Some(*window_settings.background_blur_radius),
                    background_blur_texture: *window_settings.background_blur_texture,
                    // Ignore the quake window for positioning the next window
                    anchor_new_windows_from_closed_position:
                        warpui::NextNewWindowsHasThisWindowsBoundsUponClose::No,
                    on_gpu_driver_selected: on_gpu_driver_selected_callback(),
                    window_instance: Some(ChannelState::app_id().to_string() + "-hotkey"),
                    ..Default::default()
                },
                |ctx| {
                    let mut view = RootView::new(
                        global_resource_handles.clone(),
                        NewWorkspaceSource::Empty {
                            previous_active_window: active_window_id,
                            shell: None,
                        },
                        ctx,
                    );
                    view.focus(ctx);
                    view
                },
            );

            // Update quake mode state after the call to prevent deadlocking.
            let mut quake_mode_state = QUAKE_STATE.lock();
            *quake_mode_state = Some(QuakeModeState {
                window_state: WindowState::PendingOpen,
                window_id: id,
                active_display_id: config.display_id,
            });
        }
        Some(state) if matches!(state.window_state, WindowState::Hidden) => {

            // If quake mode does not have a set pin screen -- move it to the current active screen.
            if KeysSettings::as_ref(ctx)
                .quake_mode_settings
                .pin_screen
                .is_none()
            {
                fit_quake_mode_window_within_active_screen(
                    &KeysSettings::as_ref(ctx)
                        .quake_mode_settings
                        .value()
                        .clone(),
                    QuakeModeMoveTrigger::ActiveScreenSetting,
                    ctx,
                );
            }
            ctx.windows().show_window_and_focus_app(state.window_id);

            // Update quake mode state after the call to prevent deadlocking.
            let mut quake_mode_state = QUAKE_STATE.lock();

            if let Some(state) = quake_mode_state.as_mut() {
                state.window_state = WindowState::PendingOpen;
            }
        }
        Some(state) => {
            ctx.windows().hide_window(state.window_id);

            // Update quake mode state after the call to prevent deadlocking.
            let mut quake_mode_state = QUAKE_STATE.lock();

            if let Some(state) = quake_mode_state.as_mut() {
                state.window_state = WindowState::Hidden;
            }
        }
    };
}

/// This action will show or hide all of Warp's windows except the quake window
///
/// - If Warp is active and has any windows, hide those windows.
/// - If Warp is hidden, show all windows.
/// - If Warp is active but has 0 normal windows, create a new window with a new session.
fn show_or_hide_non_quake_mode_windows(_: &(), ctx: &mut AppContext) {
    let quake_window_id = get_quake_mode_state(ctx).map(|state| state.window_id);
    let non_quake_mode_window_ids = ctx
        .window_ids()
        .filter(|window_id| Some(window_id) != quake_window_id.as_ref());
    if non_quake_mode_window_ids.count() == 0 {
        // If there are no normal windows, this action should create one.
        open_new(&(), ctx);
    }
    let windowing_model = ctx.windows();
    // Now there is at least one window. If a Warp window is active, hide the app.
    // Otherwise, show activate the app to show it in front.
    let active_window_id = windowing_model.active_window();
    match active_window_id {
        Some(_) => windowing_model.hide_app(),
        None => {
            windowing_model.activate_app();
        }
    };
}

#[cfg(feature = "voice_input")]
fn abort_voice_input(_: &(), ctx: &mut AppContext) {
    let voice_input = voice_input::VoiceInput::handle(ctx);
    if voice_input.as_ref(ctx).is_listening() {
        voice_input.update(ctx, |voice_input, _| {
            voice_input.abort_listening();
        });
    }
}

#[derive(Clone)]
pub enum NewWorkspaceSource {
    Empty {
        previous_active_window: Option<WindowId>,
        shell: Option<AvailableShell>,
    },
    FromTemplate {
        window_template: launch_config::WindowTemplate,
    },
    Restored {
        window_snapshot: WindowSnapshot,
        block_lists: Arc<HashMap<PaneUuid, Vec<SerializedBlockListItem>>>,
    },
    Session {
        options: Box<NewTerminalOptions>,
    },
    SharedSessionAsViewer {
        session_id: SessionId,
    },
    FromCloudConversationId {
        conversation_id: ServerConversationToken,
    },
    AgentSession {
        options: Box<NewTerminalOptions>,
        initial_query: Option<String>,
    },
    /// A tab is being transferred from another window via the transferable views framework.
    /// The workspace will create a placeholder tab, which will be replaced by the transferred
    /// PaneGroup after window creation.
    TransferredTab {
        /// Tab color from the source tab
        tab_color: Option<AnsiColorIdentifier>,
        /// Custom title from the source tab
        custom_title: Option<String>,
        /// Whether the left panel was open in the source tab
        left_panel_open: bool,
        /// Captured from the source window so detached tabs inherit the panel state.
        vertical_tabs_panel_open: bool,
        /// Whether the right panel was open in the source tab
        right_panel_open: bool,
        /// Whether the right panel was maximized in the source tab
        is_right_panel_maximized: bool,
        /// Whether this transferred tab window is currently being used as a drag preview.
        for_drag_preview: bool,
    },
}

impl NewWorkspaceSource {
    pub fn has_horizontal_split(&self) -> bool {
        match self {
            NewWorkspaceSource::Restored {
                window_snapshot, ..
            } => {
                if window_snapshot.tabs.is_empty() {
                    false
                } else {
                    let active_index = window_snapshot.active_tab_index;
                    let active_tab = window_snapshot
                        .tabs
                        .get(active_index)
                        .unwrap_or(&window_snapshot.tabs[0]);
                    active_tab.root.has_horizontal_split()
                }
            }
            _ => false,
        }
    }
}

/// Args needed to construct a `Workspace`.
#[derive(Clone)]
struct WorkspaceArgs {
    global_resource_handles: GlobalResourceHandles,
    server_time: Option<Arc<ServerTime>>,
    workspace_setting: NewWorkspaceSource,
}

// Some onboarding states can either contain a ref to an existing terminal view
// if it exists or, if it doesn't, the args needed to create a new empty one.
#[derive(Clone)]
enum AuthOnboardingTarget {
    Workspace(Box<WorkspaceArgs>),
    Terminal(ViewHandle<Workspace>),
}

#[derive(Clone, Debug)]
enum LocalWelcomeEvent {
    Completed,
}

#[derive(Clone, Debug)]
enum LocalWelcomeAction {
    StartClicked,
}

struct LocalWelcomeView {
    start_button: button::Button,
    icon_mouse_state: MouseStateHandle,
}

impl LocalWelcomeView {
    fn new(_: &mut ViewContext<Self>) -> Self {
        Self {
            start_button: button::Button::default(),
            icon_mouse_state: MouseStateHandle::default(),
        }
    }

    fn complete(&mut self, ctx: &mut ViewContext<Self>) {
        ctx.emit(LocalWelcomeEvent::Completed);
    }

    fn render_content(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let theme = appearance.theme();
        let main_text_color = theme.main_text_color(theme.background()).into_solid();
        let sub_text_color = theme.sub_text_color(theme.background()).into_solid();

        let icon = Hoverable::new(self.icon_mouse_state.clone(), move |state| {
            let icon = ConstrainedBox::new(
                Image::new(
                    AssetSource::Bundled {
                        path: "bundled/png/local.png",
                    },
                    CacheOption::BySize,
                )
                .with_corner_radius(CornerRadius::with_all(Radius::Pixels(12.)))
                .finish(),
            )
            .with_width(112.)
            .with_height(112.)
            .finish();

            let mut container = Container::new(icon)
                .with_uniform_padding(5.)
                .with_corner_radius(CornerRadius::with_all(Radius::Pixels(17.)));
            if state.is_hovered() {
                container = container.with_background(theme.block_selection_color());
            }
            container.finish()
        })
        .with_cursor(Cursor::PointingHand)
        .on_click(|ctx, _, _| {
            ctx.dispatch_typed_action(RootViewAction::ShowConfetti(DwarfConfettiPreset::Fireworks));
        })
        .finish();

        let title =
            FormattedTextElement::from_str("Welcome to Dwarf", appearance.ui_font_family(), 42.)
                .with_color(main_text_color)
                .with_weight(Weight::Bold)
                .with_alignment(TextAlignment::Center)
                .with_line_height_ratio(1.0)
                .finish();

        let subtitle = FormattedTextElement::from_str(
            "Your local agent terminal is ready. Local auth stays on this machine.",
            appearance.ui_font_family(),
            16.,
        )
        .with_color(sub_text_color)
        .with_weight(Weight::Normal)
        .with_alignment(TextAlignment::Center)
        .with_line_height_ratio(1.25)
        .finish();

        let enter = Keystroke::parse("enter").unwrap_or_default();
        let start_button = self.start_button.render(
            appearance,
            button::Params {
                content: button::Content::Label("Start Dwarf".into()),
                theme: &button::themes::Primary,
                options: button::Options {
                    keystroke: Some(enter),
                    on_click: Some(Box::new(|ctx, _app, _pos| {
                        ctx.dispatch_typed_action(LocalWelcomeAction::StartClicked);
                    })),
                    ..button::Options::default(appearance)
                },
            },
        );
        let content = Flex::column()
            .with_main_axis_size(MainAxisSize::Min)
            .with_main_axis_alignment(MainAxisAlignment::Center)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_child(icon)
            .with_child(Container::new(title).with_margin_top(22.).finish())
            .with_child(Container::new(subtitle).with_margin_top(14.).finish())
            .with_child(Container::new(start_button).with_margin_top(28.).finish())
            .finish();

        Container::new(ConstrainedBox::new(content).with_max_width(560.).finish())
            .with_uniform_padding(32.)
            .finish()
    }
}

impl Entity for LocalWelcomeView {
    type Event = LocalWelcomeEvent;
}

impl View for LocalWelcomeView {
    fn ui_name() -> &'static str {
        "LocalWelcomeView"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let mut stack = Stack::new();
        stack.add_child(
            Container::new(Align::new(self.render_content(app)).finish())
                .with_background(Appearance::as_ref(app).theme().background())
                .finish(),
        );
        EventHandler::new(stack.finish())
            .on_keydown(move |ctx, _app, keystroke| {
                if keystroke.is_unmodified_enter() {
                    ctx.dispatch_typed_action(LocalWelcomeAction::StartClicked);
                    DispatchEventResult::StopPropagation
                } else {
                    DispatchEventResult::PropagateToParent
                }
            })
            .finish()
    }
}

impl TypedActionView for LocalWelcomeView {
    type Action = LocalWelcomeAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            LocalWelcomeAction::StartClicked => self.complete(ctx),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DwarfConfettiPreset {
    Celebration,
    Fireworks,
    Snow,
    Cannon,
}

#[derive(Clone, Copy)]
struct DwarfConfettiRun {
    started_at: instant::Instant,
    preset: DwarfConfettiPreset,
}

struct DwarfConfettiElement {
    run: DwarfConfettiRun,
    size: Option<Vector2F>,
    origin: Option<Point>,
}

#[derive(Clone, Copy, Debug)]
enum ConfettiShape {
    Ribbon,
    Circle,
}

#[derive(Clone, Copy)]
struct ConfettiOrigin {
    x: f32,
    y: f32,
}

#[derive(Clone, Copy)]
enum ConfettiPalette {
    Bright,
    Snow,
}

#[derive(Clone, Copy)]
enum ConfettiShapeMix {
    RibbonsAndCircles,
    Circles,
}

#[derive(Clone, Copy)]
struct ConfettiOptions {
    particle_count: usize,
    angle: f32,
    spread: f32,
    start_velocity: f32,
    decay: f32,
    gravity: f32,
    drift: f32,
    ticks: u32,
    origin: ConfettiOrigin,
    palette: ConfettiPalette,
    shape_mix: ConfettiShapeMix,
    scalar: f32,
}

#[derive(Clone, Copy)]
struct ConfettiBurst {
    options: ConfettiOptions,
    delay_seconds: f32,
}

struct ConfettiParticle {
    color_slot: usize,
    shape: ConfettiShape,
    start_x: f32,
    start_y: f32,
    angle_2d: f32,
    velocity: f32,
    decay: f32,
    gravity: f32,
    drift: f32,
    width: f32,
    height: f32,
    total_ticks: f32,
    spin: f32,
    wobble_speed: f32,
    scalar: f32,
}

impl DwarfConfettiElement {
    const FRAME: Duration = Duration::from_millis(16);

    fn new(run: DwarfConfettiRun) -> Self {
        Self {
            run,
            size: None,
            origin: None,
        }
    }

    fn duration_for(preset: DwarfConfettiPreset) -> Duration {
        let seconds = preset
            .bursts()
            .iter()
            .map(|burst| burst.delay_seconds + burst.options.ticks as f32 / 60.)
            .fold(0., f32::max)
            + 0.25;
        Duration::from_secs_f32(seconds)
    }

    fn default_options() -> ConfettiOptions {
        ConfettiOptions {
            particle_count: 50,
            angle: 90.,
            spread: 45.,
            start_velocity: 45.,
            decay: 0.9,
            gravity: 1.,
            drift: 0.,
            ticks: 200,
            origin: ConfettiOrigin { x: 0.5, y: 0.5 },
            palette: ConfettiPalette::Bright,
            shape_mix: ConfettiShapeMix::RibbonsAndCircles,
            scalar: 1.,
        }
    }

    fn hash_unit(index: usize, salt: u32) -> f32 {
        let mut value = index as u32;
        value = value.wrapping_mul(747_796_405).wrapping_add(2_891_336_453);
        value ^= salt.wrapping_mul(277_803_737);
        value ^= value >> 16;
        (value % 10_000) as f32 / 10_000.
    }

    fn color_pair(palette: ConfettiPalette, slot: usize, alpha: u8) -> (ColorU, ColorU) {
        match palette {
            ConfettiPalette::Bright => match slot % 7 {
                0 => (
                    ColorU::new(38, 204, 255, alpha),
                    ColorU::new(155, 235, 255, alpha),
                ),
                1 => (
                    ColorU::new(162, 90, 253, alpha),
                    ColorU::new(211, 183, 254, alpha),
                ),
                2 => (
                    ColorU::new(255, 94, 126, alpha),
                    ColorU::new(255, 174, 191, alpha),
                ),
                3 => (
                    ColorU::new(136, 255, 90, alpha),
                    ColorU::new(192, 255, 170, alpha),
                ),
                4 => (
                    ColorU::new(252, 255, 66, alpha),
                    ColorU::new(254, 255, 170, alpha),
                ),
                5 => (
                    ColorU::new(255, 166, 45, alpha),
                    ColorU::new(255, 210, 128, alpha),
                ),
                _ => (
                    ColorU::new(255, 54, 255, alpha),
                    ColorU::new(255, 169, 255, alpha),
                ),
            },
            ConfettiPalette::Snow => match slot % 2 {
                0 => (
                    ColorU::new(255, 255, 255, alpha),
                    ColorU::new(255, 255, 255, alpha),
                ),
                _ => (
                    ColorU::new(224, 224, 224, alpha),
                    ColorU::new(255, 255, 255, alpha),
                ),
            },
        }
    }

    fn particle(
        options: &ConfettiOptions,
        particle_index: usize,
        burst_index: usize,
    ) -> ConfettiParticle {
        let salt = (burst_index as u32).wrapping_mul(31);
        let rad_angle = options.angle.to_radians();
        let rad_spread = options.spread.to_radians();
        let angle_2d = -rad_angle
            + (0.5 * rad_spread - Self::hash_unit(particle_index, 1 + salt) * rad_spread);
        let velocity = options.start_velocity * (0.5 + Self::hash_unit(particle_index, 2 + salt));
        let width = (8. + Self::hash_unit(particle_index, 3 + salt) * 4.) * options.scalar;
        let height = (5. + Self::hash_unit(particle_index, 4 + salt) * 3.) * options.scalar;
        let shape = match options.shape_mix {
            ConfettiShapeMix::Circles => ConfettiShape::Circle,
            ConfettiShapeMix::RibbonsAndCircles => match particle_index % 2 {
                0 => ConfettiShape::Circle,
                _ => ConfettiShape::Ribbon,
            },
        };

        ConfettiParticle {
            color_slot: particle_index + burst_index * 257,
            shape,
            start_x: options.origin.x,
            start_y: options.origin.y,
            angle_2d,
            velocity,
            decay: options.decay.clamp(0.01, 1.),
            gravity: options.gravity * 3.,
            drift: options.drift,
            width,
            height,
            total_ticks: options.ticks as f32,
            spin: Self::hash_unit(particle_index, 11 + salt) * std::f32::consts::TAU,
            wobble_speed: 3. + Self::hash_unit(particle_index, 12 + salt) * 3.6,
            scalar: options.scalar,
        }
    }

    fn decayed_distance(particle: &ConfettiParticle, tick: f32) -> f32 {
        if (1. - particle.decay).abs() <= f32::EPSILON {
            particle.velocity * tick
        } else {
            particle.velocity * (1. - particle.decay.powf(tick)) / (1. - particle.decay)
        }
    }

    fn draw_particle_rect(
        ctx: &mut PaintContext,
        origin: Vector2F,
        size: Vector2F,
        color: ColorU,
        corner_radius: f32,
    ) {
        ctx.scene
            .draw_rect_without_hit_recording(RectF::new(origin, size))
            .with_background(ElementFill::Solid(color))
            .with_corner_radius(CornerRadius::with_all(Radius::Pixels(corner_radius)));
    }

    fn draw_particle(
        particle: &ConfettiParticle,
        position: Vector2F,
        local_time: f32,
        color: ColorU,
        highlight: ColorU,
        ctx: &mut PaintContext,
    ) {
        let wobble = (particle.spin + local_time * particle.wobble_speed).sin();
        match particle.shape {
            ConfettiShape::Ribbon => {
                let visible_width = particle.width * (0.32 + wobble.abs() * 0.68);
                let visible_height = particle.height * (0.72 + (1. - wobble.abs()) * 0.38);
                let vertical_flip = (particle.spin + local_time * particle.wobble_speed).cos() < 0.;
                let particle_size = if vertical_flip {
                    vec2f(visible_height, visible_width)
                } else {
                    vec2f(visible_width, visible_height)
                };
                let particle_origin = position - particle_size * 0.5;
                ctx.scene
                    .draw_rect_without_hit_recording(RectF::new(particle_origin, particle_size))
                    .with_background(ElementFill::Gradient {
                        start: vec2f(0., 0.),
                        end: vec2f(1., 1.),
                        start_color: highlight,
                        end_color: color,
                    })
                    .with_corner_radius(CornerRadius::with_all(Radius::Pixels(1.8)));
            }
            ConfettiShape::Circle => {
                let diameter = (particle.width * 0.72) * (0.8 + wobble.abs() * 0.2);
                Self::draw_particle_rect(
                    ctx,
                    position - vec2f(diameter, diameter) * 0.5,
                    vec2f(diameter, diameter),
                    color,
                    diameter / 2.,
                );
            }
        }
    }
}

impl DwarfConfettiPreset {
    fn bursts(self) -> Vec<ConfettiBurst> {
        let default = DwarfConfettiElement::default_options();
        match self {
            Self::Celebration => vec![
                ConfettiBurst {
                    options: ConfettiOptions {
                        particle_count: 50,
                        angle: 60.,
                        spread: 55.,
                        origin: ConfettiOrigin { x: 0., y: 0.6 },
                        ..default
                    },
                    delay_seconds: 0.,
                },
                ConfettiBurst {
                    options: ConfettiOptions {
                        particle_count: 50,
                        angle: 120.,
                        spread: 55.,
                        origin: ConfettiOrigin { x: 1., y: 0.6 },
                        ..default
                    },
                    delay_seconds: 0.18,
                },
            ],
            Self::Fireworks => vec![ConfettiBurst {
                options: ConfettiOptions {
                    particle_count: 100,
                    spread: 360.,
                    start_velocity: 30.,
                    gravity: 0.5,
                    origin: ConfettiOrigin { x: 0.5, y: 0.5 },
                    ..default
                },
                delay_seconds: 0.,
            }],
            Self::Snow => vec![ConfettiBurst {
                options: ConfettiOptions {
                    particle_count: 50,
                    spread: 180.,
                    start_velocity: 10.,
                    gravity: 0.3,
                    ticks: 400,
                    origin: ConfettiOrigin { x: 0.5, y: 0. },
                    palette: ConfettiPalette::Snow,
                    shape_mix: ConfettiShapeMix::Circles,
                    scalar: 0.85,
                    ..default
                },
                delay_seconds: 0.,
            }],
            Self::Cannon => vec![ConfettiBurst {
                options: ConfettiOptions {
                    particle_count: 150,
                    spread: 60.,
                    start_velocity: 55.,
                    origin: ConfettiOrigin { x: 0.5, y: 1. },
                    ..default
                },
                delay_seconds: 0.,
            }],
        }
    }
}

impl Element for DwarfConfettiElement {
    fn layout(
        &mut self,
        constraint: SizeConstraint,
        ctx: &mut LayoutContext,
        _app: &AppContext,
    ) -> Vector2F {
        let fallback = ctx.window_size.max(vec2f(1., 1.));
        let size = vec2f(
            if constraint.max.x().is_finite() {
                constraint.max.x().max(1.)
            } else {
                fallback.x().max(constraint.min.x()).max(1.)
            },
            if constraint.max.y().is_finite() {
                constraint.max.y().max(1.)
            } else {
                fallback.y().max(constraint.min.y()).max(1.)
            },
        );
        self.size = Some(size);
        size
    }

    fn after_layout(&mut self, _ctx: &mut AfterLayoutContext, _app: &AppContext) {}

    fn paint(&mut self, origin: Vector2F, ctx: &mut PaintContext, _app: &AppContext) {
        let Some(size) = self.size else {
            return;
        };

        self.origin = Some(Point::from_vec2f(origin, ctx.scene.z_index()));

        let elapsed = self.run.started_at.elapsed();
        let duration = Self::duration_for(self.run.preset);
        let progress = (elapsed.as_secs_f32() / duration.as_secs_f32()).clamp(0., 1.);
        if progress >= 1. {
            return;
        }

        let time = elapsed.as_secs_f32();

        for (burst_index, burst) in self.run.preset.bursts().into_iter().enumerate() {
            let local_time = time - burst.delay_seconds;
            if local_time <= 0. {
                continue;
            }

            for particle_index in 0..burst.options.particle_count {
                let particle = Self::particle(&burst.options, particle_index, burst_index);
                let tick = local_time * 60.;
                let life_progress = (tick / particle.total_ticks).clamp(0., 1.);
                if life_progress >= 1. {
                    continue;
                }

                let alpha = ((1. - life_progress) * 255.).round() as u8;
                let (color, highlight) =
                    Self::color_pair(burst.options.palette, particle.color_slot, alpha);
                let distance = Self::decayed_distance(&particle, tick);
                let x = size.x() * particle.start_x
                    + particle.angle_2d.cos() * distance
                    + particle.drift * tick;
                let y = size.y() * particle.start_y
                    + particle.angle_2d.sin() * distance
                    + particle.gravity * tick;

                let padding = 24. * particle.scalar;
                if x < -padding || x > size.x() + padding || y < -padding || y > size.y() + padding
                {
                    continue;
                }

                Self::draw_particle(
                    &particle,
                    origin + vec2f(x, y),
                    local_time,
                    color,
                    highlight,
                    ctx,
                );
            }
        }

        ctx.repaint_after(Self::FRAME);
    }

    fn dispatch_event(
        &mut self,
        _event: &warpui::event::DispatchedEvent,
        _ctx: &mut EventContext,
        _app: &AppContext,
    ) -> bool {
        false
    }

    fn size(&self) -> Option<Vector2F> {
        self.size
    }

    fn origin(&self) -> Option<Point> {
        self.origin
    }
}

/// User preferences key to track whether the user has completed the onboarding slides locally
/// (before login). This is needed because the server-side `is_onboarded` flag requires
/// authentication.
const HAS_COMPLETED_ONBOARDING_KEY: &str = "HasCompletedOnboarding";

/// Returns whether the user has completed the onboarding slides locally (before login).
pub(crate) fn has_completed_local_onboarding(ctx: &AppContext) -> bool {
    ctx.private_user_preferences()
        .read_value(HAS_COMPLETED_ONBOARDING_KEY)
        .unwrap_or_default()
        .and_then(|s| serde_json::from_str::<bool>(&s).ok())
        .unwrap_or(false)
}

/// Persists the local onboarding-completed flag so we don't show onboarding again.
fn mark_local_onboarding_completed(ctx: &AppContext) {
    let _ = ctx.private_user_preferences().write_value(
        HAS_COMPLETED_ONBOARDING_KEY,
        serde_json::to_string(&true).expect("bool serializes to JSON"),
    );
}

fn local_agent_terminal_only() -> bool {
    matches!(ChannelState::channel(), Channel::Oss)
}

/// Whether onboarding has completed and we should render the `Workspace`.
/// In the Dwarf fork, login is always considered complete, so we only model
/// the local welcome / onboarding states and the terminal state.
enum AuthOnboardingState {
    Onboarding {
        onboarding_view: ViewHandle<AgentOnboardingView>,
        target: AuthOnboardingTarget,
    },
    LocalWelcome {
        welcome_view: ViewHandle<LocalWelcomeView>,
        target: AuthOnboardingTarget,
    },
    Terminal(ViewHandle<Workspace>),
}

pub struct RootView {
    auth_onboarding_state: AuthOnboardingState,
    server_time: Option<Arc<ServerTime>>,
    pub server_api: Arc<ServerApi>,
    pub model_event_sender: Option<SyncSender<ModelEvent>>,
    mouse_states: TrafficLightMouseStates,
    /// The window ID is needed because the "maximize" button needs to change its icon based on
    /// whether or not the current window is maximized. Ideally the window ID could just be fetched
    /// in the [`Self::render`] method, but there is no [`ViewContext`] available there. So, we
    /// need to store it in a field instead.
    window_id: WindowId,
    /// Stores the tutorial from onboarding when the user needs to log in before
    /// the guided tour can start. Consumed after auth completes.
    pending_tutorial: Option<OnboardingTutorial>,
    /// settings to apply after a new user login / initial cloud load completes
    pending_post_auth_onboarding_settings: Option<SelectedSettings>,
    confetti_run: Option<DwarfConfettiRun>,
}

impl RootView {
    pub fn new(
        global_resource_handles: GlobalResourceHandles,
        workspace_setting: NewWorkspaceSource,
        ctx: &mut ViewContext<Self>,
    ) -> Self {
        let server_api_provider = ServerApiProvider::as_ref(ctx);
        let server_api = server_api_provider.get();
        let auth_state = AuthStateProvider::as_ref(ctx).get().clone();

        ctx.subscribe_to_model(&AuthManager::handle(ctx), |me, _, event, ctx| {
            me.handle_auth_manager_event(event, ctx);
        });

        // CloudPreferencesSyncer was removed; nothing to subscribe to.

        let model_event_sender = global_resource_handles.model_event_sender.clone();
        let workspace_args = WorkspaceArgs {
            global_resource_handles,
            server_time: None,
            workspace_setting,
        };

        // The Dwarf fork is local-only: there is no remote login flow. We pick
        // between the local welcome / onboarding state machine slides and the
        // terminal itself depending on whether the user has completed the
        // local onboarding flow before.
        let auth_onboarding_state = if local_agent_terminal_only() {
            if has_completed_local_onboarding(ctx) {
                AuthOnboardingState::Terminal(workspace_args.create_workspace(ctx))
            } else {
                let welcome_view = Self::create_local_welcome_view(ctx);
                AuthOnboardingState::LocalWelcome {
                    welcome_view,
                    target: AuthOnboardingTarget::Workspace(workspace_args.into()),
                }
            }
        } else {
            // When OpenWarpNewSettingsModes is enabled, show onboarding to
            // users who haven't completed it yet (tracked via a local
            // UserPreferences key).
            let has_completed_local_onboarding = FeatureFlag::OpenWarpNewSettingsModes.is_enabled()
                && has_completed_local_onboarding(ctx);
            let should_show_onboarding = FeatureFlag::OpenWarpNewSettingsModes.is_enabled()
                && FeatureFlag::AgentOnboarding.is_enabled()
                && !has_completed_local_onboarding;
            if should_show_onboarding {
                let workspace_args_box: Box<WorkspaceArgs> = workspace_args.into();
                let onboarding_view = Self::create_agent_onboarding_view(ctx);
                onboarding_view.update(ctx, |view, ctx| {
                    view.start_onboarding(ctx);
                });
                AuthOnboardingState::Onboarding {
                    onboarding_view,
                    target: AuthOnboardingTarget::Workspace(workspace_args_box),
                }
            } else {
                AuthOnboardingState::Terminal(workspace_args.create_workspace(ctx))
            }
        };

        let confetti_run = matches!(
            auth_onboarding_state,
            AuthOnboardingState::LocalWelcome { .. }
        )
        .then_some(DwarfConfettiRun {
            started_at: instant::Instant::now(),
            preset: DwarfConfettiPreset::Celebration,
        });

        let root_view = Self {
            auth_onboarding_state,
            server_time: None,
            server_api: server_api.clone(),
            model_event_sender,
            mouse_states: Default::default(),
            window_id: ctx.window_id(),
            pending_tutorial: None,
            pending_post_auth_onboarding_settings: None,
            confetti_run,
        };

        let autoupdate_handle = AutoupdateState::handle(ctx);
        ctx.subscribe_to_model(&autoupdate_handle, |root_view, _handle, evt, ctx| {
            if let AutoupdateStateEvent::CheckComplete {
                result,
                request_type: RequestType::Poll,
            } = evt
            {
                root_view.polling_update_check_complete(result, ctx)
            }
        });

        // Ensure the onboarding view has focus after all views are created.
        // The auth_view's internal editor may have grabbed focus during construction;
        // this overrides that so keyboard input (Enter, arrow keys) routes to onboarding.
        match &root_view.auth_onboarding_state {
            AuthOnboardingState::Onboarding {
                onboarding_view, ..
            } => {
                ctx.focus(onboarding_view);
            }
            AuthOnboardingState::LocalWelcome { welcome_view, .. } => {
                ctx.focus(welcome_view);
            }
            _ => {}
        }

        root_view
    }

    /// Used for integration tests.
    pub fn workspace_view(&self) -> Option<&ViewHandle<Workspace>> {
        match &self.auth_onboarding_state {
            AuthOnboardingState::Terminal(workspace) => Some(workspace),
            _ => None,
        }
    }

    fn polling_update_check_complete(
        &mut self,
        result: &Result<UpdateReady>,
        ctx: &mut ViewContext<Self>,
    ) {
        if let Ok(UpdateReady::Yes {
            ref new_version, ..
        }) = result
        {
            log::info!("Update ready for channel version {new_version:?}");
            if new_version.update_by.is_some() {
                log::info!("Update ready, there is an update-by time, checking for server time.");
                let server_api = self.server_api.clone();
                let _ = ctx.spawn(
                    async move { server_api.server_time().await },
                    Self::server_time_updated,
                );
            }
        }
    }

    fn server_time_updated(
        &mut self,
        server_time: Result<ServerTime>,
        ctx: &mut ViewContext<Self>,
    ) {
        if let Ok(server_time) = server_time {
            let server_time = Arc::new(server_time);
            self.server_time = Some(server_time.clone());

            if let AuthOnboardingState::Terminal(workspace) = &self.auth_onboarding_state {
                workspace.update(ctx, |workspace, ctx| {
                    workspace.set_server_time(server_time);
                    ctx.notify();
                })
            }
        } else {
            log::error!("Error fetching server time {:?}", server_time.err());
        }
    }

    // Switch to Auth Screen while destroying Workspace.
    fn log_out(&mut self, _: &(), ctx: &mut ViewContext<Self>) -> bool {
        self.auth_onboarding_state.log_out(ctx);
        ctx.focus_self();
        ctx.notify();
        true
    }

    fn close_window(&mut self, _: &(), ctx: &mut ViewContext<Self>) -> bool {
        if ContextFlag::CloseWindow.is_enabled() {
            ctx.close_window();
        }
        true
    }

    fn toggle_maximize_window(&mut self, _: &(), ctx: &mut ViewContext<Self>) -> bool {
        ctx.toggle_maximized_window();
        true
    }

    fn toggle_fullscreen(&mut self, _: &(), ctx: &mut ViewContext<Self>) -> bool {
        let window_id = ctx.window_id();
        WindowManager::handle(ctx).update(ctx, |state, ctx| {
            state.toggle_fullscreen(window_id, ctx);
        });
        true
    }

    fn build_plan_yearly_price_cents(_ctx: &AppContext) -> Option<i32> {
        // Hosted pricing data is unavailable in the Dwarf fork; the onboarding
        // surfaces that consumed this value still render but show no price tag.
        None
    }

    fn create_local_welcome_view(ctx: &mut ViewContext<Self>) -> ViewHandle<LocalWelcomeView> {
        let welcome_view = ctx.add_typed_action_view(LocalWelcomeView::new);
        ctx.subscribe_to_view(&welcome_view, |me, _view, event, ctx| {
            me.handle_local_welcome_event(event, ctx);
        });
        welcome_view
    }

    fn create_agent_onboarding_view(
        ctx: &mut ViewContext<Self>,
    ) -> ViewHandle<AgentOnboardingView> {
        LLMPreferences::handle(ctx).update(ctx, |prefs, ctx| {
            prefs.refresh_available_models(ctx);
        });

        let themes = onboarding_theme_picker_themes();
        let onboarding_view = ctx.add_typed_action_view(move |ctx| {
            let (mut models, default_model_id) =
                build_onboarding_models(LLMPreferences::as_ref(ctx));
            let default_model_id =
                apply_free_tier_default_model_override(&mut models, default_model_id, ctx);

            let workspace_enforces_autonomy = UserWorkspaces::as_ref(ctx)
                .ai_autonomy_settings()
                .has_any_overrides();

            let agent_price_cents = Self::build_plan_yearly_price_cents(ctx);

            let auth_state = current_onboarding_auth_state(ctx);

            AgentOnboardingView::new(
                themes.clone(),
                false, // Always use unskippable onboarding.
                models,
                default_model_id,
                workspace_enforces_autonomy,
                FeatureFlag::AgentView.is_enabled(),
                false,
                agent_price_cents,
                auth_state,
                ctx,
            )
        });

        // Pricing updates no longer exist in the Dwarf fork.

        let onboarding_view_clone = onboarding_view.clone();
        ctx.subscribe_to_model(
            &LLMPreferences::handle(ctx),
            move |_, llm_preferences, event, ctx| match event {
                LLMPreferencesEvent::UpdatedAvailableLLMs => {
                    let (mut models, default_model_id) =
                        build_onboarding_models(llm_preferences.as_ref(ctx));
                    let default_model_id =
                        apply_free_tier_default_model_override(&mut models, default_model_id, ctx);
                    onboarding_view_clone.update(ctx, |onboarding_view, ctx| {
                        onboarding_view.set_onboarding_models(models, default_model_id, ctx);
                    })
                }

                LLMPreferencesEvent::UpdatedActiveAgentModeLLM
                | LLMPreferencesEvent::UpdatedActiveCodingLLM => {}
            },
        );

        // Subscribe to workspace changes to update autonomy enforcement state and detect upgrades.
        // TeamsChanged fires whenever the workspace/billing metadata poll returns, which is also
        // when a free→paid upgrade would be reflected (customer_type changes).
        let onboarding_view_for_workspaces = onboarding_view.clone();
        ctx.subscribe_to_model(
            &UserWorkspaces::handle(ctx),
            move |_, user_workspaces, event, ctx| {
                match event {
                    UserWorkspacesEvent::UpdateWorkspaceSettingsSuccess => {
                        let workspace_enforces_autonomy = user_workspaces
                            .as_ref(ctx)
                            .ai_autonomy_settings()
                            .has_any_overrides();
                        onboarding_view_for_workspaces.update(ctx, |onboarding_view, ctx| {
                            onboarding_view
                                .set_workspace_enforces_autonomy(workspace_enforces_autonomy, ctx);
                        });
                    }
                    UserWorkspacesEvent::TeamsChanged => {
                        let was_locked = onboarding_view_for_workspaces
                            .as_ref(ctx)
                            .free_user_no_ai_experiment(ctx);
                        if was_locked {
                            // User upgraded — skip the intention slide.
                            onboarding_view_for_workspaces.update(ctx, |view, ctx| {
                                view.set_free_user_no_ai_experiment(false, ctx);
                                view.advance_to_agent_step(ctx);
                            });
                        }
                    }
                    _ => {}
                }
                let auth_state = current_onboarding_auth_state(ctx);
                onboarding_view_for_workspaces.update(ctx, |onboarding_view, ctx| {
                    onboarding_view.set_auth_state(auth_state, ctx);
                });
            },
        );

        let onboarding_view_for_auth = onboarding_view.clone();
        ctx.subscribe_to_model(
            &AuthManager::handle(ctx),
            move |_, _auth_manager, event, ctx| {
                if matches!(
                    event,
                    AuthManagerEvent::AuthComplete | AuthManagerEvent::SkippedLogin
                ) {
                    let auth_state = current_onboarding_auth_state(ctx);
                    onboarding_view_for_auth.update(ctx, |onboarding_view, ctx| {
                        onboarding_view.set_auth_state(auth_state, ctx);
                    });
                    if matches!(event, AuthManagerEvent::AuthComplete) {
                        LLMPreferences::handle(ctx).update(ctx, |prefs, ctx| {
                            prefs.refresh_available_models(ctx);
                        });
                        TeamUpdateManager::handle(ctx).update(ctx, |manager, ctx| {
                            drop(manager.refresh_workspace_metadata(ctx));
                        });
                    }
                }
            },
        );

        ctx.subscribe_to_view(&onboarding_view, |me, _view, event, ctx| {
            me.handle_agent_onboarding_event(event, ctx);
        });
        onboarding_view
    }

    /// Debug method to enter the onboarding state.
    fn debug_enter_onboarding_state(&mut self, _: &(), ctx: &mut ViewContext<Self>) -> bool {
        if !ChannelState::enable_debug_features() {
            log::warn!("Attempted to enter onboarding state in release build");
            return false;
        }

        if !FeatureFlag::AgentOnboarding.is_enabled() {
            log::warn!("Attempted to enter onboarding state without AgentOnboarding enabled");
            return false;
        }

        self.auth_onboarding_state.try_open_onboarding_slides(ctx);

        ctx.emit(RootViewEvent::AuthOnboardingStateChanged);
        ctx.notify();
        true
    }

    fn onboarding_theme_kind(theme_name: &str) -> Option<ThemeKind> {
        WarpThemeConfig::new()
            .theme_items()
            .find_map(|(kind, theme)| {
                (theme.name().as_deref() == Some(theme_name)).then(|| kind.clone())
            })
    }

    fn handle_local_welcome_event(
        &mut self,
        event: &LocalWelcomeEvent,
        ctx: &mut ViewContext<Self>,
    ) {
        match event {
            LocalWelcomeEvent::Completed => {
                let AuthOnboardingState::LocalWelcome { target, .. } = &self.auth_onboarding_state
                else {
                    return;
                };

                let target = target.clone();
                mark_local_onboarding_completed(ctx);

                let workspace = target.to_workspace(ctx);
                self.auth_onboarding_state = AuthOnboardingState::Terminal(workspace);
                ctx.emit(RootViewEvent::AuthOnboardingStateChanged);
                self.focus(ctx);
                ctx.notify();
            }
        }
    }

    fn handle_agent_onboarding_event(
        &mut self,
        event: &AgentOnboardingEvent,
        ctx: &mut ViewContext<Self>,
    ) {
        match event {
            AgentOnboardingEvent::ThemeSelected { theme_name } => {
                let Some(theme_kind) = Self::onboarding_theme_kind(theme_name) else {
                    log::warn!("Unknown onboarding theme selected: {theme_name}");
                    return;
                };

                // Update both what we render with immediately, and the user's theme setting.
                ThemeSettings::handle(ctx).update(ctx, |settings, ctx| {
                    report_if_error!(settings.use_system_theme.set_value(false, ctx));
                    report_if_error!(settings.theme_kind.set_value(theme_kind.clone(), ctx));
                });
            }
            AgentOnboardingEvent::SyncWithOsToggled { enabled } => {
                ThemeSettings::handle(ctx).update(ctx, |settings, ctx| {
                    report_if_error!(settings.use_system_theme.set_value(*enabled, ctx));
                });
            }
            AgentOnboardingEvent::OnboardingCompleted(selected_settings) => {
                let AuthOnboardingState::Onboarding { target, .. } = &self.auth_onboarding_state
                else {
                    return;
                };
                let target = target.clone();

                mark_local_onboarding_completed(ctx);

                // Terminal-intent users should not see the conversation list
                // auto-opened for discoverability.
                if matches!(selected_settings, SelectedSettings::Terminal { .. }) {
                    AISettings::handle(ctx).update(ctx, |settings, ctx| {
                        report_if_error!(settings
                            .has_auto_opened_conversation_list
                            .set_value(true, ctx));
                    });
                }

                apply_onboarding_settings(selected_settings, ctx);

                if AuthStateProvider::as_ref(ctx).get().is_logged_in() {
                    AuthManager::handle(ctx)
                        .update(ctx, |model, ctx| model.set_user_onboarded(ctx));
                }

                let workspace = target.to_workspace(ctx);
                let tutorial = OnboardingTutorial::from(selected_settings.clone());
                self.pending_tutorial = Some(tutorial);
                self.auth_onboarding_state = AuthOnboardingState::Terminal(workspace);
                ctx.emit(RootViewEvent::AuthOnboardingStateChanged);
                self.start_pending_tutorial(ctx);
                ctx.notify();
            }
            AgentOnboardingEvent::OnboardingSkipped => {
                let AuthOnboardingState::Onboarding { target, .. } = &self.auth_onboarding_state
                else {
                    return;
                };

                mark_local_onboarding_completed(ctx);

                if AuthStateProvider::as_ref(ctx).get().is_logged_in() {
                    AuthManager::handle(ctx)
                        .update(ctx, |model, ctx| model.set_user_onboarded(ctx));
                }

                let workspace = target.to_workspace(ctx);
                self.auth_onboarding_state = AuthOnboardingState::Terminal(workspace);
                ctx.emit(RootViewEvent::AuthOnboardingStateChanged);
                ctx.notify();
            }
            AgentOnboardingEvent::UpgradeRequested => {
                let upgrade_url = AuthManager::handle(ctx)
                    .update(ctx, |auth_manager, _| auth_manager.upgrade_url());
                ctx.open_url(&upgrade_url);
            }
            AgentOnboardingEvent::UpgradeCopyUrlRequested => {
                let upgrade_url = AuthManager::handle(ctx)
                    .update(ctx, |auth_manager, _| auth_manager.upgrade_url());
                ctx.clipboard().write(ClipboardContent {
                    plain_text: upgrade_url.clone(),
                    paths: Some(vec![upgrade_url]),
                    ..Default::default()
                });
            }
            AgentOnboardingEvent::UpgradePasteTokenFromClipboardRequested => {
                // The paste-auth-token modal UI was removed. The broader AuthOnboardingState
                // refactor will replace this entrypoint in a follow-up.
            }
            AgentOnboardingEvent::PrivacySettingsFromTerminalThemeSlideRequested => {
                // The dedicated login / privacy slide was tied to the Warp Cloud
                // sign-in flow; in the local-only Dwarf fork there is no slide
                // to show.
            }
            AgentOnboardingEvent::LoginFromWelcomeRequested => {
                // No remote login flow in the local-only Dwarf fork.
            }
            AgentOnboardingEvent::AppBecameActive => {
                // fetch the models / workspace metadata when the user tabs/intents back
                // into the app during onboarding after potentially upgrading
                LLMPreferences::handle(ctx).update(ctx, |prefs, ctx| {
                    prefs.refresh_available_models(ctx);
                });
                TeamUpdateManager::handle(ctx).update(ctx, |manager, ctx| {
                    drop(manager.refresh_workspace_metadata(ctx));
                });
            }
        }
    }

    fn minimize_window(&mut self, _: &(), ctx: &mut ViewContext<Self>) -> bool {
        ctx.minimize_window();
        true
    }

    fn focus_pane(
        &mut self,
        pane_view_locator: &PaneViewLocator,
        ctx: &mut ViewContext<Self>,
    ) -> bool {
        // Focus the appropriate window.
        let window_id = ctx.window_id();

        let mut quake_mode_state = QUAKE_STATE.lock();
        // If the window we are focusing is the Quake Mode window, then update the QuakeModeState.
        if let Some(mode) = quake_mode_state.as_mut() {
            if mode.window_id == window_id {
                mode.window_state = WindowState::Open;
            }
        }

        ctx.windows().show_window_and_focus_app(window_id);

        // Focus the appropriate tab/pane.
        if let AuthOnboardingState::Terminal(workspace) = &self.auth_onboarding_state {
            workspace.update(ctx, |view, ctx| {
                view.focus_pane(*pane_view_locator, ctx);
            });
        }
        true
    }

    fn handle_notification_click(
        &mut self,
        pane_view_locator: &PaneViewLocator,
        ctx: &mut ViewContext<Self>,
    ) -> bool {
        // Focus the pane that the notification originated from.
        self.focus_pane(pane_view_locator, ctx);
        true
    }

    #[allow(clippy::ptr_arg)]
    fn add_session_at_path(&mut self, path: &PathBuf, ctx: &mut ViewContext<Self>) -> bool {
        let window_id = ctx.window_id();
        if let AuthOnboardingState::Terminal(handle) = &self.auth_onboarding_state {
            handle.update(ctx, |view, ctx| {
                view.add_tab_with_pane_layout(
                    PanesLayout::SingleTerminal(Box::new(
                        NewTerminalOptions::default()
                            .with_initial_directory_opt(path_if_directory(path).map(Into::into)),
                    )),
                    Arc::new(HashMap::new()),
                    None,
                    ctx,
                );
                ctx.windows().show_window_and_focus_app(window_id);
                ctx.notify();
            })
        } else {
            log::warn!("Auth not complete before trying to add new session at path");
        }
        true
    }

    pub fn join_shared_session_in_existing_window(
        &mut self,
        session_id: &SessionId,
        ctx: &mut ViewContext<Self>,
    ) -> bool {
        if let AuthOnboardingState::Terminal(handle) = &self.auth_onboarding_state {
            handle.update(ctx, |workspace, ctx| {
                workspace.add_tab_for_joining_shared_session(*session_id, ctx);
            });
            let window_id = ctx.window_id();
            ctx.windows().show_window_and_focus_app(window_id);
            ctx.notify();
            true
        } else {
            log::warn!("Auth not complete before trying to join shared session");
            false
        }
    }

    /// Opens a cloud conversation in an existing window.
    /// If the user owns the conversation, restores or navigates to it directly.
    /// Otherwise, opens a read-only transcript viewer.
    pub fn open_cloud_conversation_in_existing_window(
        &mut self,
        conversation_id: &ServerConversationToken,
        ctx: &mut ViewContext<Self>,
    ) -> bool {
        if let AuthOnboardingState::Terminal(handle) = &self.auth_onboarding_state {
            handle.update(ctx, |workspace, ctx| {
                workspace.open_cloud_conversation_from_server_token(conversation_id.clone(), ctx);
            });
            let window_id = ctx.window_id();
            ctx.windows().show_window_and_focus_app(window_id);
            ctx.notify();
            true
        } else {
            log::warn!("Auth not complete before trying to open conversation viewer");
            false
        }
    }

    /// Adds a tab and starts the guided `/create-environment` setup flow.
    fn create_environment_in_existing_window(
        &mut self,
        arg: &CreateEnvironmentArg,
        ctx: &mut ViewContext<Self>,
    ) -> bool {
        let _ = (arg, ctx);
        log::info!("Ignoring create-environment action; Dwarf does not expose cloud environments");
        true
    }

    /// Adds a tab and starts the guided `/create-environment` setup flow immediately.
    fn create_environment_in_existing_window_and_run(
        &mut self,
        arg: &CreateEnvironmentArg,
        ctx: &mut ViewContext<Self>,
    ) -> bool {
        let _ = (arg, ctx);
        log::info!("Ignoring create-environment action; Dwarf does not expose cloud environments");
        true
    }

    pub fn add_file_pane(&mut self, _path: &PathBuf, ctx: &mut ViewContext<Self>) -> bool {
        if let AuthOnboardingState::Terminal(_) = &self.auth_onboarding_state {
            // File-notebook tabs have been removed from this fork.
            let window_id = ctx.window_id();
            ctx.windows().show_window_and_focus_app(window_id);
            ctx.notify();
        } else {
            log::warn!("Auth not complete before trying to open file pane");
        }
        true
    }

    /// Insert a command that should create a subshell. If we support bootstrapping AKA
    /// "warpifying" its [`ShellType`], set a flag to automatically bootstrap it when the command's
    /// block receives the [`AfterBlockStarted`] event.
    pub fn insert_subshell_command_and_bootstrap_if_supported(
        &mut self,
        arg: &SubshellCommandArg,
        ctx: &mut ViewContext<Self>,
    ) -> bool {
        let window_id = ctx.window_id();
        if let AuthOnboardingState::Terminal(handle) = &self.auth_onboarding_state {
            handle.update(ctx, |workspace, ctx| {
                workspace.insert_subshell_command_and_bootstrap_if_supported(
                    &arg.command,
                    arg.shell_type,
                    ctx,
                );
                ctx.windows().show_window_and_focus_app(window_id);
            })
        } else {
            log::warn!("Auth not complete before trying to fill input");
        }
        true
    }

    /// Shows the user the settings view of their newly joined team
    /// within the app.
    pub fn handle_team_intent_link_action(&mut self, _: &(), ctx: &mut ViewContext<Self>) -> bool {
        // Dwarf Drive has been removed; just focus the window.
        let window_id = ctx.window_id();
        if let AuthOnboardingState::Terminal(_) = &self.auth_onboarding_state {
            ctx.windows().show_window_and_focus_app(window_id);
        }

        // Use the team tester model to notify relevant subscribers to refresh their data.
        TeamTesterStatus::handle(ctx).update(ctx, |model, ctx| {
            model.initiate_data_pollers(true, ctx);
        });
        true
    }

    pub fn open_settings_page_in_existing_window(
        &mut self,
        section: &SettingsSection,
        ctx: &mut ViewContext<Self>,
    ) -> bool {
        let window_id = ctx.window_id();
        if let AuthOnboardingState::Terminal(handle) = &self.auth_onboarding_state {
            ctx.dispatch_typed_action_for_view(
                window_id,
                handle.id(),
                &WorkspaceAction::ShowSettingsPage(*section),
            );
            ctx.windows().show_window_and_focus_app(window_id);
        } else {
            log::error!("Auth not complete before trying to open settings page {section:?}");
        }
        true
    }

    /// Opens the MCP servers settings page in an existing window, optionally triggering auto-install.
    /// Waits for `initial_load_complete` before opening so gallery data is available for autoinstall.
    pub fn open_mcp_settings_in_existing_window(
        &mut self,
        args: &OpenMCPSettingsArgs,
        ctx: &mut ViewContext<Self>,
    ) -> bool {
        if let AuthOnboardingState::Terminal(handle) = &self.auth_onboarding_state {
            let autoinstall = args.autoinstall.clone();
            handle.update(ctx, |workspace, ctx| {
                workspace.open_mcp_servers_page(
                    MCPServersSettingsPage::List,
                    autoinstall.as_deref(),
                    ctx,
                );
            });
            let window_id = ctx.window_id();
            ctx.windows().show_window_and_focus_app(window_id);
        } else {
            log::error!("Auth not complete before trying to open MCP settings page");
        }
        true
    }

    /// Opens the Codex modal in an existing window.
    pub fn open_codex_in_existing_window(&mut self, _: &(), ctx: &mut ViewContext<Self>) -> bool {
        let window_id = ctx.window_id();
        if let AuthOnboardingState::Terminal(handle) = &self.auth_onboarding_state {
            handle.update(ctx, |workspace, ctx| {
                workspace.open_codex_modal(ctx);
            });
            ctx.windows().show_window_and_focus_app(window_id);
        } else {
            log::error!("Auth not complete before trying to open Codex modal");
        }
        true
    }

    /// Opens a new tab with agent view for a Linear issue work deeplink.
    pub fn open_linear_issue_work_in_existing_window(
        &mut self,
        args: &LinearIssueWork,
        ctx: &mut ViewContext<Self>,
    ) -> bool {
        let window_id = ctx.window_id();
        if let AuthOnboardingState::Terminal(handle) = &self.auth_onboarding_state {
            let args = args.clone();
            handle.update(ctx, |workspace, ctx| {
                workspace.open_linear_issue_work(&args, ctx);
            });
            ctx.windows().show_window_and_focus_app(window_id);
        } else {
            log::error!("Auth not complete before trying to open Linear issue work");
        }
        true
    }

    /// Syncs the local "onboarding completed" flag to the server if the user
    /// finished onboarding pre-login and has since authenticated. Runs on every
    /// `AuthComplete`, so it also covers users who skipped login during onboarding
    /// and later signed up through a different entrypoint (e.g. login modal,
    /// settings, command palette) while already in the `Terminal` state.
    fn sync_local_onboarding_to_server(auth_state: &AuthState, ctx: &mut AppContext) {
        let is_onboarded = auth_state.is_onboarded().unwrap_or(true);
        let is_anonymous = auth_state.is_user_anonymous().unwrap_or(false);
        let has_completed_local_onboarding = has_completed_local_onboarding(ctx);

        if has_completed_local_onboarding && !is_onboarded && !is_anonymous {
            AuthManager::handle(ctx).update(ctx, |model, ctx| model.set_user_onboarded(ctx));
        }
    }

    fn handle_auth_manager_event(&mut self, event: &AuthManagerEvent, ctx: &mut ViewContext<Self>) {
        let auth_state = AuthStateProvider::as_ref(ctx).get().clone();

        match event {
            AuthManagerEvent::AuthComplete => {
                // Sync onboarding-completed state for users who finished
                // onboarding pre-login and only later signed in. This is the
                // only `AuthOnboardingState` transition that survives in the
                // local-only Dwarf fork; everything else (auth UI, SSO link,
                // web import) was tied to the removed Warp Cloud login flow.
                Self::sync_local_onboarding_to_server(&auth_state, ctx);
                self.focus(ctx);
            }
            AuthManagerEvent::AuthFailed(err) => match err {
                UserAuthenticationError::DeniedAccessToken(_) => {}
                UserAuthenticationError::UserAccountDisabled(_) => {
                    cfg_if! {
                        if #[cfg(target_family = "wasm")] {
                        } else {
                            // On native, force sign them out, as they should not be able to continue
                            // to use Warp. Instead, they can sign in or up with a valid account.
                            crate::auth::log_out(ctx);
                        }
                    }
                }
                UserAuthenticationError::Unexpected(err) => {
                    log::error!("Encountered unexpected error when trying to fetch user: {err:#}");
                }
                UserAuthenticationError::InvalidStateParameter => {}
                UserAuthenticationError::MissingStateParameter => {}
            },
            AuthManagerEvent::SkippedLogin => {
                self.focus(ctx);
            }
            AuthManagerEvent::LoginOverrideDetected(_interrupted_auth_payload) => {
                let _ = ctx;
            }
            _ => {}
        }
    }

    fn export_all_warp_drive_objects(&mut self, _ctx: &mut ViewContext<Self>) {
        // Dwarf Drive export was tied to the removed cloud_object/drive modules; this
        // method is now a no-op kept only so existing dispatch wiring still compiles.
    }

    pub fn focus(&mut self, ctx: &mut ViewContext<Self>) -> bool {
        match &self.auth_onboarding_state {
            AuthOnboardingState::Onboarding {
                onboarding_view, ..
            } => {
                ctx.focus(onboarding_view);
            }
            AuthOnboardingState::LocalWelcome { welcome_view, .. } => {
                ctx.focus(welcome_view);
            }
            AuthOnboardingState::Terminal(workspace) => {
                ctx.focus(workspace);
            }
        }
        ctx.notify();
        true
    }

    /// Stops active voice input, if the configured voice input toggle key is released.
    #[cfg(feature = "voice_input")]
    fn maybe_stop_active_voice_input(
        &mut self,
        key_code: &warpui::platform::keyboard::KeyCode,
        ctx: &mut ViewContext<Self>,
    ) -> bool {
        use crate::settings::AISettings;
        use voice_input::{VoiceInput, VoiceInputState, VoiceInputToggledFrom};
        use warpui::event::KeyState;

        // Check that the released key matches the configured voice input toggle key.
        let ai_settings = AISettings::as_ref(ctx);
        if let Some(configured_key_code) = ai_settings.voice_input_toggle_key.value().to_key_code()
        {
            if configured_key_code == *key_code {
                let voice_input = VoiceInput::handle(ctx);
                // Check if we're actively listening and it was started from a key press.
                if let VoiceInputState::Listening { enabled_from, .. } =
                    voice_input.as_ref(ctx).state()
                {
                    if matches!(
                        enabled_from,
                        VoiceInputToggledFrom::Key {
                            state: KeyState::Pressed
                        }
                    ) {
                        log::debug!("Voice input key release detected: {key_code:?}");
                        // Stop listening and proceed to transcription (don't abort).
                        voice_input.update(ctx, |voice_input, ctx| {
                            if let Err(e) = voice_input.stop_listening(ctx) {
                                log::error!("Failed to stop voice input on key release: {e:?}");
                            }
                        });
                    }
                }
            }
        }
        true
    }

    /// If onboarding stored a pending tutorial (because login was required first),
    /// start it now that the workspace exists.
    fn start_pending_tutorial(&mut self, ctx: &mut ViewContext<Self>) {
        let Some(tutorial) = self.pending_tutorial.take() else {
            return;
        };

        let AuthOnboardingState::Terminal(workspace) = &self.auth_onboarding_state else {
            return;
        };

        if FeatureFlag::OpenWarpNewSettingsModes.is_enabled()
            && FeatureFlag::TabConfigs.is_enabled()
        {
            let intention = tutorial.intention();
            // Terminal-intent users skip the session config modal.
            if matches!(intention, OnboardingIntention::AgentDrivenDevelopment) {
                workspace.update(ctx, |view, ctx| {
                    view.set_pending_onboarding_intention(intention);
                    view.open_vertical_tabs_panel_if_enabled(ctx);
                    view.show_session_config_modal(ctx);
                });
            } else {
                workspace.update(ctx, |view, ctx| {
                    view.open_vertical_tabs_panel_if_enabled(ctx);
                });
            }
        } else if *AISettings::as_ref(ctx).is_any_ai_enabled {
            workspace.update(ctx, |view, ctx| {
                view.start_agent_onboarding_tutorial(tutorial, ctx);
            });
        }
    }

    fn traffic_light_data(&self, ctx: &AppContext) -> Option<TrafficLightData> {
        // The workspace view will handle rendering of the traffic lights (so
        // that they can be hidden when the tab bar is hidden).
        if matches!(self.auth_onboarding_state, AuthOnboardingState::Terminal(_)) {
            return None;
        }

        traffic_light_data(ctx, self.window_id)
    }
}

#[derive(Clone, Debug)]
pub enum RootViewEvent {
    AuthOnboardingStateChanged,
}

impl Entity for RootView {
    type Event = RootViewEvent;
}

impl View for RootView {
    fn ui_name() -> &'static str {
        "RootView"
    }

    fn on_focus(&mut self, focus_ctx: &FocusContext, ctx: &mut ViewContext<Self>) {
        if focus_ctx.is_self_focused() {
            self.focus(ctx);
        } else if matches!(
            self.auth_onboarding_state,
            AuthOnboardingState::Onboarding { .. } | AuthOnboardingState::LocalWelcome { .. }
        ) {
            // During onboarding, aggressively redirect focus.
            // This ensures keystrokes (Enter) are handled by the correct view rather
            // than something hidden like the input editor.
            self.focus(ctx);
        }
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let child = match &self.auth_onboarding_state {
            AuthOnboardingState::Onboarding {
                onboarding_view, ..
            } => ChildView::new(onboarding_view).finish(),
            AuthOnboardingState::LocalWelcome { welcome_view, .. } => {
                ChildView::new(welcome_view).finish()
            }
            AuthOnboardingState::Terminal(workspace) => ChildView::new(workspace).finish(),
        };

        let mut stack = Stack::new();
        stack.add_child(child);

        if let Some(run) = self
            .confetti_run
            .filter(|run| run.started_at.elapsed() < DwarfConfettiElement::duration_for(run.preset))
        {
            stack.add_positioned_overlay_child(
                DwarfConfettiElement::new(run).finish(),
                OffsetPositioning::offset_from_parent(
                    vec2f(0., 0.),
                    ParentOffsetBounds::ParentBySize,
                    ParentAnchor::TopLeft,
                    ChildAnchor::TopLeft,
                ),
            );
        }

        if let Some(traffic_light_data) = self.traffic_light_data(app) {
            let theme = Appearance::as_ref(app).theme();
            let fullscreen_state = app
                .windows()
                .platform_window(self.window_id)
                .map(|window| window.fullscreen_state())
                .unwrap_or_default();
            stack.add_positioned_child(
                traffic_light_data.render(fullscreen_state, &self.mouse_states, theme, app),
                OffsetPositioning::offset_from_parent(
                    vec2f(0., 0.),
                    ParentOffsetBounds::WindowByPosition,
                    ParentAnchor::TopRight,
                    ChildAnchor::TopRight,
                ),
            );
        }

        cfg_if::cfg_if! {
            if #[cfg(feature = "voice_input")] {
                use warpui::elements::{EventHandler, DispatchEventResult};
                EventHandler::new(stack.finish())
                    .on_modifier_state_changed(|ctx, _app, key_code, key_state| {
                        if matches!(key_state, warpui::event::KeyState::Released) {
                            ctx.dispatch_action("root_view:maybe_stop_active_voice_input", *key_code);
                        }
                        DispatchEventResult::PropagateToParent
                    })
                    .finish()
            } else {
                stack.finish()
            }
        }
    }

    fn keymap_context(&self, app: &AppContext) -> warpui::keymap::Context {
        let mut context = Self::default_keymap_context();
        if quake_mode_window_is_open() {
            context.set.insert(flags::QUAKE_WINDOW_OPEN_FLAG);
        }
        if *KeysSettings::as_ref(app).quake_mode_enabled {
            context.set.insert(flags::QUAKE_MODE_ENABLED_CONTEXT_FLAG);
        }
        if *KeysSettings::as_ref(app).activation_hotkey_enabled.value() {
            context.set.insert(flags::ACTIVATION_HOTKEY_FLAG);
        }
        context
    }
}

#[derive(Clone, Debug)]
pub enum RootViewAction {
    ToggleQuakeModeWindow,
    ShowOrHideNonQuakeModeWindows,
    ToggleFullscreen,
    DebugEnterOnboardingState,
    ShowConfetti(DwarfConfettiPreset),
}

impl TypedActionView for RootView {
    type Action = RootViewAction;
    fn handle_action(&mut self, action: &RootViewAction, ctx: &mut ViewContext<Self>) {
        match action {
            RootViewAction::ToggleQuakeModeWindow => {
                let global_resource_handles =
                    GlobalResourceHandlesProvider::as_ref(ctx).get().clone();
                toggle_quake_mode_window(&global_resource_handles, ctx)
            }
            RootViewAction::ShowOrHideNonQuakeModeWindows => {
                show_or_hide_non_quake_mode_windows(&(), ctx)
            }
            RootViewAction::ToggleFullscreen => {
                let window_id = ctx.window_id();
                WindowManager::handle(ctx).update(ctx, |state, ctx| {
                    state.toggle_fullscreen(window_id, ctx);
                });
            }
            RootViewAction::DebugEnterOnboardingState => {
                self.debug_enter_onboarding_state(&(), ctx);
            }
            RootViewAction::ShowConfetti(preset) => {
                self.confetti_run = Some(DwarfConfettiRun {
                    started_at: instant::Instant::now(),
                    preset: *preset,
                });
                ctx.notify();
            }
        }
    }
}

impl WorkspaceArgs {
    fn create_workspace(self, ctx: &mut ViewContext<RootView>) -> ViewHandle<Workspace> {
        ctx.add_typed_action_view(|ctx| {
            Workspace::new(
                self.global_resource_handles,
                self.server_time,
                self.workspace_setting,
                ctx,
            )
        })
    }
}

impl AuthOnboardingState {
    fn complete_auth_and_create_workspace(&mut self, ctx: &mut ViewContext<RootView>) {
        // Check if we should show onboarding (only for users who are not yet onboarded).
        // The server-side `is_onboarded` flag is synced separately by
        // `RootView::sync_local_onboarding_to_server`, which runs on every `AuthComplete`
        // before we get here.
        let auth_state = AuthStateProvider::as_ref(ctx).get();
        let is_onboarded = auth_state.is_onboarded().unwrap_or(true);
        let is_anonymous = auth_state.is_user_anonymous().unwrap_or(false);

        let has_completed_local_onboarding = has_completed_local_onboarding(ctx);

        if !local_agent_terminal_only()
            && !is_onboarded
            && !is_anonymous
            && !has_completed_local_onboarding
            && FeatureFlag::AgentOnboarding.is_enabled()
        {
            self.try_open_onboarding_slides(ctx);
        }

        ctx.emit(RootViewEvent::AuthOnboardingStateChanged);
    }

    fn try_open_onboarding_slides(&mut self, ctx: &mut ViewContext<RootView>) {
        let target = match self {
            AuthOnboardingState::Terminal(workspace) => {
                AuthOnboardingTarget::Terminal(workspace.clone())
            }
            _ => {
                // Onboarding slides can only be opened from Terminal state in the local-only fork.
                return;
            }
        };

        let onboarding_view = RootView::create_agent_onboarding_view(ctx);
        onboarding_view.update(ctx, |view, ctx| {
            view.start_onboarding(ctx);
        });
        *self = AuthOnboardingState::Onboarding {
            onboarding_view,
            target,
        };
    }

    fn log_out(&mut self, ctx: &mut ViewContext<RootView>) {
        // Local-only fork has no Warp Cloud account; logout just refreshes the workspace.
        if let AuthOnboardingState::Terminal(workspace) = self {
            workspace.update(ctx, |workspace, ctx| {
                workspace.on_log_out(ctx);
            });
        }
    }
}

impl AuthOnboardingTarget {
    fn to_workspace(&self, ctx: &mut ViewContext<RootView>) -> ViewHandle<Workspace> {
        match self {
            AuthOnboardingTarget::Terminal(workspace) => workspace.clone(),
            AuthOnboardingTarget::Workspace(args) => args.clone().create_workspace(ctx),
        }
    }
}

#[cfg(test)]
#[path = "root_view_tests.rs"]
mod tests;
