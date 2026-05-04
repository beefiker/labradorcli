use super::DriveSortOrder;
use settings::{
    macros::define_settings_group, RespectUserSyncSetting, SupportedPlatforms, SyncToCloud,
};

pub const HAS_AUTO_OPENED_WELCOME_FOLDER: &str = "HasAutoOpenedWelcomeFolder";

define_settings_group!(WarpDriveSettings, settings: [
    sorting_choice: WarpDriveSortingChoice {
        type: DriveSortOrder,
        default: DriveSortOrder::ByObjectType,
        supported_platforms: SupportedPlatforms::ALL,
        sync_to_cloud: SyncToCloud::Globally(RespectUserSyncSetting::Yes),
        private: false,
        toml_path: "warp_drive.sorting_choice",
        description: "The sort order for items in Dwarf Drive.",
    },
    sharing_onboarding_block_shown: WarpDriveSharingOnboardingBlockShown {
        type: bool,
        default: false,
        supported_platforms: SupportedPlatforms::ALL,
        sync_to_cloud: SyncToCloud::Globally(RespectUserSyncSetting::Yes),
        private: true,
    },
    // Controls whether Dwarf Drive appears in the tools panel, command palette, and command search.
    enable_warp_drive: EnableWarpDrive {
        type: bool,
        default: true,
        supported_platforms: SupportedPlatforms::ALL,
        sync_to_cloud: SyncToCloud::Globally(RespectUserSyncSetting::Yes),
        private: false,
        toml_path: "warp_drive.enabled",
        description: "Whether Dwarf Drive is enabled.",
    },
]);

impl WarpDriveSettings {
    /// Dwarf Drive remains disabled in local-only builds because it still depends on hosted
    /// Warp collaboration/storage surfaces.
    pub fn is_warp_drive_enabled(app: &warpui::AppContext) -> bool {
        let _ = app;
        false
    }
}
