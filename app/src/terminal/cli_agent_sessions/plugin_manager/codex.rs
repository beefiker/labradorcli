use std::sync::LazyLock;

use async_trait::async_trait;
use labrador_core::channel::ChannelState;

use super::{CliAgentPluginManager, PluginInstructionStep, PluginInstructions};

pub(super) struct CodexPluginManager;

#[async_trait]
impl CliAgentPluginManager for CodexPluginManager {
    fn minimum_plugin_version(&self) -> &'static str {
        "0.0.0"
    }

    fn can_auto_install(&self) -> bool {
        false
    }

    fn supports_update(&self) -> bool {
        false
    }

    fn install_instructions(&self) -> &'static PluginInstructions {
        &INSTALL_INSTRUCTIONS
    }

    fn update_instructions(&self) -> &'static PluginInstructions {
        &EMPTY_INSTRUCTIONS
    }
}

static INSTALL_INSTRUCTIONS: LazyLock<PluginInstructions> = LazyLock::new(|| {
    PluginInstructions {
        title: format!(
            "Enable {} Notifications for Codex",
            ChannelState::app_name_display()
        ),
        subtitle: format!(
            "Update Codex to the latest version, then enable in-focus notifications so {} can display them while you work.",
            ChannelState::app_name_display()
        ),
        steps: &[
            PluginInstructionStep {
                description: "Update Codex to the latest version.",
                command: "",
                executable: false,
                link: Some("https://developers.openai.com/codex/cli#upgrade"),
            },
            PluginInstructionStep {
                description: "Set the notification condition to \"always\" in your Codex config. Open or create ~/.codex/config.toml and add:",
                command: "[tui]\nnotification_condition = \"always\"",
                executable: false,
                link: None,
            },
        ],
        post_install_notes: &["Restart Codex to apply the changes."],
    }
});

static EMPTY_INSTRUCTIONS: LazyLock<PluginInstructions> = LazyLock::new(|| PluginInstructions {
    title: String::new(),
    subtitle: String::new(),
    steps: &[],
    post_install_notes: &[],
});

#[cfg(test)]
#[path = "codex_tests.rs"]
mod tests;
