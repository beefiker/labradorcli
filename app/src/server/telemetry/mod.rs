mod collector;
mod context;
pub mod context_provider;
mod events;
mod macros;
pub mod rudder_message;
pub mod secret_redaction;

pub use collector::*;
pub use context::telemetry_context;
pub use events::*;

use std::path::PathBuf;

/// Filename for file where telemetry events are written on app quit.
const RUDDER_TELEMETRY_EVENTS_FILE_NAME: &str = "rudder_telemetry_events.json";

/// Filepath where the Rudder events should be written on app quit.
fn rudder_event_file_path() -> PathBuf {
    warp_core::paths::secure_state_dir()
        .unwrap_or_else(warp_core::paths::state_dir)
        .join(RUDDER_TELEMETRY_EVENTS_FILE_NAME)
}

/// Removes all telemetry events from the app telemetry event queue.
pub fn clear_event_queue() {
    let _ = warpui::telemetry::flush_events();
}
