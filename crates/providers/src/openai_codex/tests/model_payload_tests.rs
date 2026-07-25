use super::super::*;

#[test]
fn parse_models_payload_from_models_array() {
    let value = serde_json::json!({
        "models": [
            {"id": "gpt-5.3", "name": "GPT-5.3"},
            {"id": "gpt-5.2-codex", "display_name": "GPT-5.2 Codex"}
        ]
    });
    let models = parse_models_payload(&value);
    assert_eq!(models.len(), 2);
    assert_eq!(models[0].id, "gpt-5.3");
    assert_eq!(models[0].display_name, "GPT-5.3");
    assert_eq!(models[1].id, "gpt-5.2-codex");
}

#[test]
fn parse_models_payload_from_nested_data_array() {
    let value = serde_json::json!({
        "data": {
            "items": [
                {"slug": "gpt-5.3-codex"},
                {"model": "gpt-5.1-codex-mini", "title": "GPT-5.1 Codex Mini"}
            ]
        }
    });
    let models = parse_models_payload(&value);
    assert_eq!(models.len(), 2);
    assert_eq!(models[0].id, "gpt-5.3-codex");
    assert_eq!(models[0].display_name, "gpt-5.3-codex");
    assert_eq!(models[1].id, "gpt-5.1-codex-mini");
}

#[test]
fn parse_models_payload_ignores_empty_ids_and_dedupes_without_id_heuristics() {
    let value = serde_json::json!({
        "models": [
            {"id": "gpt-5.3"},
            {"id": "gpt-5.3", "name": "Duplicate"},
            {"id": "this has spaces"},
            {"id": ""}
        ]
    });
    let models = parse_models_payload(&value);
    assert_eq!(models.len(), 2);
    assert_eq!(models[0].id, "gpt-5.3");
    assert_eq!(models[1].id, "this has spaces");
}

#[test]
fn parse_models_payload_keeps_non_codex_and_codex_variants() {
    let value = serde_json::json!({
        "models": [
            {"id": "gpt-5.3", "name": "GPT-5.3"},
            {"id": "gpt-5.3-codex", "name": "GPT-5.3 Codex"}
        ]
    });
    let models = parse_models_payload(&value);
    assert_eq!(models.len(), 2);
    assert_eq!(models[0].id, "gpt-5.3");
    assert_eq!(models[1].id, "gpt-5.3-codex");
}
