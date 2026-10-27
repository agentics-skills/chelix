//! Priority list of Context7 API tokens.

use std::sync::Mutex;

use secrecy::{ExposeSecret, Secret};

/// Rotation state for the last configured priority list.
#[derive(Default)]
pub(crate) struct Context7Tokens {
    state: Mutex<RotationState>,
}

#[derive(Default)]
struct RotationState {
    tokens: Vec<Secret<String>>,
    index: usize,
    generation: u64,
}

impl Context7Tokens {
    /// Select an owned token from the freshly read list, resetting changed lists.
    pub(crate) fn select(
        &self,
        tokens: Vec<Secret<String>>,
        rotate: bool,
    ) -> (Option<Secret<String>>, usize, u64) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let unchanged = state.tokens.len() == tokens.len()
            && state
                .tokens
                .iter()
                .zip(&tokens)
                .all(|(previous, current)| previous.expose_secret() == current.expose_secret());
        if !unchanged {
            state.tokens = tokens;
            state.index = 0;
            state.generation = state.generation.wrapping_add(1);
        } else if rotate && !state.tokens.is_empty() {
            state.index = (state.index + 1) % state.tokens.len();
        }
        (
            state.tokens.get(state.index).cloned(),
            state.tokens.len(),
            state.generation,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(values: &[&str]) -> Vec<Secret<String>> {
        values
            .iter()
            .map(|value| Secret::new((*value).to_string()))
            .collect()
    }

    #[test]
    fn changed_list_resets_priority_even_during_quota_rotation() {
        let rotation = Context7Tokens::default();
        let (first, count, initial_generation) =
            rotation.select(tokens(&["first", "second"]), false);
        assert_eq!(count, 2);
        assert_eq!(
            first
                .as_ref()
                .map(ExposeSecret::expose_secret)
                .map(String::as_str),
            Some("first")
        );
        let (second, _, generation) = rotation.select(tokens(&["first", "second"]), true);
        assert_eq!(generation, initial_generation);
        assert_eq!(
            second
                .as_ref()
                .map(ExposeSecret::expose_secret)
                .map(String::as_str),
            Some("second")
        );

        let (changed, count, changed_generation) =
            rotation.select(tokens(&["third", "first", "second"]), true);
        assert_ne!(changed_generation, initial_generation);
        assert_eq!(count, 3);
        assert_eq!(
            changed
                .as_ref()
                .map(ExposeSecret::expose_secret)
                .map(String::as_str),
            Some("third")
        );
        let (reordered, _, reordered_generation) =
            rotation.select(tokens(&["second", "first", "third"]), true);
        assert_ne!(reordered_generation, changed_generation);
        assert_eq!(
            reordered
                .as_ref()
                .map(ExposeSecret::expose_secret)
                .map(String::as_str),
            Some("second")
        );
        assert_eq!(
            first
                .as_ref()
                .map(ExposeSecret::expose_secret)
                .map(String::as_str),
            Some("first")
        );

        let (anonymous, count, empty_generation) = rotation.select(Vec::new(), true);
        assert_ne!(empty_generation, reordered_generation);
        assert!(anonymous.is_none());
        assert_eq!(count, 0);
    }
}
