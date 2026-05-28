//! Task attachment download stub. The cloud-hosted task subsystem and its
//! `TaskAttachment` graph type have been removed; callers in the agent SDK
//! retain the entry point but it now no-ops.

use std::path::PathBuf;
use std::sync::Arc;

use crate::server::server_api::ai::AIClient;
use crate::server::server_api::ServerApi;

/// Always returns `Ok(None)` since cloud-hosted task attachments are no longer
/// fetched from the server.
pub(crate) async fn fetch_and_download_attachments(
    _ai_client: Arc<dyn AIClient>,
    _http_client: Arc<ServerApi>,
    _task_id: String,
    _attachments_dir: PathBuf,
) -> anyhow::Result<Option<String>> {
    Ok(None)
}
