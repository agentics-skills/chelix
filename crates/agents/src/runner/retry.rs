//! Provider error classification and retry delay logic.

/// Error patterns that indicate a transient server error worth retrying.
const RETRYABLE_SERVER_PATTERNS: &[&str] = &[
    "http 500",
    "http 502",
    "http 503",
    "http 504",
    "http 529",
    "server_error",
    "internal server error",
    "overloaded",
    "bad gateway",
    "service unavailable",
    "gateway timeout",
    "temporarily unavailable",
    "the server had an error processing your request",
    "timeout",
    "connection reset",
];

/// Check if an error looks like a transient provider failure that may
/// succeed on retry (5xx, overloaded, etc.).
pub(crate) fn is_retryable_server_error(msg: &str) -> bool {
    let lower = msg.to_ascii_lowercase();
    RETRYABLE_SERVER_PATTERNS.iter().any(|p| lower.contains(p))
}

/// Error patterns that indicate provider-side rate limiting.
const RATE_LIMIT_PATTERNS: &[&str] = &[
    "http 429",
    "status=429",
    "status 429",
    "status: 429",
    "too many requests",
    "rate limit",
    "rate_limit",
];

pub(crate) fn is_rate_limit_error(msg: &str) -> bool {
    let lower = msg.to_ascii_lowercase();
    RATE_LIMIT_PATTERNS.iter().any(|p| lower.contains(p))
}

/// Error patterns that indicate the account is out of credits/quota.
/// These are not retryable in the short term and should surface directly.
const BILLING_QUOTA_PATTERNS: &[&str] = &[
    "insufficient_quota",
    "quota exceeded",
    "current quota",
    "billing details",
    "billing limit",
    "credit balance",
];

pub(crate) fn is_billing_quota_error(msg: &str) -> bool {
    let lower = msg.to_ascii_lowercase();
    BILLING_QUOTA_PATTERNS.iter().any(|p| lower.contains(p))
}

const TERMINAL_PROVIDER_PATTERNS: &[&str] = &[
    "http 400",
    "http 401",
    "http 403",
    "http 404",
    "http 405",
    "http 413",
    "http 415",
    "http 422",
    "invalid_request_error",
    "invalid_api_key",
    "authentication_error",
    "permission_denied",
    "model_not_found",
    "unsupported_model",
    "context_length_exceeded",
    "response incomplete:",
];

fn is_terminal_provider_error(msg: &str) -> bool {
    let lower = msg.to_ascii_lowercase();
    TERMINAL_PROVIDER_PATTERNS
        .iter()
        .any(|pattern| lower.contains(pattern))
}

pub(crate) const SERVER_MAX_RETRIES: u8 = 1;

/// Base delay for non-rate-limit transient retries.
pub(crate) const SERVER_RETRY_DELAY: std::time::Duration = std::time::Duration::from_secs(2);

pub(crate) const EMPTY_PROVIDER_RESPONSE_ERROR: &str =
    "The provider returned an empty response (possible network error). Please try again.";

/// Unknown provider errors receive one delayed retry before surfacing.
pub(crate) const UNKNOWN_RETRY_DELAY_MS: u64 = 10_000;
pub(crate) const UNKNOWN_MAX_RETRIES: u8 = 1;

/// Rate-limit retries use exponential backoff with a cap.
pub(crate) const RATE_LIMIT_INITIAL_RETRY_MS: u64 = 2_000;
pub(crate) const RATE_LIMIT_MAX_RETRY_MS: u64 = 60_000;
pub(crate) const RATE_LIMIT_MAX_RETRIES: u8 = 10;

pub(crate) fn next_rate_limit_retry_ms(previous_ms: Option<u64>) -> u64 {
    previous_ms
        .map(|ms| ms.saturating_mul(2))
        .unwrap_or(RATE_LIMIT_INITIAL_RETRY_MS)
        .clamp(RATE_LIMIT_INITIAL_RETRY_MS, RATE_LIMIT_MAX_RETRY_MS)
}

fn parse_retry_delay_ms_from_fragment(
    fragment: &str,
    unit_default_ms: bool,
    max_ms: u64,
) -> Option<u64> {
    let start = fragment.find(|c: char| c.is_ascii_digit())?;
    let tail = &fragment[start..];
    let digits_len = tail.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits_len == 0 {
        return None;
    }
    let amount = tail[..digits_len].parse::<u64>().ok()?;
    let unit = tail[digits_len..].trim_start();

    let ms = if unit.starts_with("ms") || unit.starts_with("millisecond") {
        amount
    } else if unit.starts_with("sec") || unit.starts_with("second") || unit.starts_with('s') {
        amount.saturating_mul(1_000)
    } else if unit.starts_with("min") || unit.starts_with("minute") || unit.starts_with('m') {
        amount.saturating_mul(60_000)
    } else if unit_default_ms {
        amount
    } else {
        amount.saturating_mul(1_000)
    };

    Some(ms.clamp(1, max_ms))
}

/// Extract retry delay hints embedded in provider error messages.
///
/// Supports patterns like:
/// - `retry_after_ms=1234`
/// - `Retry-After: 30`
/// - `retry after 30s`
/// - `retry in 45 seconds`
pub(crate) fn extract_retry_after_ms(msg: &str, max_ms: u64) -> Option<u64> {
    let lower = msg.to_ascii_lowercase();
    for (needle, default_ms) in [
        ("retry_after_ms=", true),
        ("retry-after-ms=", true),
        ("retry_after=", false),
        ("retry-after:", false),
        ("retry after ", false),
        ("retry in ", false),
    ] {
        if let Some(idx) = lower.find(needle) {
            let fragment = &lower[idx + needle.len()..];
            if let Some(ms) = parse_retry_delay_ms_from_fragment(fragment, default_ms, max_ms) {
                return Some(ms);
            }
        }
    }
    None
}

pub(crate) fn next_retry_delay_ms(
    msg: &str,
    server_retries_remaining: &mut u8,
    rate_limit_retries_remaining: &mut u8,
    rate_limit_backoff_ms: &mut Option<u64>,
    unknown_retries_remaining: &mut u8,
) -> Option<u64> {
    // Account/billing quota exhaustion is not transient; don't auto-retry.
    if is_billing_quota_error(msg) {
        return None;
    }

    if is_rate_limit_error(msg) {
        if *rate_limit_retries_remaining == 0 {
            return None;
        }
        *rate_limit_retries_remaining -= 1;

        // Keep exponential state advancing even when the provider gives a
        // Retry-After hint, so future retries remain bounded and predictable.
        let current_backoff = *rate_limit_backoff_ms;
        *rate_limit_backoff_ms = Some(next_rate_limit_retry_ms(current_backoff));

        let hinted_ms = extract_retry_after_ms(msg, RATE_LIMIT_MAX_RETRY_MS);
        let delay_ms = hinted_ms
            .or(*rate_limit_backoff_ms)
            .unwrap_or(RATE_LIMIT_INITIAL_RETRY_MS);
        return Some(delay_ms.clamp(1, RATE_LIMIT_MAX_RETRY_MS));
    }

    if is_retryable_server_error(msg) {
        if *server_retries_remaining == 0 {
            return None;
        }
        *server_retries_remaining -= 1;
        return Some(SERVER_RETRY_DELAY.as_millis() as u64);
    }

    if is_terminal_provider_error(msg) {
        return None;
    }

    if *unknown_retries_remaining == 0 {
        return None;
    }
    *unknown_retries_remaining -= 1;
    Some(UNKNOWN_RETRY_DELAY_MS)
}
