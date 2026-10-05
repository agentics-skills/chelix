pub fn normalize_provider_name(value: &str) -> Option<String> {
    let normalized = value.trim().to_ascii_lowercase().replace('_', "-");
    (!normalized.is_empty()).then_some(normalized)
}
