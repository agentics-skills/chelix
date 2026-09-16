//! Split oversized memory chunks so sidecar embed JSON stays within the body limit.

use chelix_protocol::{EMBEDDING_PRIORITY_INDEX, EmbeddingRequest};

use crate::{chunker::Chunk, error::Result};

pub(crate) fn split_oversize_embed_chunks(chunks: Vec<Chunk>, limit: usize) -> Result<Vec<Chunk>> {
    let mut split = Vec::new();
    for chunk in chunks {
        split.extend(split_chunk(chunk, limit)?);
    }
    Ok(split)
}

fn split_chunk(chunk: Chunk, limit: usize) -> Result<Vec<Chunk>> {
    if embed_request_bytes(&chunk.text)? <= limit {
        return Ok(vec![chunk]);
    }
    if chunk.text.chars().count() <= 1 {
        return Err(crate::error::Error::Embedding(
            "embedding payload exceeds sidecar body limit".into(),
        ));
    }

    let Some((left, right)) = split_text(&chunk.text) else {
        return Err(crate::error::Error::Embedding(
            "embedding payload exceeds sidecar body limit".into(),
        ));
    };

    let left_offset = 0;
    let right_offset = left.len();
    let mut parts = split_chunk(chunk_part(&chunk, left_offset, left), limit)?;
    parts.extend(split_chunk(chunk_part(&chunk, right_offset, right), limit)?);
    Ok(parts)
}

fn embed_request_bytes(text: &str) -> Result<usize> {
    Ok(serde_json::to_vec(&EmbeddingRequest {
        text: text.to_owned(),
        priority: EMBEDDING_PRIORITY_INDEX,
    })?
    .len())
}

fn split_text(text: &str) -> Option<(&str, &str)> {
    let mid = char_boundary_at_or_before(text, text.len() / 2);
    if let Some(idx) = nearest_split_after(text, mid, |ch| ch == '\n') {
        return Some(text.split_at(idx));
    }
    if let Some(idx) = nearest_split_after(text, mid, char::is_whitespace) {
        return Some(text.split_at(idx));
    }
    if mid == 0 || mid == text.len() {
        let next = text.chars().next()?.len_utf8();
        if next == text.len() {
            return None;
        }
        return Some(text.split_at(next));
    }
    Some(text.split_at(mid))
}

fn char_boundary_at_or_before(text: &str, mut idx: usize) -> usize {
    if idx > text.len() {
        idx = text.len();
    }
    while idx > 0 && !text.is_char_boundary(idx) {
        idx -= 1;
    }
    if idx == 0 {
        return text.chars().next().map_or(0, |ch| ch.len_utf8());
    }
    idx
}

fn nearest_split_after(text: &str, mid: usize, matches: impl Fn(char) -> bool) -> Option<usize> {
    let mut best: Option<(usize, usize)> = None;
    for (idx, ch) in text.char_indices() {
        if !matches(ch) {
            continue;
        }
        let split_idx = idx + ch.len_utf8();
        if split_idx == 0 || split_idx == text.len() {
            continue;
        }
        let distance = split_idx.abs_diff(mid);
        match best {
            Some((best_distance, _)) if best_distance <= distance => {},
            _ => best = Some((distance, split_idx)),
        }
    }
    best.map(|(_, split_idx)| split_idx)
}

fn chunk_part(parent: &Chunk, byte_offset: usize, part: &str) -> Chunk {
    let prefix = &parent.text[..byte_offset];
    let start_line = parent.start_line + prefix.matches('\n').count();
    let newlines = part.matches('\n').count();
    let end_line = start_line + newlines - usize::from(part.ends_with('\n'));
    Chunk {
        text: part.to_owned(),
        start_line,
        end_line,
    }
}

#[cfg(test)]
mod tests {
    use chelix_protocol::EMBEDDING_MAX_BODY_BYTES;

    use super::*;

    fn request_len(text: &str) -> usize {
        embed_request_bytes(text).unwrap_or_else(|error| panic!("{error}"))
    }

    #[test]
    fn oversized_line_splits_without_truncation() {
        let text = "x".repeat(EMBEDDING_MAX_BODY_BYTES + 64);
        let chunks = split_oversize_embed_chunks(
            vec![Chunk {
                text: text.clone(),
                start_line: 1,
                end_line: 1,
            }],
            EMBEDDING_MAX_BODY_BYTES,
        )
        .unwrap_or_else(|error| panic!("{error}"));

        assert!(chunks.len() > 1);
        assert_eq!(
            chunks
                .iter()
                .map(|chunk| chunk.text.as_str())
                .collect::<String>(),
            text
        );
        for chunk in &chunks {
            assert!(request_len(&chunk.text) <= EMBEDDING_MAX_BODY_BYTES);
            assert_eq!(chunk.start_line, 1);
            assert_eq!(chunk.end_line, 1);
        }
    }

    #[test]
    fn whitespace_split_keeps_needle_in_one_part() {
        let text = format!("{} needle {}", "a".repeat(600_000), "b".repeat(600_000));
        let chunks = split_oversize_embed_chunks(
            vec![Chunk {
                text: text.clone(),
                start_line: 1,
                end_line: 1,
            }],
            EMBEDDING_MAX_BODY_BYTES,
        )
        .unwrap_or_else(|error| panic!("{error}"));

        assert_eq!(
            chunks
                .iter()
                .map(|chunk| chunk.text.as_str())
                .collect::<String>(),
            text
        );
        assert!(
            chunks.iter().any(|chunk| chunk.text.contains("needle")),
            "needle was split across chunks: {chunks:?}"
        );
        for chunk in &chunks {
            assert!(request_len(&chunk.text) <= EMBEDDING_MAX_BODY_BYTES);
        }
    }

    #[test]
    fn newline_split_keeps_trailing_newline_out_of_end_line() {
        let line = "x".repeat(500_000);
        let text = format!("{line}\n{line}\n{line}");
        let chunks = split_oversize_embed_chunks(
            vec![Chunk {
                text: text.clone(),
                start_line: 1,
                end_line: 3,
            }],
            EMBEDDING_MAX_BODY_BYTES,
        )
        .unwrap_or_else(|error| panic!("{error}"));

        assert_eq!(
            chunks
                .iter()
                .map(|chunk| chunk.text.as_str())
                .collect::<String>(),
            text
        );
        assert!(chunks.len() >= 2);
        assert!(chunks[0].text.ends_with('\n'));
        assert_eq!(chunks[0].start_line, 1);
        assert_eq!(chunks[0].end_line, 1);
        assert_eq!(chunks[1].start_line, 2);
        assert!(
            chunks
                .iter()
                .all(|chunk| chunk.start_line != 1 || chunk.end_line != 3)
        );
        for chunk in &chunks {
            assert!(request_len(&chunk.text) <= EMBEDDING_MAX_BODY_BYTES);
        }
    }

    #[test]
    fn mid_line_split_keeps_parent_line_number() {
        let text = "w".repeat(EMBEDDING_MAX_BODY_BYTES + 32);
        let chunks = split_oversize_embed_chunks(
            vec![Chunk {
                text: text.clone(),
                start_line: 2,
                end_line: 2,
            }],
            EMBEDDING_MAX_BODY_BYTES,
        )
        .unwrap_or_else(|error| panic!("{error}"));

        assert!(chunks.len() > 1);
        assert_eq!(
            chunks
                .iter()
                .map(|chunk| chunk.text.as_str())
                .collect::<String>(),
            text
        );
        for chunk in &chunks {
            assert_eq!(chunk.start_line, 2);
            assert_eq!(chunk.end_line, 2);
        }
    }

    #[test]
    fn mid_line_offset_recomputes_overlapping_line_ranges() {
        let parent = Chunk {
            text: "aaa\nbbbbbb\nccc".into(),
            start_line: 1,
            end_line: 3,
        };
        let offset = "aaa\nbbb".len();
        let left = chunk_part(&parent, 0, &parent.text[..offset]);
        let right = chunk_part(&parent, offset, &parent.text[offset..]);

        assert_eq!((left.start_line, left.end_line), (1, 2));
        assert_eq!((right.start_line, right.end_line), (2, 3));
    }
}
