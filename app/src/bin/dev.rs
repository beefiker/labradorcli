// On Windows, we don't want to display a console window when the application is running in release
// builds. See https://doc.rust-lang.org/reference/runtime.html#the-windows_subsystem-attribute.
#![cfg_attr(feature = "release_bundle", windows_subsystem = "windows")]

use anyhow::Result;
use labrador_core::{
    channel::{Channel, ChannelConfig, ChannelState, OzConfig, LabradorServerConfig},
    features, AppId,
};

// Simple wrapper around labrador::run() for dev channel builds.
fn main() -> Result<()> {
    ChannelState::set(
        ChannelState::new(
            Channel::Dev,
            ChannelConfig {
                app_id: AppId::new(
                    "dev",
                    labrador_core::channel::APP_ID_ORGANIZATION,
                    ChannelState::app_id_application_name(Channel::Dev),
                ),
                logfile_name: format!("{}.log", ChannelState::app_name()).into(),
                server_config: LabradorServerConfig::production(),
                oz_config: OzConfig::production(),
                telemetry_config: None,
                crash_reporting_config: None,
                autoupdate_config: None,
                mcp_static_config: None,
            },
        )
        .with_additional_features(features::DEBUG_FLAGS)
        .with_additional_features(features::DOGFOOD_FLAGS)
        .with_additional_features(features::PREVIEW_FLAGS),
    );

    labrador::run()
}
