#![allow(clippy::unwrap_used, clippy::expect_used)]
fn main() {
    divan::main();
}

// ── Config parsing ──────────────────────────────────────────────────────────

/// Benchmark generating the default TOML config template.
#[divan::bench]
fn config_template_generation() -> String {
    divan::black_box(chelix_config::template::default_config_template(8080))
}

/// Benchmark constructing a `ChelixConfig` with all defaults.
#[divan::bench]
fn config_default_construction() -> chelix_config::ChelixConfig {
    divan::black_box(chelix_config::ChelixConfig::default())
}

/// Benchmark loading + parsing a TOML config from disk (the full boot path).
#[divan::bench]
fn config_load_toml(bencher: divan::Bencher) {
    let toml_content = chelix_config::template::default_config_template(8080);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("chelix.toml");
    std::fs::write(&path, &toml_content).unwrap();

    bencher.bench_local(|| divan::black_box(chelix_config::loader::load_config(&path).unwrap()));
}

/// Benchmark config round-trip: serialize ChelixConfig to TOML, then deserialize.
#[divan::bench]
fn config_serde_roundtrip() {
    let config = chelix_config::ChelixConfig::default();
    let toml_str = divan::black_box(toml::to_string_pretty(&config).unwrap());
    let _: chelix_config::ChelixConfig = divan::black_box(toml::from_str(&toml_str).unwrap());
}

/// Benchmark validating a TOML config string (schema checks, semantic warnings).
#[divan::bench]
fn config_validate_toml(bencher: divan::Bencher) {
    let toml_content = chelix_config::template::default_config_template(8080);

    bencher.bench_local(|| {
        divan::black_box(chelix_config::validate::validate_toml_str(&toml_content))
    });
}

// ── Provider model metadata ─────────────────────────────────────────────────

fn benchmark_model_metadata() -> chelix_common::ModelMetadata {
    chelix_common::PartialModelMetadata {
        context_length: Some(400_000),
        max_input_tokens: Some(272_000),
        max_output_tokens: Some(128_000),
        reasoning: Some(chelix_common::PartialReasoningMetadata {
            supported_efforts: Some(Vec::new()),
            ..Default::default()
        }),
        ..Default::default()
    }
    .resolve()
    .expect("benchmark metadata must be complete")
}

#[divan::bench]
fn context_window_lookup(bencher: divan::Bencher) {
    let metadata = benchmark_model_metadata();
    bencher.bench_local(|| divan::black_box(metadata.context_length));
}

#[divan::bench]
fn vision_support_lookup(bencher: divan::Bencher) {
    let metadata = benchmark_model_metadata();
    bencher.bench_local(|| {
        divan::black_box(metadata.supports_input(chelix_common::ModelModality::Image))
    });
}

#[divan::bench]
fn namespaced_model_id() -> String {
    divan::black_box(chelix_providers::model_id::namespaced_model_id(
        "openai", "gpt-4o",
    ))
}

// ── Session store ───────────────────────────────────────────────────────────

const SESSION_KEYS: &[&str] = &[
    "default",
    "project:backend:debug-auth",
    "2026-02-09T12:00:00Z",
    "user@host:session:42",
];

#[divan::bench(args = SESSION_KEYS)]
fn session_key_to_filename(key: &str) -> String {
    divan::black_box(chelix_sessions::store::SessionStore::key_to_filename(key))
}

fn build_sanitize_input(payload_bytes: usize) -> String {
    let image_blob = "A".repeat(payload_bytes);
    let hex_blob = "deadbeef".repeat(payload_bytes / 8);
    format!("before data:image/png;base64,{image_blob} middle {hex_blob} after")
}

#[divan::bench(args = [10_000, 100_000, 1_000_000])]
fn sanitize_tool_result(bencher: divan::Bencher, payload_bytes: usize) {
    let input = build_sanitize_input(payload_bytes);
    bencher.bench_local(|| divan::black_box(chelix_agents::runner::sanitize_tool_result(&input)));
}

fn build_persisted_messages(n: usize) -> Vec<serde_json::Value> {
    let mut values = Vec::with_capacity(n + 1);
    values.push(serde_json::json!({
        "role": "system",
        "content": "You are a helpful assistant."
    }));

    for i in 0..n {
        match i % 6 {
            0 => values.push(serde_json::json!({
                "role": "user",
                "content": format!("How do I fix issue #{i}?"),
            })),
            1 => values.push(serde_json::json!({
                "role": "assistant",
                "content": format!("Try step {}", i % 5),
            })),
            2 => values.push(serde_json::json!({
                "role": "assistant",
                "content": format!("Calling tool {i}"),
                "tool_calls": [{
                    "id": format!("tool_{i}"),
                    "type": "function",
                    "function": {
                        "name": "web.search",
                        "arguments": r#"{"q":"chelix release notes"}"#,
                    }
                }],
            })),
            3 => values.push(serde_json::json!({
                "role": "tool",
                "tool_call_id": format!("tool_{i}"),
                "content": {"ok": true, "items": i},
            })),
            4 => values.push(serde_json::json!({
                "role": "user",
                "content": [
                    {"type": "text", "text": "Please inspect this screenshot"},
                    {
                        "type": "image_url",
                        "image_url": {"url": "data:image/png;base64,AAAA"},
                    }
                ],
            })),
            _ => values.push(serde_json::json!({
                "role": "tool_result",
                "tool_call_id": format!("tool_{i}"),
                "content": {"success": true},
            })),
        }
    }

    values
}

#[divan::bench(args = [50, 500, 2000])]
fn values_to_chat_messages(bencher: divan::Bencher, n: usize) {
    let values = build_persisted_messages(n);
    bencher
        .bench_local(|| divan::black_box(chelix_agents::model::values_to_chat_messages(&values)));
}

// ── Env substitution ────────────────────────────────────────────────────────

#[divan::bench]
fn env_substitution(bencher: divan::Bencher) {
    let input = r#"
        api_key = "${CHELIX_API_KEY}"
        base_url = "${CHELIX_BASE_URL:-https://api.example.com}"
        name = "no-vars-here"
        port = 8080
    "#;

    bencher.bench_local(|| divan::black_box(chelix_config::env_subst::substitute_env(input)));
}

// ── Config load from disk (simulated boot) ──────────────────────────────────

/// Full boot-path simulation: generate template, write to disk, load, validate.
#[divan::bench]
fn full_config_boot_path(bencher: divan::Bencher) {
    let toml_content = chelix_config::template::default_config_template(8080);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("chelix.toml");
    std::fs::write(&path, &toml_content).unwrap();

    bencher.bench_local(|| {
        let config = chelix_config::loader::load_config(&path).unwrap();
        let _ = chelix_config::validate::validate_toml_str(&toml_content);
        divan::black_box(config)
    });
}
