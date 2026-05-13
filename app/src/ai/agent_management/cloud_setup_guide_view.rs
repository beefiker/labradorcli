use serde::Serialize;

/// Which URL the user clicked in the setup guide (also used in telemetry)
#[derive(Clone, Copy, Debug, Serialize)]
pub enum SetupGuideDocs {
    Main,
    Environment,
    Integration,
}
