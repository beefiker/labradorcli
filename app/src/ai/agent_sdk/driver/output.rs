//! Stdout formatters for the agent driver run lifecycle.
//!
//! Only the run-started and shared-session-established events are still surfaced
//! from the driver; everything else moved to local output paths when the Oz
//! dispatch was dropped.

use std::io::{self, BufWriter, Write};

use warp_core::channel::ChannelState;

pub mod text {
    use std::io::{self, Write};

    /// Report the run ID with a link to the Dwarf dashboard.
    pub fn run_started<W: Write>(run_id: &str, w: &mut W) -> io::Result<()> {
        let run_url = super::run_url(run_id);
        writeln!(w, "Run ID: {run_id}")?;
        writeln!(w, "Open in Dwarf: {run_url}\n")
    }

    /// Report that a shared session has been established.
    pub fn shared_session_established<W: Write>(join_url: &str, w: &mut W) -> io::Result<()> {
        writeln!(w, "Sharing session at: {join_url}")
    }
}

pub mod json {
    use serde::Serialize;
    use std::io::{self, Write};

    #[derive(Serialize)]
    #[serde(tag = "type", rename_all = "snake_case")]
    enum JsonSystemEvent<'a> {
        RunStarted {
            run_id: &'a str,
            run_url: &'a str,
        },
        SharedSessionEstablished {
            join_url: &'a str,
        },
    }

    fn write_event<W: Write>(event: &JsonSystemEvent<'_>, w: &mut W) -> io::Result<()> {
        serde_json::to_writer(&mut *w, event)?;
        writeln!(w)
    }

    /// Write a run_started system event to stdout.
    pub fn run_started<W: Write>(run_id: &str, w: &mut W) -> io::Result<()> {
        let run_url = super::run_url(run_id);
        write_event(
            &JsonSystemEvent::RunStarted {
                run_id,
                run_url: &run_url,
            },
            w,
        )
    }

    /// Write a shared_session_established system event to stdout.
    pub fn shared_session_established<W: Write>(join_url: &str, w: &mut W) -> io::Result<()> {
        write_event(&JsonSystemEvent::SharedSessionEstablished { join_url }, w)
    }
}

/// Constructs the dashboard URL for a given run ID.
fn run_url(run_id: &str) -> String {
    let oz_root_url = ChannelState::oz_root_url();
    format!("{oz_root_url}/runs/{run_id}")
}

/// Execute a closure with a buffered stdout writer and flush it afterwards.
pub fn with_stdout_buffered<F>(f: F) -> io::Result<()>
where
    F: FnOnce(&mut BufWriter<io::StdoutLock>) -> io::Result<()>,
{
    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut buf = BufWriter::new(handle);
    f(&mut buf)?;
    buf.flush()
}
