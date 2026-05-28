use std::path::PathBuf;

pub const AGENT_CONFIG_FILE_ENV: &str = "LABRADOR_AGENT_CONFIG_FILE";

/// Shared CLI args for loading command configuration from a file.
#[derive(Debug, Default, Clone, clap::Args)]
pub struct ConfigFileArgs {
    /// Path to a YAML or JSON configuration file.
    #[arg(
        short = 'f',
        long = "file",
        value_name = "PATH",
        env = "LABRADOR_AGENT_CONFIG_FILE"
    )]
    pub file: Option<PathBuf>,
}

impl ConfigFileArgs {
    pub fn file(&self) -> Option<&std::path::Path> {
        self.file.as_deref()
    }
}
