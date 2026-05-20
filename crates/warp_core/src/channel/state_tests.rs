use super::{derive_http_origin_from_ws_url, Channel, ChannelState};

#[test]
fn app_name_is_shared_labrador_value() {
    assert_eq!(ChannelState::app_name(), "labrador");
    assert_eq!(ChannelState::app_name_display(), "Labrador");
    assert_eq!(ChannelState::app_name_with_suffix("AI"), "Labrador AI");
    assert_eq!(ChannelState::app_name_ai(), "Labrador AI");
    assert_eq!(ChannelState::app_name_agent(), "Labrador Agent");
    assert_eq!(ChannelState::app_name_cli(), "Labrador CLI");
    assert_eq!(ChannelState::app_name_drive(), "Labrador Drive");
    assert_eq!(ChannelState::app_name_api_key(), "Labrador API Key");
    assert!(!crate::channel::APP_CLI_ABOUT.is_empty());
    assert_eq!(ChannelState::app_name_possessive(), "Labrador's");
    assert_eq!(ChannelState::app_name_verbify(), "Labradorify");
    assert_eq!(ChannelState::app_name_verbification(), "Labradorification");
    assert_eq!(ChannelState::app_name_verbifying(), "Labradorifying");
    assert_eq!(ChannelState::app_name_verbed(), "Labradorified");
    assert_eq!(ChannelState::app_name_gerund(), "Labradoring");
    assert_eq!(
        ChannelState::app_id_application_name(Channel::Local),
        "LabradorLocal"
    );
}

#[test]
fn wss_becomes_https_and_strips_path() {
    let got = derive_http_origin_from_ws_url("wss://rtc.app.warp.dev/graphql/v2");
    assert_eq!(got.as_deref(), Some("https://rtc.app.warp.dev"));
}

#[test]
fn ws_becomes_http_and_preserves_port() {
    let got = derive_http_origin_from_ws_url("ws://localhost:8080/graphql/v2");
    assert_eq!(got.as_deref(), Some("http://localhost:8080"));
}

#[test]
fn unparseable_input_returns_none() {
    assert!(derive_http_origin_from_ws_url("not a url").is_none());
    assert!(derive_http_origin_from_ws_url("https://app.warp.dev").is_none());
}
