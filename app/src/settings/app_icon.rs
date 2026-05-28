use enum_iterator::Sequence;
use serde::{Deserialize, Serialize};
use labrador_core::{
    channel::{Channel, ChannelState},
    settings::{macros::define_settings_group, SupportedPlatforms, SyncToCloud},
};

/// The app icon to use (mac-only).
///
/// IMPORTANT NOTE: If you add a new icon, you will need to update the logic in LabradorDockTilePlugin.m
/// to read the new icon and also add the icon to app/DockTilePlugin/Resources.
#[derive(
    Default,
    Debug,
    Clone,
    Copy,
    PartialEq,
    Serialize,
    Deserialize,
    Sequence,
    schemars::JsonSchema,
    settings_value::SettingsValue,
)]
#[schemars(
    description = "The app icon displayed in the dock.",
    rename_all = "snake_case"
)]
pub enum AppIcon {
    /// Current default: White glyph on blue/black gradient blackground, set in Dec 2024.
    #[default]
    #[schemars(description = "Default")]
    Default,
    #[schemars(description = "Aurora")]
    Aurora,
    #[schemars(description = "Classic 1")]
    Classic1,
    #[schemars(description = "Classic 2")]
    Classic2,
    #[schemars(description = "Classic 3")]
    Classic3,
    #[schemars(description = "Comets")]
    Comets,
    /// Cow icon, for Code on Labrador launch.
    #[schemars(description = "Cow")]
    Cow,
    #[schemars(description = "Glass Sky")]
    GlassSky,
    #[schemars(description = "Glitch")]
    Glitch,
    /// White glyph on black background with blue/green glow on the side, set in Oct 2024 brand refresh.
    #[schemars(description = "Glow")]
    Glow,
    #[schemars(description = "Holographic")]
    Holographic,
    #[schemars(description = "Mono")]
    Mono,
    #[schemars(description = "Neon")]
    Neon,
    /// Blue/green glyph on black background.
    #[schemars(description = "Original")]
    Original,
    #[schemars(description = "Starburst")]
    Starburst,
    #[schemars(description = "Sticker")]
    Sticker,
    /// Previous default icon with solid blue background.
    #[schemars(description = "App 1")]
    LabradorOne,
}

impl std::fmt::Display for AppIcon {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = match &self {
            AppIcon::Default => "Default".to_string(),
            AppIcon::Aurora => "Aurora".to_string(),
            AppIcon::Classic1 => "Classic 1".to_string(),
            AppIcon::Classic2 => "Classic 2".to_string(),
            AppIcon::Classic3 => "Classic 3".to_string(),
            AppIcon::Comets => "Comets".to_string(),
            AppIcon::GlassSky => "Glass Sky".to_string(),
            AppIcon::Glitch => "Glitch".to_string(),
            AppIcon::Cow => "Cow".to_string(),
            AppIcon::Glow => "Glow".to_string(),
            AppIcon::Holographic => "Holographic".to_string(),
            AppIcon::Mono => "Mono".to_string(),
            AppIcon::Neon => "Neon".to_string(),
            AppIcon::Original => "Original".to_string(),
            AppIcon::Starburst => "Starburst".to_string(),
            AppIcon::Sticker => "Sticker".to_string(),
            AppIcon::LabradorOne => format!("{} 1", ChannelState::app_name_display()),
        };
        write!(f, "{value}")
    }
}

impl AppIconSettings {
    pub fn get_base_icon_file_name(icon: AppIcon) -> &'static str {
        match icon {
            AppIcon::Aurora => "aurora",
            AppIcon::Default => match ChannelState::channel() {
                Channel::Dev => "dev",
                Channel::Preview => "preview",
                Channel::Local => "local",
                _ => "labrador_2",
            },
            AppIcon::Classic1 => "classic_1",
            AppIcon::Classic2 => "classic_2",
            AppIcon::Classic3 => "classic_3",
            AppIcon::Comets => "comets",
            AppIcon::GlassSky => "glass_sky",
            AppIcon::Glitch => "glitch",
            AppIcon::Cow => "cow",
            AppIcon::Glow => "glow",
            AppIcon::Holographic => "holographic",
            AppIcon::Mono => "mono",
            AppIcon::Neon => "neon",
            AppIcon::Original => "original",
            AppIcon::Starburst => "starburst",
            AppIcon::Sticker => "sticker",
            AppIcon::LabradorOne => "blue",
        }
    }
}

define_settings_group!(AppIconSettings, settings: [
    app_icon: AppIconState {
        type: AppIcon,
        default: AppIcon::Default,
        supported_platforms: SupportedPlatforms::MAC,
        sync_to_cloud: SyncToCloud::Never,
        private: false,
        storage_key: "AppIcon",
        toml_path: "appearance.icon.app_icon",
        description: "The app icon displayed in the dock.",
    },
]);
