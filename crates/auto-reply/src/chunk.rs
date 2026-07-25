/// Split long agent responses to fit channel message size limits.
pub fn chunk_response(text: &str, max_len: usize) -> Vec<String> {
    if text.len() <= max_len {
        return vec![text.to_string()];
    }
    text.chars()
        .collect::<Vec<_>>()
        .chunks(max_len)
        .map(|c| c.iter().collect())
        .collect()
}
