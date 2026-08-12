use std::collections::BTreeMap;

use chelix_common::ReasoningContent;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ResponsesSummaryPartKey {
    output_index: usize,
    summary_index: usize,
    item_id: String,
}

impl ResponsesSummaryPartKey {
    fn new(item_id: &str, output_index: usize, summary_index: usize) -> Self {
        Self {
            output_index,
            summary_index,
            item_id: item_id.to_string(),
        }
    }
}

/// Accumulates visible reasoning without flattening Responses summary parts.
#[derive(Debug, Clone, Default)]
pub struct ReasoningAccumulator {
    text: String,
    responses_parts: BTreeMap<ResponsesSummaryPartKey, String>,
}

impl ReasoningAccumulator {
    /// Replace reasoning with one continuous provider reasoning stream.
    pub fn set_text(&mut self, text: &str) {
        self.text.clear();
        self.text.push_str(text);
        self.responses_parts.clear();
    }

    /// Append reasoning from providers with one continuous reasoning stream.
    pub fn append_text(&mut self, text: &str) {
        self.text.push_str(text);
    }

    /// Append a delta to one OpenAI Responses summary part.
    pub fn append_responses_delta(
        &mut self,
        item_id: &str,
        output_index: usize,
        summary_index: usize,
        delta: &str,
    ) {
        self.responses_parts
            .entry(ResponsesSummaryPartKey::new(
                item_id,
                output_index,
                summary_index,
            ))
            .or_default()
            .push_str(delta);
    }

    /// Replace accumulated deltas with the authoritative completed part text.
    pub fn complete_responses_part(
        &mut self,
        item_id: &str,
        output_index: usize,
        summary_index: usize,
        text: String,
    ) {
        self.responses_parts.insert(
            ResponsesSummaryPartKey::new(item_id, output_index, summary_index),
            text,
        );
    }

    /// Current visible reasoning in its provider-defined representation.
    #[must_use]
    pub fn content(&self) -> Option<ReasoningContent> {
        let mut parts = self.responses_parts.values().cloned().collect::<Vec<_>>();
        match (self.text.is_empty(), parts.is_empty()) {
            (true, true) => None,
            (false, true) => Some(ReasoningContent::Text(self.text.clone())),
            (true, false) => Some(ReasoningContent::Parts(parts)),
            (false, false) => {
                parts.insert(0, self.text.clone());
                Some(ReasoningContent::Parts(parts))
            },
        }
    }

    /// Whether the accumulator contains no visible reasoning text.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.content().is_none_or(|reasoning| reasoning.is_empty())
    }

    /// Whether the accumulator contains only whitespace reasoning text.
    #[must_use]
    pub fn is_blank(&self) -> bool {
        self.content().is_none_or(|reasoning| reasoning.is_blank())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn responses_parts_keep_api_order_and_completed_text_is_authoritative() {
        let mut reasoning = ReasoningAccumulator::default();
        reasoning.append_responses_delta("rs_second", 1, 0, "partial second");
        reasoning.append_responses_delta("rs_first", 0, 1, "partial first");
        reasoning.complete_responses_part("rs_first", 0, 1, "**Analyzing request**".to_string());
        reasoning.complete_responses_part("rs_second", 1, 0, "**Tracing response**".to_string());

        assert_eq!(
            reasoning.content(),
            Some(ReasoningContent::Parts(vec![
                "**Analyzing request**".to_string(),
                "**Tracing response**".to_string(),
            ]))
        );
    }
}
