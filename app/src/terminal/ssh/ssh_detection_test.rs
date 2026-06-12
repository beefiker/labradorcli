use regex::Regex;
use settings::Setting;

use super::*;
use crate::terminal::labradorify::settings::{
    AddedSubshellCommands, AutoLabradorifySsh, EnableSshLabradorification,
    SshExtensionInstallModeSetting, SshHostsDenylist, SubshellCommandsDenylist, UseSshTmuxWrapper,
};

fn labradorify_settings(
    enable_ssh_labradorification: bool,
    auto_labradorify_ssh: bool,
    use_ssh_tmux_wrapper: bool,
    ssh_hosts_denylist: Vec<String>,
) -> LabradorifySettings {
    let parsed_ssh_hosts_denylist = ssh_hosts_denylist
        .iter()
        .map(|pattern| Regex::new(pattern))
        .collect();
    LabradorifySettings {
        added_subshell_commands: AddedSubshellCommands::new(None),
        parsed_added_subshell_commands: Vec::new(),
        subshell_command_denylist: SubshellCommandsDenylist::new(None),
        parsed_subshell_command_denylist: Vec::new(),
        ssh_hosts_denylist: SshHostsDenylist::new(Some(ssh_hosts_denylist)),
        parsed_ssh_hosts_denylist,
        enable_ssh_labradorification: EnableSshLabradorification::new(Some(
            enable_ssh_labradorification,
        )),
        use_ssh_tmux_wrapper: UseSshTmuxWrapper::new(Some(use_ssh_tmux_wrapper)),
        auto_labradorify_ssh: AutoLabradorifySsh::new(Some(auto_labradorify_ssh)),
        ssh_extension_install_mode: SshExtensionInstallModeSetting::new(None),
    }
}

#[test]
fn test_auto_labradorify_disabled_by_default() {
    let settings = labradorify_settings(true, false, false, Vec::new());
    assert!(!should_auto_labradorify_ssh_host(
        Some("example.com"),
        &settings
    ));
}

#[test]
fn test_auto_labradorify_enabled() {
    let settings = labradorify_settings(true, true, false, Vec::new());
    assert!(should_auto_labradorify_ssh_host(
        Some("example.com"),
        &settings
    ));
}

#[test]
fn test_auto_labradorify_requires_master_switch() {
    let settings = labradorify_settings(false, true, false, Vec::new());
    assert!(!should_auto_labradorify_ssh_host(
        Some("example.com"),
        &settings
    ));
}

#[test]
fn test_auto_labradorify_disabled_with_tmux_wrapper() {
    let settings = labradorify_settings(true, true, true, Vec::new());
    assert!(!should_auto_labradorify_ssh_host(
        Some("example.com"),
        &settings
    ));
}

#[test]
fn test_auto_labradorify_respects_host_denylist() {
    let settings = labradorify_settings(true, true, false, vec!["example\\.com".to_owned()]);
    assert!(!should_auto_labradorify_ssh_host(
        Some("example.com"),
        &settings
    ));
    assert!(should_auto_labradorify_ssh_host(
        Some("other-host.com"),
        &settings
    ));
}

#[test]
fn test_auto_labradorify_requires_known_host() {
    let settings = labradorify_settings(true, true, false, Vec::new());
    assert!(!should_auto_labradorify_ssh_host(None, &settings));
}
