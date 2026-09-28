//! Active provider context loaded directly from the canonical journal.

use std::collections::HashSet;

use {
    chelix_agents::model::{ChatMessage, ChatReconstruction},
    chelix_sessions::{ActiveEvent, store::SessionStore},
};

use crate::{
    compaction_reminder::CompactionReminder,
    error::{self, Error},
};

pub(crate) async fn load_active_messages(
    store: &SessionStore,
    key: &str,
) -> error::Result<(Vec<ChatMessage>, usize)> {
    let mut reconstruction = None;
    let tail = store
        .with_active_records(key, |event| match event {
            ActiveEvent::Start { segment_ids, .. } => {
                reconstruction = Some(ChatReconstruction::new(true, segment_ids));
                Ok(())
            },
            ActiveEvent::Row { index, payload } => {
                let reconstruction = reconstruction.as_mut().ok_or_else(|| {
                    chelix_sessions::Error::message(
                        "active context started without its segment set",
                    )
                })?;
                reconstruction.push(index, &payload).map_err(|error| {
                    chelix_sessions::Error::message(format!(
                        "failed to reconstruct provider context: {error}"
                    ))
                })?;
                Ok(())
            },
        })
        .await?;
    let messages = reconstruction
        .ok_or_else(|| Error::message("active context produced no reconstruction"))?
        .finish()
        .map_err(|error| Error::external("failed to reconstruct provider context", error))?;
    Ok((messages, tail))
}

pub(crate) async fn visible_tools(
    store: &SessionStore,
    key: &str,
    tools_enabled: bool,
    lazy: bool,
) -> error::Result<HashSet<String>> {
    if tools_enabled && lazy {
        match store.visible_tool_names(key).await {
            Ok(names) => Ok(names),
            Err(chelix_sessions::Error::NoCanonicalJournal { .. }) => Ok(HashSet::new()),
            Err(error) => Err(error.into()),
        }
    } else {
        Ok(HashSet::new())
    }
}

pub(crate) async fn reminder_from_journal(
    store: &SessionStore,
    key: &str,
    enabled: bool,
) -> error::Result<CompactionReminder> {
    let pointers = match store.pointers(key).await {
        Ok(pointers) => pointers,
        Err(chelix_sessions::Error::NoCanonicalJournal { .. }) => {
            return CompactionReminder::from_parts(enabled, false, None);
        },
        Err(error) => return Err(error.into()),
    };
    let first_user = if enabled {
        if let Some(index) = pointers.first_user_index {
            Some(store.read_record(key, index).await?)
        } else {
            None
        }
    } else {
        None
    };
    CompactionReminder::from_parts(
        enabled,
        pointers.last_checkpoint_index.is_some(),
        first_user.as_ref(),
    )
}
