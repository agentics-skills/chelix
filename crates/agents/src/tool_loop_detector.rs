//! Loop detector for equivalent tool failures repeated across model rounds.
//!
//! A tool-call batch emitted by one LLM response is one observation round. The
//! detector updates atomically after every batch, so parallel sibling calls can
//! contribute at most one observation per equivalent failure identity.
//!
//! Failure equivalence is intentionally unchanged: failures match when they use
//! the same tool and have either the same normalized arguments or the same
//! non-empty error.
//!
//! Two escalation stages:
//! 1. **Nudge** — inject a directive message after the configured number of
//!    equivalent failed model rounds.
//! 2. **Tool stripping** — if a later model round repeats that failure after the
//!    nudge was visible, omit tool schemas for one forced-text turn.

use std::{
    collections::{BTreeMap, BTreeSet, VecDeque, hash_map::DefaultHasher},
    hash::{Hash, Hasher},
};

use serde_json::Value;

/// Fingerprint of one tool-call outcome used for round-level loop detection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCallFingerprint {
    pub tool_name: String,
    pub args_hash: u64,
    /// Whether this call failed (tool error, validation rejection, or
    /// `{success: false}` even without an `error` key).
    pub failed: bool,
    /// Hash of a non-empty tool error string. Logical failures without an error
    /// message use `None` and can still match by normalized arguments.
    pub error_hash: Option<u64>,
    /// Raw error string (kept for formatting the intervention message).
    pub error_text: Option<String>,
    /// Raw arguments (kept for formatting the intervention message).
    pub arguments: Value,
}

impl ToolCallFingerprint {
    /// Create a fingerprint for a successful tool call.
    #[must_use]
    pub fn success(tool_name: &str, arguments: &Value) -> Self {
        Self {
            tool_name: tool_name.to_string(),
            args_hash: hash_value(arguments),
            failed: false,
            error_hash: None,
            error_text: None,
            arguments: arguments.clone(),
        }
    }

    /// Create a fingerprint for a failed tool call.
    ///
    /// `error` may be `None` when the tool returned `{success: false}` without
    /// an `error` field. The failure still participates through its argument
    /// identity.
    #[must_use]
    pub fn failure(tool_name: &str, arguments: &Value, error: Option<&str>) -> Self {
        let non_empty_error = error.filter(|error| !error.is_empty());
        Self {
            tool_name: tool_name.to_string(),
            args_hash: hash_value(arguments),
            failed: true,
            error_hash: non_empty_error.map(hash_str),
            error_text: non_empty_error.map(String::from),
            arguments: arguments.clone(),
        }
    }

    #[must_use]
    pub fn is_failure(&self) -> bool {
        self.failed
    }

    fn argument_identity(&self) -> FailureIdentity {
        FailureIdentity::Arguments {
            tool_name: self.tool_name.clone(),
            args_hash: self.args_hash,
        }
    }

    fn identities(&self) -> Vec<FailureIdentity> {
        let mut identities = vec![self.argument_identity()];
        if let Some(error_hash) = self.error_hash {
            identities.push(FailureIdentity::Error {
                tool_name: self.tool_name.clone(),
                error_hash,
            });
        }
        identities
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum FailureIdentity {
    Arguments { tool_name: String, args_hash: u64 },
    Error { tool_name: String, error_hash: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FailureStreak {
    observations: VecDeque<ToolCallFingerprint>,
}

impl FailureStreak {
    fn new(observation: ToolCallFingerprint) -> Self {
        Self {
            observations: VecDeque::from([observation]),
        }
    }

    fn push(&mut self, observation: ToolCallFingerprint, window: usize) {
        self.observations.push_back(observation);
        while self.observations.len() > window {
            self.observations.pop_front();
        }
    }
}

/// Escalation stages for the loop detector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InterventionStage {
    /// No intervention active.
    #[default]
    None,
    /// Stage 1 fired: a directive nudge has been injected; the next iteration
    /// still passes the normal tool schemas.
    Nudged,
    /// Stage 2 fired: the next iteration will pass an empty tool list, forcing
    /// a text response. After that forced-text turn the detector is reset.
    StripTools,
}

/// Action computed once after recording a complete model round.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopDetectorAction {
    /// No intervention — continue normally.
    None,
    /// Stage 1: inject a directive intervention message for the next LLM call.
    InjectNudge,
    /// Stage 2: strip tool schemas on the next LLM call.
    StripTools,
}

/// Round-aware loop detector.
#[derive(Debug, PartialEq, Eq)]
pub struct ToolLoopDetector {
    active: BTreeMap<FailureIdentity, FailureStreak>,
    window: usize,
    strip_on_second_fire: bool,
    stage: InterventionStage,
    intervention_identities: BTreeSet<FailureIdentity>,
    intervention_evidence: Vec<ToolCallFingerprint>,
}

impl ToolLoopDetector {
    /// Create a detector with the given failed-model-round window. `window == 0`
    /// disables detection entirely.
    #[must_use]
    pub fn new(window: usize, strip_on_second_fire: bool) -> Self {
        Self {
            active: BTreeMap::new(),
            window,
            strip_on_second_fire,
            stage: InterventionStage::None,
            intervention_identities: BTreeSet::new(),
            intervention_evidence: Vec::new(),
        }
    }

    #[must_use]
    pub fn stage(&self) -> InterventionStage {
        self.stage
    }

    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.window > 0
    }

    /// Reset all failure streaks and escalation state.
    pub fn reset(&mut self) {
        self.active.clear();
        self.stage = InterventionStage::None;
        self.intervention_identities.clear();
        self.intervention_evidence.clear();
    }

    /// Atomically record all outcomes from one LLM tool-call batch.
    ///
    /// The batch is reduced independently of sibling order. Equivalent failures
    /// update each identity at most once, unrelated successes do not erase a
    /// failure, and an exact successful operation suppresses the matching
    /// `same tool + same arguments` failure for this round.
    ///
    /// Returns at most one intervention action for the complete round.
    pub fn record_round(&mut self, outcomes: &[ToolCallFingerprint]) -> LoopDetectorAction {
        if self.window == 0 {
            return LoopDetectorAction::None;
        }

        let successful_operations: BTreeSet<FailureIdentity> = outcomes
            .iter()
            .filter(|outcome| !outcome.is_failure())
            .map(ToolCallFingerprint::argument_identity)
            .collect();

        let mut failures: Vec<ToolCallFingerprint> = outcomes
            .iter()
            .filter(|outcome| {
                outcome.is_failure()
                    && !successful_operations.contains(&outcome.argument_identity())
            })
            .cloned()
            .collect();
        failures.sort_by(fingerprint_cmp);
        failures.dedup();

        let mut round_observations = BTreeMap::<FailureIdentity, ToolCallFingerprint>::new();
        for failure in &failures {
            for identity in failure.identities() {
                match round_observations.get(&identity) {
                    Some(current) if fingerprint_cmp(current, failure).is_le() => {},
                    _ => {
                        round_observations.insert(identity, failure.clone());
                    },
                }
            }
        }

        if round_observations.is_empty() {
            self.reset();
            return LoopDetectorAction::None;
        }

        self.active
            .retain(|identity, _| round_observations.contains_key(identity));
        for (identity, observation) in &round_observations {
            if let Some(streak) = self.active.get_mut(identity) {
                streak.push(observation.clone(), self.window);
            } else {
                self.active
                    .insert(identity.clone(), FailureStreak::new(observation.clone()));
            }
        }

        if self.stage == InterventionStage::StripTools {
            return LoopDetectorAction::None;
        }

        if self.stage == InterventionStage::Nudged {
            let repeated_identity = self
                .intervention_identities
                .iter()
                .find(|identity| round_observations.contains_key(*identity))
                .cloned();
            if let Some(identity) = repeated_identity {
                if self.strip_on_second_fire {
                    self.stage = InterventionStage::StripTools;
                    if let Some(streak) = self.active.get(&identity) {
                        self.intervention_evidence = streak.observations.iter().cloned().collect();
                    }
                    return LoopDetectorAction::StripTools;
                }
                return LoopDetectorAction::None;
            }

            // This failed round does not match the class that received the
            // nudge. Start a fresh escalation cycle for the current failures.
            self.stage = InterventionStage::None;
            self.intervention_identities.clear();
            self.intervention_evidence.clear();
        }

        let candidate_streak = self
            .active
            .values()
            .find(|streak| streak.observations.len() >= self.window)
            .cloned();
        let Some(candidate_streak) = candidate_streak else {
            return LoopDetectorAction::None;
        };

        self.intervention_identities = self
            .active
            .iter()
            .filter(|(_, streak)| streak.observations.len() >= self.window)
            .map(|(identity, _)| identity.clone())
            .collect();
        self.intervention_evidence = candidate_streak.observations.iter().cloned().collect();
        self.stage = InterventionStage::Nudged;
        LoopDetectorAction::InjectNudge
    }

    /// Called after the forced-text iteration produced by stage 2. The next
    /// tool-enabled model round starts with no prior failure observations.
    pub fn clear_strip_tools(&mut self) {
        if self.stage == InterventionStage::StripTools {
            self.reset();
        }
    }

    /// Return one representative equivalent failure from each model round that
    /// caused the current intervention.
    #[must_use]
    pub fn window_snapshot(&self) -> Vec<ToolCallFingerprint> {
        self.intervention_evidence.clone()
    }
}

/// Build the stage-1 nudge from representative failures across model rounds.
#[must_use]
pub fn format_intervention_message(rounds: &[ToolCallFingerprint]) -> String {
    let mut msg = String::from(
        "SYSTEM INTERVENTION — LOOP DETECTED\n\nEquivalent tool failures were repeated across ",
    );
    msg.push_str(&rounds.len().to_string());
    msg.push_str(" distinct model rounds after earlier results were available:\n");
    for (index, fingerprint) in rounds.iter().enumerate() {
        let args_str =
            serde_json::to_string(&fingerprint.arguments).unwrap_or_else(|_| "{}".to_string());
        let error = fingerprint.error_text.as_deref().unwrap_or("(no error)");
        msg.push_str(&format!(
            "  Round {}: {}({}) → error: {}\n",
            index + 1,
            fingerprint.tool_name,
            args_str,
            error
        ));
    }

    let tool_name = rounds
        .first()
        .map(|fingerprint| fingerprint.tool_name.as_str())
        .unwrap_or("this tool");

    msg.push_str(
        "\nThese failures are equivalent because the tool and either its normalized arguments or \
         its non-empty error match across rounds. The displayed arguments may differ. Repeating \
         the same failed operation without changing the underlying approach is unlikely to help.\n\n\
         On your next turn:\n",
    );
    msg.push_str(&format!(
        "1. Do NOT call `{tool_name}` or any other tool.\n"
    ));
    msg.push_str("2. Do NOT repeat this failure pattern.\n");
    msg.push_str("3. Respond to the user in plain text.\n");
    msg.push_str("4. Explain what you were trying to accomplish.\n");
    msg.push_str("5. If you do not know what arguments to use, ask the user for clarification.\n");
    msg.push_str("\nThe user is waiting for a text response.");
    msg
}

/// Stage-2 reinforcement used when the runner strips tool schemas for the next
/// iteration.
#[must_use]
pub fn format_strip_tools_message() -> String {
    "SYSTEM INTERVENTION — TOOLS DISABLED FOR THIS TURN\n\nYou repeated an equivalent failed \
     tool operation in another model round after receiving both the earlier result and a recovery \
     directive. Tools are disabled for this single turn. Respond to the user in plain text: explain \
     what you were trying to do, and ask for clarification if needed."
        .to_string()
}

fn fingerprint_cmp(left: &ToolCallFingerprint, right: &ToolCallFingerprint) -> std::cmp::Ordering {
    (
        left.tool_name.as_str(),
        canonicalize(&left.arguments),
        left.error_text.as_deref(),
        left.failed,
    )
        .cmp(&(
            right.tool_name.as_str(),
            canonicalize(&right.arguments),
            right.error_text.as_deref(),
            right.failed,
        ))
}

fn hash_value(value: &Value) -> u64 {
    let canonical = canonicalize(value);
    let mut hasher = DefaultHasher::new();
    canonical.hash(&mut hasher);
    hasher.finish()
}

fn hash_str(value: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

fn canonicalize(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(boolean) => boolean.to_string(),
        Value::Number(number) => number.to_string(),
        Value::String(string) => format!("\"{string}\""),
        Value::Array(array) => {
            let inner: Vec<String> = array.iter().map(canonicalize).collect();
            format!("[{}]", inner.join(","))
        },
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let inner: Vec<String> = keys
                .into_iter()
                .map(|key| format!("\"{}\":{}", key, canonicalize(&map[key])))
                .collect();
            format!("{{{}}}", inner.join(","))
        },
    }
}

#[cfg(test)]
mod tests {
    use {super::*, serde_json::json};

    fn success(tool: &str, args: Value) -> ToolCallFingerprint {
        ToolCallFingerprint::success(tool, &args)
    }

    fn failure(tool: &str, args: Value, error: Option<&str>) -> ToolCallFingerprint {
        ToolCallFingerprint::failure(tool, &args, error)
    }

    fn repeated_failure() -> ToolCallFingerprint {
        failure(
            "execute_command",
            json!({}),
            Some("missing 'command' parameter"),
        )
    }

    #[test]
    fn window_zero_disables_detection() {
        let mut detector = ToolLoopDetector::new(0, true);
        for _ in 0..3 {
            assert_eq!(
                detector.record_round(&[repeated_failure(), repeated_failure()]),
                LoopDetectorAction::None
            );
        }
        assert_eq!(detector.stage(), InterventionStage::None);
    }

    #[test]
    fn same_round_different_args_same_error_does_not_fire() {
        let mut detector = ToolLoopDetector::new(2, true);
        let error = Some("Grep requires an absolute 'path' argument");
        let action = detector.record_round(&[
            failure("Grep", json!({"pattern": "alpha", "path": null}), error),
            failure("Grep", json!({"pattern": "beta", "path": null}), error),
        ]);

        assert_eq!(action, LoopDetectorAction::None);
        assert_eq!(detector.stage(), InterventionStage::None);
    }

    #[test]
    fn same_round_identical_failures_count_once() {
        let mut detector = ToolLoopDetector::new(2, true);
        let failures = vec![repeated_failure(); 20];

        assert_eq!(detector.record_round(&failures), LoopDetectorAction::None);
        assert_eq!(detector.stage(), InterventionStage::None);
    }

    #[test]
    fn equivalent_failure_in_next_round_fires_nudge() {
        let mut detector = ToolLoopDetector::new(2, true);
        let error = Some("Grep requires an absolute 'path' argument");
        assert_eq!(
            detector.record_round(&[
                failure("Grep", json!({"pattern": "alpha", "path": null}), error),
                failure("Grep", json!({"pattern": "beta", "path": null}), error),
            ]),
            LoopDetectorAction::None
        );

        assert_eq!(
            detector.record_round(&[failure(
                "Grep",
                json!({"pattern": "gamma", "path": null}),
                error,
            )]),
            LoopDetectorAction::InjectNudge
        );
        assert_eq!(detector.stage(), InterventionStage::Nudged);
        assert_eq!(detector.window_snapshot().len(), 2);
    }

    #[test]
    fn stage_two_requires_another_distinct_round() {
        let mut detector = ToolLoopDetector::new(2, true);
        let batch = vec![repeated_failure(); 20];

        assert_eq!(detector.record_round(&batch), LoopDetectorAction::None);
        assert_eq!(
            detector.record_round(&batch),
            LoopDetectorAction::InjectNudge
        );
        assert_eq!(detector.stage(), InterventionStage::Nudged);
        assert_eq!(
            detector.record_round(&batch),
            LoopDetectorAction::StripTools
        );
        assert_eq!(detector.stage(), InterventionStage::StripTools);
    }

    #[test]
    fn mixed_batch_is_order_independent() {
        let error = Some("Grep requires an absolute 'path' argument");
        let success = success("memory_search", json!({"query": "preferences"}));
        let fail_a = failure("Grep", json!({"pattern": "alpha", "path": null}), error);
        let fail_b = failure("Grep", json!({"pattern": "beta", "path": null}), error);
        let mut forward = ToolLoopDetector::new(2, true);
        let mut reverse = ToolLoopDetector::new(2, true);

        assert_eq!(
            forward.record_round(&[success.clone(), fail_a.clone(), fail_b.clone()]),
            LoopDetectorAction::None
        );
        assert_eq!(
            reverse.record_round(&[fail_b, fail_a, success]),
            LoopDetectorAction::None
        );
        assert_eq!(forward, reverse);
    }

    #[test]
    fn unrelated_success_does_not_erase_failed_class_in_same_round() {
        let mut detector = ToolLoopDetector::new(2, true);
        assert_eq!(
            detector.record_round(&[repeated_failure()]),
            LoopDetectorAction::None
        );
        assert_eq!(
            detector.record_round(&[
                success("memory_search", json!({"query": "preferences"})),
                repeated_failure(),
            ]),
            LoopDetectorAction::InjectNudge
        );
    }

    #[test]
    fn successful_equivalent_operation_resets_its_failure_sequence() {
        let mut detector = ToolLoopDetector::new(2, true);
        let failed_args = json!({"command": "ls"});
        assert_eq!(
            detector.record_round(&[failure(
                "execute_command",
                failed_args.clone(),
                Some("temporary error"),
            )]),
            LoopDetectorAction::None
        );
        assert_eq!(
            detector.record_round(&[
                success("execute_command", failed_args.clone()),
                failure("browser", json!({"action": "open"}), Some("unavailable")),
            ]),
            LoopDetectorAction::None
        );
        assert_eq!(
            detector.record_round(&[failure(
                "execute_command",
                failed_args,
                Some("temporary error"),
            )]),
            LoopDetectorAction::None
        );
    }

    #[test]
    fn all_success_round_resets_detector() {
        let mut detector = ToolLoopDetector::new(2, true);
        let _ = detector.record_round(&[repeated_failure()]);
        assert_eq!(
            detector.record_round(&[repeated_failure()]),
            LoopDetectorAction::InjectNudge
        );

        assert_eq!(
            detector.record_round(&[success("execute_command", json!({"command": "ls"}))]),
            LoopDetectorAction::None
        );
        assert_eq!(detector.stage(), InterventionStage::None);
        assert!(detector.window_snapshot().is_empty());
        assert_eq!(
            detector.record_round(&[repeated_failure()]),
            LoopDetectorAction::None
        );
    }

    #[test]
    fn non_matching_failure_starts_new_sequence_without_intervention() {
        let mut detector = ToolLoopDetector::new(2, true);
        assert_eq!(
            detector.record_round(&[failure(
                "execute_command",
                json!({}),
                Some("missing command"),
            )]),
            LoopDetectorAction::None
        );
        assert_eq!(
            detector.record_round(&[failure(
                "browser",
                json!({"action": "open"}),
                Some("browser unavailable"),
            )]),
            LoopDetectorAction::None
        );
        assert_eq!(detector.stage(), InterventionStage::None);
        assert_eq!(
            detector.record_round(&[failure(
                "browser",
                json!({"action": "open"}),
                Some("browser unavailable"),
            )]),
            LoopDetectorAction::InjectNudge
        );
    }

    #[test]
    fn different_args_same_tool_same_error_matches_across_rounds() {
        let mut detector = ToolLoopDetector::new(2, true);
        let error = Some("missing 'command' parameter");
        let _ = detector.record_round(&[failure("execute_command", json!({}), error)]);
        assert_eq!(
            detector.record_round(&[failure("execute_command", json!({"cmd": ""}), error,)]),
            LoopDetectorAction::InjectNudge
        );
    }

    #[test]
    fn different_tools_do_not_match_across_rounds() {
        let mut detector = ToolLoopDetector::new(2, true);
        let _ = detector.record_round(&[failure("execute_command", json!({}), Some("error"))]);
        assert_eq!(
            detector.record_round(&[failure("browser", json!({}), Some("error"))]),
            LoopDetectorAction::None
        );
    }

    #[test]
    fn strip_tools_disabled_stays_nudged() {
        let mut detector = ToolLoopDetector::new(2, false);
        let _ = detector.record_round(&[repeated_failure()]);
        assert_eq!(
            detector.record_round(&[repeated_failure()]),
            LoopDetectorAction::InjectNudge
        );
        assert_eq!(
            detector.record_round(&[repeated_failure()]),
            LoopDetectorAction::None
        );
        assert_eq!(detector.stage(), InterventionStage::Nudged);
    }

    #[test]
    fn non_matching_failure_after_nudge_starts_fresh_cycle() {
        let mut detector = ToolLoopDetector::new(2, true);
        let _ = detector.record_round(&[repeated_failure()]);
        let _ = detector.record_round(&[repeated_failure()]);
        assert_eq!(detector.stage(), InterventionStage::Nudged);

        assert_eq!(
            detector.record_round(&[failure("browser", json!({}), Some("offline"))]),
            LoopDetectorAction::None
        );
        assert_eq!(detector.stage(), InterventionStage::None);
        assert_eq!(
            detector.record_round(&[failure("browser", json!({}), Some("offline"))]),
            LoopDetectorAction::InjectNudge
        );
    }

    #[test]
    fn sibling_identity_that_did_not_reach_window_cannot_trigger_stage_two() {
        let mut detector = ToolLoopDetector::new(2, true);
        let established = failure("Grep", json!({"pattern": "stable"}), Some("missing path"));
        let sibling = failure("browser", json!({"action": "open"}), Some("offline"));

        let _ = detector.record_round(std::slice::from_ref(&established));
        assert_eq!(
            detector.record_round(&[established, sibling.clone()]),
            LoopDetectorAction::InjectNudge
        );
        assert_eq!(
            detector.record_round(std::slice::from_ref(&sibling)),
            LoopDetectorAction::InjectNudge
        );
        assert_eq!(detector.stage(), InterventionStage::Nudged);
        assert_eq!(
            detector.record_round(&[sibling]),
            LoopDetectorAction::StripTools
        );
    }

    #[test]
    fn trailing_equivalent_success_suppresses_failure_independent_of_order() {
        let failed = failure("execute_command", json!({"command": "ls"}), Some("error"));
        let succeeded = success("execute_command", json!({"command": "ls"}));
        let mut first = ToolLoopDetector::new(2, true);
        let mut second = ToolLoopDetector::new(2, true);
        let _ = first.record_round(std::slice::from_ref(&failed));
        let _ = second.record_round(std::slice::from_ref(&failed));

        assert_eq!(
            first.record_round(&[failed.clone(), succeeded.clone()]),
            LoopDetectorAction::None
        );
        assert_eq!(
            second.record_round(&[succeeded, failed]),
            LoopDetectorAction::None
        );
        assert_eq!(first, second);
        assert_eq!(first.stage(), InterventionStage::None);
    }

    #[test]
    fn clear_after_strip_resets_state_fully() {
        let mut detector = ToolLoopDetector::new(2, true);
        let _ = detector.record_round(&[repeated_failure()]);
        let _ = detector.record_round(&[repeated_failure()]);
        let _ = detector.record_round(&[repeated_failure()]);
        assert_eq!(detector.stage(), InterventionStage::StripTools);

        detector.clear_strip_tools();
        assert_eq!(detector.stage(), InterventionStage::None);
        assert!(detector.window_snapshot().is_empty());
        assert_eq!(
            detector.record_round(&[repeated_failure()]),
            LoopDetectorAction::None
        );
    }

    #[test]
    fn failure_without_error_string_matches_by_arguments_across_rounds() {
        let mut detector = ToolLoopDetector::new(2, true);
        let failed = failure("browser", json!({"action": "open"}), None);
        assert_eq!(
            detector.record_round(&[failed.clone(), failed.clone()]),
            LoopDetectorAction::None
        );
        assert_eq!(
            detector.record_round(&[failed]),
            LoopDetectorAction::InjectNudge
        );
    }

    #[test]
    fn canonical_argument_hashing_is_order_stable() {
        let left = json!({"a": 1, "b": 2});
        let right = json!({"b": 2, "a": 1});
        assert_eq!(hash_value(&left), hash_value(&right));

        let mut detector = ToolLoopDetector::new(2, true);
        let _ = detector.record_round(&[failure("tool", left, Some("first"))]);
        assert_eq!(
            detector.record_round(&[failure("tool", right, Some("second"))]),
            LoopDetectorAction::InjectNudge
        );
    }

    #[test]
    fn intervention_message_describes_equivalent_failed_rounds() {
        let rounds = vec![
            failure("Grep", json!({"pattern": "alpha"}), Some("missing path")),
            failure("Grep", json!({"pattern": "beta"}), Some("missing path")),
        ];
        let message = format_intervention_message(&rounds);

        assert!(message.contains("LOOP DETECTED"));
        assert!(message.contains("distinct model rounds"));
        assert!(message.contains("arguments may differ"));
        assert!(!message.contains("identical failed invocations"));
    }
}
