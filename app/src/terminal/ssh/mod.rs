use std::time::Duration;

pub mod error;
pub mod install_tmux;
pub mod root_access;
pub mod ssh_detection;
pub mod util;
pub mod labradorify;

pub const SSH_LABRADORIFY_TIMEOUT_DURATION: Duration = Duration::from_secs(8);
