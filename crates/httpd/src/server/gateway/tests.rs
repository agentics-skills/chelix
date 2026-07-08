use axum::http::{HeaderMap, HeaderValue};

use super::telephony_webhook_url;

#[test]
fn telephony_webhook_url_builds_absolute_url_from_forwarded_headers() {
    let mut headers = HeaderMap::new();
    headers.insert(
        "x-forwarded-host",
        HeaderValue::from_static("calls.example.com"),
    );
    headers.insert("x-forwarded-proto", HeaderValue::from_static("https"));

    let url = telephony_webhook_url(
        "default",
        "gather",
        &headers,
        None,
        &chelix_config::schema::ChelixConfig::default(),
    )
    .unwrap_or_default();

    assert_eq!(
        url,
        "https://calls.example.com/api/channels/telephony/default/gather"
    );
}

#[test]
fn telephony_webhook_url_prefers_account_webhook_base() {
    let account_config = serde_json::json!({
        "webhook_url": "https://phone.example.com/base/",
    });

    let url = telephony_webhook_url(
        "default",
        "answer",
        &HeaderMap::new(),
        Some(account_config),
        &chelix_config::schema::ChelixConfig::default(),
    )
    .unwrap_or_default();

    assert_eq!(
        url,
        "https://phone.example.com/base/api/channels/telephony/default/answer"
    );
}
