//! Transport-neutral reasoning request policy.

use crate::{ReasoningEffort, ReasoningInclude, ReasoningSummary};

/// Complete provider-owned reasoning state for one API request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReasoningRequestState {
    selected_effort: ReasoningEffort,
    supported_efforts: Vec<ReasoningEffort>,
    summary: Option<ReasoningSummary>,
    include: Option<Vec<ReasoningInclude>>,
}

impl ReasoningRequestState {
    #[must_use]
    pub fn new(
        selected_effort: ReasoningEffort,
        supported_efforts: Vec<ReasoningEffort>,
        summary: Option<ReasoningSummary>,
        include: Option<Vec<ReasoningInclude>>,
    ) -> Self {
        Self {
            selected_effort,
            supported_efforts,
            summary,
            include,
        }
    }

    #[must_use]
    pub const fn selected_effort(&self) -> &ReasoningEffort {
        &self.selected_effort
    }
}

/// Closed reasoning decision consumed by wire serializers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReasoningPolicyDecision<'a> {
    Omit,
    Send {
        effort: &'a ReasoningEffort,
        summary: Option<ReasoningSummary>,
        include: Option<&'a [ReasoningInclude]>,
    },
}

/// Resolve the only provider-independent reasoning omission rule.
#[must_use]
pub fn resolve_reasoning_policy(state: &ReasoningRequestState) -> ReasoningPolicyDecision<'_> {
    if matches!(
        state.supported_efforts.as_slice(),
        [effort]
            if effort.as_str() == "off" && state.selected_effort.as_str() == "off"
    ) {
        return ReasoningPolicyDecision::Omit;
    }

    ReasoningPolicyDecision::Send {
        effort: &state.selected_effort,
        summary: state.summary,
        include: state.include.as_deref(),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn reasoning_policy_handles_only_exact_singleton_off_as_omit() {
        struct Case {
            supported_efforts: Vec<ReasoningEffort>,
            selected_effort: ReasoningEffort,
            omit: bool,
        }

        let cases = [
            Case {
                supported_efforts: vec!["off".into()],
                selected_effort: "off".into(),
                omit: true,
            },
            Case {
                supported_efforts: vec!["off".into(), "low".into()],
                selected_effort: "off".into(),
                omit: false,
            },
            Case {
                supported_efforts: vec!["low".into()],
                selected_effort: "low".into(),
                omit: false,
            },
        ];

        for case in cases {
            let state = ReasoningRequestState::new(
                case.selected_effort.clone(),
                case.supported_efforts,
                Some(ReasoningSummary::Detailed),
                Some(vec![ReasoningInclude::EncryptedContent]),
            );
            match resolve_reasoning_policy(&state) {
                ReasoningPolicyDecision::Omit => assert!(case.omit),
                ReasoningPolicyDecision::Send {
                    effort,
                    summary,
                    include,
                } => {
                    assert!(!case.omit);
                    assert_eq!(effort, &case.selected_effort);
                    assert_eq!(summary, Some(ReasoningSummary::Detailed));
                    assert_eq!(
                        include,
                        Some([ReasoningInclude::EncryptedContent].as_slice())
                    );
                },
            }
        }
    }
}
