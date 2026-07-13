use {super::registration::openai_builtin_capabilities, crate::openai::ResponsesWebSocketPolicy};

#[test]
fn openai_default_base_url_enables_responses_websocket() {
    assert_eq!(
        openai_builtin_capabilities(false).responses_websocket_policy,
        ResponsesWebSocketPolicy::OpenAiPlatform,
    );
}

#[test]
fn openai_custom_base_url_disables_responses_websocket() {
    assert_eq!(
        openai_builtin_capabilities(true).responses_websocket_policy,
        ResponsesWebSocketPolicy::Unsupported,
    );
}
