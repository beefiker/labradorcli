// On Windows, we don't want to display a console window when the application is running in release
// builds. See https://doc.rust-lang.org/reference/runtime.html#the-windows_subsystem-attribute.
#![cfg_attr(feature = "release_bundle", windows_subsystem = "windows")]

use anyhow::Result;
use labrador_core::{
    channel::{Channel, ChannelConfig, ChannelState, OzConfig, LabradorServerConfig},
    AppId,
};

// Simple wrapper around labrador::run() for Labrador OSS builds.
fn main() -> Result<()> {
    let mut state = ChannelState::new(
        Channel::Oss,
        ChannelConfig {
            app_id: AppId::new(
                "dev",
                labrador_core::channel::APP_ID_ORGANIZATION,
                ChannelState::app_name_display(),
            ),
            logfile_name: format!("{}.log", ChannelState::app_name()).into(),
            server_config: LabradorServerConfig::production(),
            oz_config: OzConfig::production(),
            telemetry_config: None,
            crash_reporting_config: None,
            autoupdate_config: None,
            mcp_static_config: None,
        },
    );
    // Enable the in-pane git ops (commit / push / Create-PR) in debug builds.
    // For release bundles this comes through RELEASE_FLAGS automatically.
    state = state
        .with_additional_features(&[labrador_core::features::FeatureFlag::GitOperationsInCodeReview]);
    if cfg!(debug_assertions) {
        state = state.with_additional_features(labrador_core::features::DEBUG_FLAGS);
    }
    ChannelState::set(state);

    labrador::run()
}

// If we're not using an external plist, embed the following as the Info.plist.
#[cfg(all(not(feature = "extern_plist"), target_os = "macos"))]
const OSS_INFO_PLIST_PARTS: [&[u8]; 13] = [
    br#"
    <?xml version="1.0" encoding="UTF-8"?>
    <!DOCTYPE plist PUBLIC "-//Apple Computer//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
    <plist version="1.0">
    <dict>
    <key>CFBundleDevelopmentRegion</key>
    <string>English</string>
    <key>CFBundleDisplayName</key>
    <string>"#,
    labrador_core::channel::APP_NAME_DISPLAY.as_bytes(),
    br#"</string>
    <key>CFBundleExecutable</key>
    <string>"#,
    labrador_core::channel::APP_NAME.as_bytes(),
    br#"</string>
    <key>CFBundleIdentifier</key>
    <string>dev.labrador."#,
    labrador_core::channel::APP_NAME_DISPLAY.as_bytes(),
    br#"</string>
    <key>CFBundleInfoDictionaryVersion</key>
    <string>6.0</string>
    <key>CFBundleName</key>
    <string>"#,
    labrador_core::channel::APP_NAME_DISPLAY.as_bytes(),
    br#"</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>0.1.0</string>
    <key>LSApplicationCategoryType</key>
    <string>public.app-category.developer-tools</string>
    <key>NSHighResolutionCapable</key>
    <true/>
    <key>UIDesignRequiresCompatibility</key>
    <true/>
    <key>CFBundleURLTypes</key>
    <array><dict><key>CFBundleURLName</key><string>"#,
    labrador_core::channel::APP_NAME_DISPLAY.as_bytes(),
    br#"</string><key>CFBundleURLSchemes</key><array><string>"#,
    labrador_core::channel::APP_NAME.as_bytes(),
    r#"</string></array></dict></array>
    <key>NSHumanReadableCopyright</key>
    <string>© 2026, Denver Technologies, Inc</string>
    </dict>
    </plist>
"#
    .as_bytes(),
];

#[cfg(all(not(feature = "extern_plist"), target_os = "macos"))]
const fn plist_parts_len(parts: &[&[u8]]) -> usize {
    let mut total = 0;
    let mut index = 0;
    while index < parts.len() {
        total += parts[index].len();
        index += 1;
    }
    total
}

#[cfg(all(not(feature = "extern_plist"), target_os = "macos"))]
const OSS_INFO_PLIST_LEN: usize = plist_parts_len(&OSS_INFO_PLIST_PARTS);

#[cfg(all(not(feature = "extern_plist"), target_os = "macos"))]
const fn write_plist_part<const N: usize>(
    mut out: [u8; N],
    mut offset: usize,
    bytes: &[u8],
) -> ([u8; N], usize) {
    let mut index = 0;
    while index < bytes.len() {
        out[offset] = bytes[index];
        offset += 1;
        index += 1;
    }
    (out, offset)
}

#[cfg(all(not(feature = "extern_plist"), target_os = "macos"))]
const fn build_oss_info_plist<const N: usize>(parts: &[&[u8]]) -> [u8; N] {
    let mut out = [0; N];
    let mut offset = 0;
    let mut index = 0;
    while index < parts.len() {
        let written = write_plist_part(out, offset, parts[index]);
        out = written.0;
        offset = written.1;
        index += 1;
    }
    out
}

#[cfg(all(not(feature = "extern_plist"), target_os = "macos"))]
const OSS_INFO_PLIST: [u8; OSS_INFO_PLIST_LEN] = build_oss_info_plist(&OSS_INFO_PLIST_PARTS);

#[cfg(all(not(feature = "extern_plist"), target_os = "macos"))]
embed_plist::embed_info_plist_bytes!(&OSS_INFO_PLIST);
