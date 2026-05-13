use settings::{
    macros::define_settings_group, RespectUserSyncSetting, SupportedPlatforms, SyncToCloud,
};
define_settings_group!(CloudPreferencesSettings, settings: [
   settings_sync_enabled: IsSettingsSyncEnabled {
       type: bool,
       default: false,
       supported_platforms: SupportedPlatforms::ALL,
       sync_to_cloud: SyncToCloud::Globally(RespectUserSyncSetting::No),
       private: false,
       toml_path: "account.is_settings_sync_enabled",
       description: "Whether settings are synced across devices via the cloud.",
   },
]);
