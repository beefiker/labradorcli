use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context;
use futures::future::join_all;
use futures::TryStreamExt as _;
use tokio::fs;
use tokio_util::io::StreamReader;
use warp_core::features::FeatureFlag;

use crate::ai::agent_sdk::retry::with_bounded_retry;
use crate::ai::ambient_agents::task::TaskAttachment;
use crate::server::server_api::ai::AIClient;
use crate::server::server_api::presigned_upload::HttpStatusError;
use crate::server::server_api::ServerApi;

/// Fetches task attachments via GraphQL and downloads them to the filesystem.
/// Returns the attachments directory path if any attachments were downloaded,
/// so the caller can pass it to the server via `StartFromAmbientRunPrompt`.
///
/// `attachments_dir` is the per-session directory where files should be downloaded.
///
/// Makes a best-effort attempt to download all attachments.
/// Individual download failures are logged but don't cause the entire function to fail.
pub(crate) async fn fetch_and_download_attachments(
    ai_client: Arc<dyn AIClient>,
    http_client: Arc<ServerApi>,
    task_id: String,
    attachments_dir: PathBuf,
) -> anyhow::Result<Option<String>> {
    if !FeatureFlag::AmbientAgentsImageUpload.is_enabled() {
        return Ok(None);
    }

    let attachments = ai_client
        .get_task_attachments(task_id.clone())
        .await
        .context("Failed to fetch task attachments")?;

    log::info!("Fetched {} task attachments", attachments.len());

    if attachments.is_empty() {
        return Ok(None);
    }

    download_and_write_attachments(attachments, &attachments_dir, &http_client).await?;

    Ok(Some(attachments_dir.to_string_lossy().into_owned()))
}


/// Downloads task attachments from presigned URLs and writes them to the filesystem.
/// Downloads are performed concurrently using `join_all`.
/// Makes a best-effort attempt to download all attachments, logging warnings for failures.
/// The filename is already formatted by the server with UUID prefix (e.g., "uuid_filename.png").
async fn download_and_write_attachments(
    attachments: Vec<TaskAttachment>,
    attachment_dir: &Path,
    http_client: &ServerApi,
) -> anyhow::Result<()> {
    fs::create_dir_all(attachment_dir)
        .await
        .context("Failed to create attachments directory")?;
    log::info!(
        "Created attachments directory at: {}",
        attachment_dir.display()
    );

    let http = http_client.http_client();
    let download_futures = attachments
        .into_iter()
        .map(|attachment| download_task_attachment(attachment, attachment_dir, http));
    let results = join_all(download_futures).await;

    let mut successful = 0;
    let mut failed = 0;
    for result in results {
        match result {
            Ok(()) => successful += 1,
            Err(_) => failed += 1,
        }
    }

    log::info!("Attachment download complete: {successful} successful, {failed} failed");

    Ok(())
}

/// Download a single task attachment into `attachment_dir/<sanitized filename>`.
///
/// Delegates to [`download_attachment`] so transient failures retry on the shared schedule.
async fn download_task_attachment(
    attachment: TaskAttachment,
    attachment_dir: &Path,
    http_client: &http_client::Client,
) -> anyhow::Result<()> {
    let safe_filename = Path::new(&attachment.filename)
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| anyhow::anyhow!("Invalid filename for file_id={}", attachment.file_id))?
        .to_string();

    let file_path = attachment_dir.join(&safe_filename);
    log::info!(
        "Downloading attachment: {} -> {}",
        attachment.filename,
        file_path.display()
    );

    download_attachment(http_client, &attachment.download_url, &file_path).await?;

    log::info!("Successfully wrote attachment to: {}", file_path.display());
    Ok(())
}


/// Shared download primitive: GET `download_url`, write the body to `file_path`, and retry
/// transient HTTP failures on the shared bounded-backoff schedule. Non-2xx responses surface
/// an [`HttpStatusError`] so the retry classifier can decide whether to retry.
async fn download_attachment(
    http_client: &http_client::Client,
    download_url: &str,
    file_path: &Path,
) -> anyhow::Result<()> {
    let operation = format!("download attachment '{}'", file_path.display());
    with_bounded_retry(&operation, || async {
        let response = http_client
            .get(download_url)
            .send()
            .await
            .context("Failed to send download request")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(anyhow::Error::new(HttpStatusError {
                status: status.as_u16(),
                body: body.clone(),
            })
            .context(format!("Download failed with status {status}: {body}")));
        }

        // Stream the response body directly to disk instead of buffering the full payload
        // in memory.
        let mut file = fs::File::create(file_path)
            .await
            .context("Failed to create file")?;
        let mut response_stream =
            StreamReader::new(response.bytes_stream().map_err(std::io::Error::other));
        tokio::io::copy(&mut response_stream, &mut file)
            .await
            .context("Failed to write file")?;

        Ok(())
    })
    .await
}

