//! No-op telemetry-sender macros for the labrador fork.
//!
//! Upstream Labrador shipped a Rudderstack/Amplitude analytics pipeline that fired
//! HTTP requests to Labrador's hosted servers whenever any of these macros expanded.
//! labrador is a local-only CLI terminal and must not phone home, so each macro is
//! redefined here as `{}` — Rust parses the argument expressions but never
//! evaluates them, so existing call sites compile but emit nothing at runtime.

/// No-op: was previously a synchronous Rudderstack `track` send from a
/// `ViewContext`/`ModelContext`.
#[macro_export]
macro_rules! send_telemetry_sync_from_ctx {
    ($event:expr, $ctx:expr) => {{}};
}

/// No-op: was previously a synchronous Rudderstack `track` send from an
/// `AppContext`.
#[macro_export]
macro_rules! send_telemetry_sync_from_app_ctx {
    ($event:expr, $app_ctx:expr) => {{}};
}

/// No-op: was previously an asynchronous Rudderstack `track` send dispatched on
/// a background executor when no app context was available.
#[macro_export]
macro_rules! send_telemetry_on_executor {
    ($auth_state:expr, $event:expr, $executor:expr) => {{}};
}
