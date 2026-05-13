//! Stub: anonymous user tracking removed. The fork has no Warp Cloud account
//! concept, so callers that previously fetched an anonymous user id now get a
//! stable local-only stub.

use uuid::Uuid;
use warpui::AppContext;

pub fn get_or_create_anonymous_id(_app: &AppContext) -> Uuid {
    Uuid::nil()
}
