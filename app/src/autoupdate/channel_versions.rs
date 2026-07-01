use std::{fs::read_to_string, sync::Arc};

use anyhow::{Context as _, Result};
use channel_versions::ChannelVersions;

use crate::{
    channel::ChannelState,
    server::server_api::{ServerApi, FETCH_CHANNEL_VERSIONS_TIMEOUT},
};

// Fetches channel versions asynchronously.
//
// This fork hosts its release manifest (`channel_versions.json`) directly on
// GitHub Releases and does not operate the upstream Labrador `client_version`
// server, so we skip that server hop entirely and read the manifest from the
// configured releases base URL. (The `include_changelogs`/`is_daily` arguments
// are retained for API compatibility with the upstream signature but are not
// used here.)
pub async fn fetch_channel_versions(
    nonce: &str,
    server_api: Arc<ServerApi>,
    _include_changelogs: bool,
    _is_daily: bool,
) -> Result<ChannelVersions> {
    if let Ok(path) = std::env::var("LABRADOR_CHANNEL_VERSIONS_PATH") {
        // Load channel versions from local filesystem. Used for testing both
        // autoupdate and changelog behavior.
        let path = shellexpand::tilde(&path);
        let channel_versions_string = read_to_string::<&str>(&path)?;
        return serde_json::from_str(channel_versions_string.as_str())
            .context("Failed to parse channel versions JSON");
    }

    fetch_channel_versions_from_json_storage(server_api.http_client(), nonce).await
}

// Synchronously fetches updated Labrador [`ChannelVersions`] from GCP JSON storage. This will soon
// be deprecated in favor of retrieving updated channel versions from the Labrador Server.
// Note, in order to run against a test file you can use the "channel_versions_test.json" file
// and upload it to the configured releases bucket as "channel_versions_test.json".
async fn fetch_channel_versions_from_json_storage(
    client: &http_client::Client,
    nonce: &str,
) -> Result<ChannelVersions> {
    log::info!("Fetching channel versions from GCP JSON storage");
    let res = client
        .get(
            format!(
                "{}/channel_versions.json?r={}",
                ChannelState::releases_base_url(),
                nonce
            )
            .as_str(),
        )
        .timeout(FETCH_CHANNEL_VERSIONS_TIMEOUT)
        .send()
        .await?;
    let versions: ChannelVersions = res.json().await?;
    log::info!("Received channel versions from GCP JSON storage: {versions}");
    Ok(versions)
}
