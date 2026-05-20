use dirs::home_dir;

use super::*;

#[test]
fn test_data_dir_path() {
    let home_dir = home_dir().expect("Should be able to compute home directory");
    // ChannelState, by default, is configured for Channel::Oss.
    cfg_if::cfg_if! {
        if #[cfg(target_os = "macos")] {
            assert_eq!(data_dir(), home_dir.join(format!(".{}", ChannelState::app_name())));
        } else if #[cfg(target_os = "linux")] {
            assert_eq!(data_dir(), home_dir.join(format!(".local/share/{}", ChannelState::app_name_display())));
        } else if #[cfg(windows)] {
            assert_eq!(data_dir(), home_dir.join(format!("AppData\\Roaming\\{}\\{}\\data", crate::channel::APP_ID_ORGANIZATION, ChannelState::app_name_display())));
        } else {
            unimplemented!("Need to update tests for current platform!");
        }
    }
}

#[test]
fn test_config_local_dir_path() {
    let home_dir = home_dir().expect("Should be able to compute home directory");
    // ChannelState, by default, is configured for Channel::Oss.
    cfg_if::cfg_if! {
        if #[cfg(target_os = "macos")] {
            assert_eq!(config_local_dir(), home_dir.join(format!(".{}", ChannelState::app_name())));
        } else if #[cfg(target_os = "linux")] {
            assert_eq!(config_local_dir(), home_dir.join(format!(".config/{}", ChannelState::app_name_display())));
        } else if #[cfg(windows)] {
            assert_eq!(config_local_dir(), home_dir.join(format!("AppData\\Local\\{}\\{}\\config", crate::channel::APP_ID_ORGANIZATION, ChannelState::app_name_display())));
        } else {
            unimplemented!("Need to update tests for current platform!");
        }
    }
}

#[test]
fn test_warp_home_config_dir_path() {
    let home_dir = home_dir().expect("Should be able to compute home directory");
    let expected_dir_name = match ChannelState::data_profile() {
        Some(data_profile) => format!(".{}-{data_profile}", ChannelState::app_name()),
        None => format!(".{}", ChannelState::app_name()),
    };

    assert_eq!(
        warp_home_config_dir(),
        Some(home_dir.join(expected_dir_name))
    );
}

#[test]
fn test_warp_home_skills_and_mcp_paths() {
    let Some(config_dir) = warp_home_config_dir() else {
        panic!("Should be able to compute Warp home config directory");
    };

    assert_eq!(warp_home_skills_dir(), Some(config_dir.join("skills")));
    assert_eq!(
        warp_home_mcp_config_file_path(),
        Some(config_dir.join(".mcp.json"))
    );
}
#[test]
fn test_cache_dir_path() {
    let home_dir = home_dir().expect("Should be able to compute home directory");
    // ChannelState, by default, is configured for Channel::Oss.
    cfg_if::cfg_if! {
        if #[cfg(target_os = "macos")] {
            assert_eq!(cache_dir(), home_dir.join(format!("Library/Application Support/dev.{}.{}", crate::channel::APP_ID_ORGANIZATION, ChannelState::app_name_display())));
        } else if #[cfg(target_os = "linux")] {
            assert_eq!(cache_dir(), home_dir.join(format!(".cache/{}", ChannelState::app_name_display())));
        } else if #[cfg(windows)] {
            assert_eq!(cache_dir(), home_dir.join(format!("AppData\\Local\\{}\\{}\\cache", crate::channel::APP_ID_ORGANIZATION, ChannelState::app_name_display())));
        } else {
            unimplemented!("Need to update tests for current platform!");
        }
    }
}

#[test]
fn test_state_dir_path() {
    let home_dir = home_dir().expect("Should be able to compute home directory");
    cfg_if::cfg_if! {
        // ChannelState, by default, is configured for Channel::Oss.
        if #[cfg(target_os = "macos")] {
            assert_eq!(state_dir(), home_dir.join(format!("Library/Application Support/dev.{}.{}", crate::channel::APP_ID_ORGANIZATION, ChannelState::app_name_display())));
        } else if #[cfg(target_os = "linux")] {
            assert_eq!(state_dir(), home_dir.join(format!(".local/state/{}", ChannelState::app_name_display())));
        } else if #[cfg(windows)] {
            assert_eq!(state_dir(), home_dir.join(format!("AppData\\Local\\{}\\{}\\data", crate::channel::APP_ID_ORGANIZATION, ChannelState::app_name_display())));
        } else {
            unimplemented!("Need to update tests for current platform!");
        }
    }
}

#[test]
fn test_project_path_for_app_id() {
    let project_dirs = project_dirs_for_app_id(
        AppId::new(
            "dev",
            crate::channel::APP_ID_ORGANIZATION,
            ChannelState::app_name_display(),
        ),
        None,
    )
    .expect("should be able to compute project dirs");
    cfg_if::cfg_if! {
        if #[cfg(target_os = "macos")] {
            let expected = format!("dev.{}.{}", crate::channel::APP_ID_ORGANIZATION, ChannelState::app_name_display());
            assert_eq!(project_dirs.project_path(), std::path::Path::new(&expected));
        } else if #[cfg(target_os = "linux")] {
            let expected = format!("{}-Terminal", ChannelState::app_name_display());
            assert_eq!(project_dirs.project_path(), std::path::Path::new(&expected));
        } else if #[cfg(windows)] {
            let expected = format!("{}\\{}", crate::channel::APP_ID_ORGANIZATION, ChannelState::app_name_display());
            assert_eq!(project_dirs.project_path(), std::path::Path::new(&expected));
        } else {
            unimplemented!("Need to update tests for current platform!");
        }
    }
}

#[test]
fn test_project_path_for_dev_app_id() {
    let dev_name = ChannelState::app_id_application_name(crate::channel::Channel::Dev);
    let project_dirs = project_dirs_for_app_id(
        AppId::new("dev", crate::channel::APP_ID_ORGANIZATION, dev_name.clone()),
        None,
    )
    .expect("should be able to compute project dirs");
    cfg_if::cfg_if! {
        if #[cfg(target_os = "macos")] {
            let expected = format!("dev.{}.{}", crate::channel::APP_ID_ORGANIZATION, dev_name);
            assert_eq!(project_dirs.project_path(), std::path::Path::new(&expected));
        } else if #[cfg(target_os = "linux")] {
            let expected = format!("{}-Terminal-Dev", ChannelState::app_name_display());
            assert_eq!(project_dirs.project_path(), std::path::Path::new(&expected));
        } else if #[cfg(windows)] {
            let expected = format!("{}\\{}", crate::channel::APP_ID_ORGANIZATION, dev_name);
            assert_eq!(project_dirs.project_path(), std::path::Path::new(&expected));
        } else {
            unimplemented!("Need to update tests for current platform!");
        }
    }
}

#[test]
fn test_project_path_for_labrador_app_id() {
    let project_dirs = project_dirs_for_app_id(
        AppId::new(
            "dev",
            crate::channel::APP_ID_ORGANIZATION,
            ChannelState::app_name_display(),
        ),
        None,
    )
    .expect("should be able to compute project dirs");
    cfg_if::cfg_if! {
        if #[cfg(target_os = "macos")] {
            let expected = format!("dev.{}.{}", crate::channel::APP_ID_ORGANIZATION, ChannelState::app_name_display());
            assert_eq!(project_dirs.project_path(), std::path::Path::new(&expected));
        } else if #[cfg(target_os = "linux")] {
            let expected = format!("{}-Terminal", ChannelState::app_name_display());
            assert_eq!(project_dirs.project_path(), std::path::Path::new(&expected));
        } else if #[cfg(windows)] {
            let expected = format!("{}\\{}", crate::channel::APP_ID_ORGANIZATION, ChannelState::app_name_display());
            assert_eq!(project_dirs.project_path(), std::path::Path::new(&expected));
        } else {
            unimplemented!("Need to update tests for current platform!");
        }
    }
}
