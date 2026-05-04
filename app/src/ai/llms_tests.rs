use super::*;

#[test]
fn llm_info_deserializes_without_base_model_name() {
    let raw = r#"{
            "display_name": "gpt-4o",
            "id": "gpt-4o",
            "usage_metadata": {
                "request_multiplier": 1,
                "credit_multiplier": null
            },
            "description": null,
            "disable_reason": null,
            "vision_supported": false,
            "spec": null,
            "provider": "Unknown"
        }"#;

    let info: LLMInfo = serde_json::from_str(raw).expect("should deserialize");
    assert_eq!(info.display_name, "gpt-4o");
    assert_eq!(info.base_model_name, "gpt-4o");
}

#[test]
fn llm_info_deserializes_host_configs_as_vec() {
    // Wire format from server: host_configs is a Vec
    let raw = r#"{
            "display_name": "gpt-4o",
            "id": "gpt-4o",
            "usage_metadata": { "request_multiplier": 1, "credit_multiplier": null },
            "provider": "OpenAI",
            "host_configs": [
                { "enabled": true, "model_routing_host": "DirectApi" },
                { "enabled": false, "model_routing_host": "AwsBedrock" }
            ]
        }"#;

    let info: LLMInfo = serde_json::from_str(raw).expect("should deserialize vec format");
    assert_eq!(info.display_name, "gpt-4o");
    assert_eq!(info.host_configs.len(), 2);
    assert!(
        info.host_configs
            .get(&LLMModelHost::DirectApi)
            .unwrap()
            .enabled
    );
    assert!(
        !info
            .host_configs
            .get(&LLMModelHost::AwsBedrock)
            .unwrap()
            .enabled
    );
}

#[test]
fn llm_info_round_trip_serializes_and_deserializes() {
    // Start with wire format (Vec)
    let wire_json = r#"{
            "display_name": "claude-3",
            "base_model_name": "claude-3",
            "id": "claude-3",
            "usage_metadata": { "request_multiplier": 2, "credit_multiplier": 1.5 },
            "description": "A powerful model",
            "vision_supported": true,
            "provider": "Anthropic",
            "host_configs": [
                { "enabled": true, "model_routing_host": "DirectApi" }
            ]
        }"#;

    // Deserialize from wire format
    let info: LLMInfo = serde_json::from_str(wire_json).expect("should deserialize");

    // Serialize (produces HashMap format)
    let serialized = serde_json::to_string(&info).expect("should serialize");

    // Deserialize again (from HashMap format)
    let round_tripped: LLMInfo =
        serde_json::from_str(&serialized).expect("should deserialize after round trip");

    assert_eq!(info, round_tripped);
}

#[test]
fn default_agent_models_are_local_codex_models() {
    let models = ModelsByFeature::default();
    let choices = &models.agent_mode.choices;

    assert_eq!(models.agent_mode.default_llm_info().display_name, "gpt-5.5");
    assert_eq!(choices.len(), LOCAL_CODEX_MODEL_IDS.len() + 1);

    for model_id in LOCAL_CODEX_MODEL_IDS {
        let info = models
            .agent_mode
            .info_for_id(&LLMId::from(*model_id))
            .expect("local Codex model should be available");
        assert_eq!(info.display_name, *model_id);
        assert_eq!(info.provider, LLMProvider::OpenAI);
    }
}

#[test]
fn local_codex_agent_models_replace_server_agent_models() {
    let mut models = ModelsByFeature {
        agent_mode: AvailableLLMs {
            default_id: "server-model".to_owned().into(),
            choices: vec![LLMInfo {
                display_name: "server-model".to_owned(),
                base_model_name: "server-model".to_owned(),
                id: "server-model".to_owned().into(),
                reasoning_level: None,
                usage_metadata: LLMUsageMetadata {
                    request_multiplier: 1,
                    credit_multiplier: None,
                },
                description: None,
                disable_reason: None,
                vision_supported: false,
                spec: None,
                provider: LLMProvider::Unknown,
                host_configs: Default::default(),
                discount_percentage: None,
                context_window: LLMContextWindow::default(),
            }],
            preferred_codex_model_id: None,
        },
        ..Default::default()
    };

    use_local_codex_agent_models(&mut models);

    assert!(models
        .agent_mode
        .info_for_id(&LLMId::from("server-model"))
        .is_none());
    assert!(models
        .agent_mode
        .info_for_id(&LLMId::from("gpt-5.5"))
        .is_some());
}
