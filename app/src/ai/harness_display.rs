//! Shared display metadata for [`Harness`] variants.
//!
//! Any UI surface that shows a harness to the user — the harness selector
//! dropdown, the conversation details sidebar, etc. — should source its label,
//! icon, and brand color from here so the two surfaces cannot drift.

use pathfinder_color::ColorU;
use labrador_cli::agent::Harness;

use crate::ai::agent::conversation::AIAgentHarness;
use crate::ai::blocklist::CLAUDE_ORANGE;
use crate::terminal::cli_agent::OPENAI_COLOR;
use crate::ui_components::icons::Icon;

/// User-visible display name for a [`Harness`].
pub fn display_name(harness: Harness) -> &'static str {
    match harness {
        Harness::Claude => "Claude Code",
        Harness::OpenCode => "OpenCode",
        Harness::Codex => "Codex",
        Harness::Unknown => "Unknown",
    }
}

/// Leading icon for a [`Harness`].
pub fn icon_for(harness: Harness) -> Icon {
    match harness {
        Harness::Claude => Icon::ClaudeLogo,
        Harness::OpenCode => Icon::OpenCodeLogo,
        Harness::Codex => Icon::OpenAILogo,
        Harness::Unknown => Icon::HelpCircle,
    }
}

/// Brand tint for a [`Harness`]'s icon. `None` means "use the surface's
/// default foreground color".
pub fn brand_color(harness: Harness) -> Option<ColorU> {
    match harness {
        Harness::Claude => Some(CLAUDE_ORANGE),
        Harness::OpenCode => None,
        Harness::Codex => Some(OPENAI_COLOR),
        Harness::Unknown => None,
    }
}

/// Map [`AIAgentHarness`] (from `ServerAIConversationMetadata`) to the
/// canonical [`Harness`]. Server-only `Oz` and `Gemini` conversations surface
/// as `Unknown` since labrador no longer drives them.
impl From<AIAgentHarness> for Harness {
    fn from(harness: AIAgentHarness) -> Self {
        match harness {
            AIAgentHarness::ClaudeCode => Harness::Claude,
            AIAgentHarness::Codex => Harness::Codex,
            AIAgentHarness::Oz | AIAgentHarness::Gemini | AIAgentHarness::Unknown => {
                Harness::Unknown
            }
        }
    }
}

impl PartialEq<Harness> for AIAgentHarness {
    fn eq(&self, other: &Harness) -> bool {
        Harness::from(*self) == *other
    }
}
