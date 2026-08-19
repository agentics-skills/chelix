//! Ingress-side assignment of canonical provider item positions.
//!
//! A provider item position is the item's canonical slot in the provider
//! response output. It is assigned exactly once, on provider ingress, in the
//! order items first appear in the stream, and is keyed by the item identity.
//! Every later update for the same identity reuses the position assigned on
//! first sight.
//!
//! Streaming transports are not a reliable source of that slot:
//!
//! * Chat Completions has no notion of output items at all. Reasoning text,
//!   visible text and tool calls arrive as parallel delta channels with no
//!   index that orders them against each other.
//! * Responses carries `output_index` on delta events, but that value orders
//!   items only within a channel for some providers, so distinct items can
//!   share the same `output_index` inside a single response.
//! * External agents report text and thinking as separate event kinds with no
//!   ordering between them at all.
//!
//! Deriving a position from those fields makes two different items collide on
//! one slot, which the materializer rejects as a position/id conflict. This
//! allocator is therefore the single place where positions come from, and it
//! guarantees distinct identities never receive the same position.

use std::collections::HashMap;

use crate::provider_output::{ProviderItemId, ProviderItemPosition};

/// Assigns a stable position to every provider item identity in one segment.
///
/// Positions are handed out densely from `0` in first-appearance order. The
/// allocator never reassigns a position, so an identity keeps the same slot
/// for the whole lifetime of the segment.
#[derive(Debug, Default)]
pub struct ItemPositionAllocator {
    assigned: HashMap<ProviderItemId, ProviderItemPosition>,
    next_position: usize,
}

impl ItemPositionAllocator {
    /// Return the position for `item_id`, allocating the next free slot the
    /// first time this identity is seen.
    pub fn position_for(&mut self, item_id: &ProviderItemId) -> ProviderItemPosition {
        if let Some(position) = self.assigned.get(item_id) {
            return *position;
        }
        let position = ProviderItemPosition::new(self.next_position);
        self.next_position += 1;
        self.assigned.insert(item_id.clone(), position);
        position
    }

    /// Position already assigned to `item_id`, if it has been seen before.
    #[must_use]
    pub fn assigned_position(&self, item_id: &ProviderItemId) -> Option<ProviderItemPosition> {
        self.assigned.get(item_id).copied()
    }

    /// Number of identities that have received a position.
    #[must_use]
    pub fn len(&self) -> usize {
        self.assigned.len()
    }

    /// Whether no identity has received a position yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.assigned.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_follow_first_appearance_order() {
        let mut allocator = ItemPositionAllocator::default();
        let reasoning = ProviderItemId::new("rs_0");
        let message = ProviderItemId::new("msg_0");
        let call = ProviderItemId::new("call_a");

        assert_eq!(allocator.position_for(&reasoning).as_usize(), 0);
        assert_eq!(allocator.position_for(&message).as_usize(), 1);
        assert_eq!(allocator.position_for(&call).as_usize(), 2);
    }

    #[test]
    fn repeated_lookups_reuse_the_first_assignment() {
        let mut allocator = ItemPositionAllocator::default();
        let reasoning = ProviderItemId::new("rs_0");
        let message = ProviderItemId::new("msg_0");

        let first = allocator.position_for(&reasoning);
        let _ = allocator.position_for(&message);
        // Interleaved updates must not move an identity to another slot.
        assert_eq!(allocator.position_for(&reasoning), first);
        assert_eq!(allocator.position_for(&reasoning), first);
        assert_eq!(allocator.len(), 2);
    }

    #[test]
    fn distinct_identities_never_share_a_position() {
        let mut allocator = ItemPositionAllocator::default();
        let ids = ["rs_0", "msg_0", "call_a", "call_b"].map(ProviderItemId::new);

        let positions: Vec<_> = ids
            .iter()
            .map(|id| allocator.position_for(id).as_usize())
            .collect();

        assert_eq!(positions, vec![0, 1, 2, 3]);
    }

    #[test]
    fn assigned_position_reports_only_known_identities() {
        let mut allocator = ItemPositionAllocator::default();
        let known = ProviderItemId::new("rs_0");
        let unknown = ProviderItemId::new("msg_0");

        assert!(allocator.is_empty());
        let position = allocator.position_for(&known);

        assert_eq!(allocator.assigned_position(&known), Some(position));
        assert_eq!(allocator.assigned_position(&unknown), None);
    }
}
