use serde::{Deserialize, Serialize};
use labrador_core::{features::FeatureFlag, settings::Setting};
use labrador_util::path::ShellFamily;

use crate::terminal::labradorify::settings::LabradorifySettings;

/// The different possible outcomes of detecting an interactive SSH session.
/// Also the payload for the [`crate::server::telemetry::TelemetryEvent::SshInteractiveSessionDetected`] event.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum SshInteractiveSessionDetected {
    #[serde(rename = "feature_disabled")]
    FeatureDisabled,
    #[serde(rename = "host_denylisted")]
    HostDenylisted,
    #[serde(rename = "labradorify_prompt")]
    ShouldPromptLabradorification {
        #[serde(skip)]
        command: String,
        #[serde(skip)]
        host: Option<String>,
    },
}

/// Determines whether a host could be Labradorified.
pub fn evaluate_labradorify_ssh_host(
    command: &str,
    ssh_host: Option<&str>,
    shell_family: ShellFamily,
    labradorify_settings: &LabradorifySettings,
) -> SshInteractiveSessionDetected {
    let should_prompt_ssh_tmux_wrapper = *labradorify_settings.enable_ssh_labradorification.value()
        && *labradorify_settings.use_ssh_tmux_wrapper.value();
    let matches_subshell = labradorify_settings.is_denylisted_subshell_command(command)
        || labradorify_settings.is_compatible_subshell_command(command, shell_family);
    if !should_prompt_ssh_tmux_wrapper
        || matches_subshell
        || !FeatureFlag::SSHTmuxWrapper.is_enabled()
    {
        return SshInteractiveSessionDetected::FeatureDisabled;
    }

    if let Some(ssh_host) = ssh_host {
        if labradorify_settings.is_ssh_host_denylisted(ssh_host) {
            return SshInteractiveSessionDetected::HostDenylisted;
        }
    }

    SshInteractiveSessionDetected::ShouldPromptLabradorification {
        host: ssh_host.map(|host| host.to_owned()),
        command: command.to_string(),
    }
}
