//! Selects a fork prefix shared by semantic positions and canonical bindings.

use crate::{
    Error, Result, ui_history_projection,
    ui_history_types::{UiContent, UiEntry, UiForkBoundaryReason, UiForkResult},
};

pub(crate) struct UiForkSnapshot {
    pub(crate) result: UiForkResult,
    pub(crate) canonical_tail: usize,
    pub(crate) entries: Vec<UiEntry>,
}

pub(crate) fn select_prefix(
    entries: Vec<UiEntry>,
    requested: Option<u64>,
    source_end: u64,
) -> Result<UiForkSnapshot> {
    let mut position = requested.unwrap_or(source_end);
    if position > source_end {
        return Err(Error::message("fork point is outside UI history"));
    }
    let mut reasons = Vec::new();
    let unfinished = entries
        .iter()
        .filter(|entry| {
            matches!(entry.snapshot.content, UiContent::Record(_))
                && (!entry.snapshot.canonical_committed || ui_history_projection::active(entry))
        })
        .map(|entry| entry.snapshot.position)
        .min();
    if let Some(unfinished) = unfinished.filter(|unfinished| *unfinished < position) {
        if requested.is_some() {
            return Err(Error::message(
                "fork prefix contains uncommitted provider content",
            ));
        }
        position = unfinished;
        reasons.push(UiForkBoundaryReason::ActiveContent);
    }
    let canonical_tail = loop {
        let tail = entries
            .iter()
            .filter(|entry| entry.snapshot.position < position)
            .filter_map(|entry| entry.canonical.map(|binding| binding.end))
            .max()
            .unwrap_or(0);
        let crossing = entries
            .iter()
            .filter(|entry| entry.snapshot.position >= position)
            .filter_map(|entry| entry.canonical.map(|binding| binding.start))
            .filter(|start| *start < tail)
            .min();
        let Some(crossing) = crossing else {
            break tail;
        };
        if requested.is_some() {
            return Err(Error::message(
                "fork point splits an interleaved canonical segment",
            ));
        }
        position = entries
            .iter()
            .filter(|entry| {
                entry.snapshot.position < position
                    && entry
                        .canonical
                        .is_some_and(|binding| binding.end > crossing)
            })
            .map(|entry| entry.snapshot.position)
            .min()
            .ok_or_else(|| Error::message("interleaved fork boundary has no owning snapshot"))?;
        if !reasons.contains(&UiForkBoundaryReason::InterleavedSegment) {
            reasons.push(UiForkBoundaryReason::InterleavedSegment);
        }
    };
    if requested.is_none() && source_end > 0 && position == 0 {
        return Err(Error::message(
            "no confirmed UI history prefix is available for default fork",
        ));
    }
    let fork_point = u32::try_from(position).map_err(|error| Error::message(error.to_string()))?;
    Ok(UiForkSnapshot {
        result: UiForkResult {
            fork_point,
            source_end,
            boundary_adjusted: requested.is_none() && position < source_end,
            boundary_reasons: reasons,
        },
        canonical_tail,
        entries: entries
            .into_iter()
            .filter(|entry| entry.snapshot.position < position)
            .collect(),
    })
}
