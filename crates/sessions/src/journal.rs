//! Canonical session journal stored beside the UI history tables.

use std::collections::HashSet;

use {
    chelix_common::{ProviderSegmentId, tool_disclosure::record_disclosure},
    serde_json::Value,
    sqlx::{Connection, Row, SqliteConnection, SqlitePool},
};

use crate::{Error, Result, ui_history_database::unsigned};

const PAGE: i64 = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JournalImportConflict {
    Skip,
    Overwrite,
}

#[derive(Debug, Clone, Copy)]
pub struct JournalPointers {
    pub canonical_tail: usize,
    pub last_checkpoint_index: Option<usize>,
    pub first_user_index: Option<usize>,
}

#[derive(Debug)]
pub enum ActiveEvent {
    Start {
        tail: usize,
        segment_ids: HashSet<ProviderSegmentId>,
    },
    Row {
        index: usize,
        payload: Value,
    },
}

pub(crate) async fn begin_immediate(
    pool: &SqlitePool,
) -> Result<sqlx::Transaction<'static, sqlx::Sqlite>> {
    Ok(pool.begin_with("BEGIN IMMEDIATE").await?)
}

pub(crate) async fn ensure_open_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    key: &str,
    discard: bool,
) -> Result<JournalPointers> {
    let ui = sqlx::query("SELECT total_messages FROM ui_history_sessions WHERE session_key = ?")
        .bind(key)
        .fetch_optional(&mut **tx)
        .await?;
    let journal = sqlx::query(
        "SELECT canonical_tail, last_checkpoint_index, first_user_index FROM session_journal WHERE session_key = ?",
    )
    .bind(key)
    .fetch_optional(&mut **tx)
    .await?;
    if let Some(journal) = journal {
        return pointers_from_row(&journal);
    }
    let total_messages = ui
        .as_ref()
        .map(|row| row.try_get::<i64, _>("total_messages"))
        .transpose()?
        .unwrap_or(0);
    let snapshots: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM ui_history_snapshots WHERE session_key = ?")
            .bind(key)
            .fetch_one(&mut **tx)
            .await?;
    if !discard && (total_messages > 0 || snapshots > 0) {
        return Err(Error::HistoryWithoutJournal {
            session_key: key.to_string(),
        });
    }
    if !discard {
        sqlx::query(
            "INSERT INTO session_journal (session_key, canonical_tail, last_checkpoint_index, first_user_index) VALUES (?, 0, NULL, NULL)",
        )
        .bind(key)
        .execute(&mut **tx)
        .await?;
    }
    Ok(JournalPointers {
        canonical_tail: 0,
        last_checkpoint_index: None,
        first_user_index: None,
    })
}

pub(crate) async fn pointers(pool: &SqlitePool, key: &str) -> Result<JournalPointers> {
    let row = sqlx::query(
        "SELECT canonical_tail, last_checkpoint_index, first_user_index FROM session_journal WHERE session_key = ?",
    )
    .bind(key)
    .fetch_optional(pool)
    .await?;
    row.map(|row| pointers_from_row(&row))
        .transpose()?
        .ok_or_else(|| Error::NoCanonicalJournal {
            session_key: key.to_string(),
        })
}

pub(crate) async fn append(
    pool: &SqlitePool,
    key: &str,
    records: &[Value],
    expected_tail: usize,
) -> Result<usize> {
    if records.is_empty() {
        return Err(Error::message("session append batch is empty"));
    }
    let mut tx = begin_immediate(pool).await?;
    let updated = sqlx::query(
        "UPDATE session_journal SET canonical_tail = canonical_tail WHERE session_key = ? AND canonical_tail = ?",
    )
    .bind(key)
    .bind(i64::try_from(expected_tail).map_err(|error| Error::message(error.to_string()))?)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if updated != 1 {
        let found = sqlx::query_scalar::<_, i64>(
            "SELECT canonical_tail FROM session_journal WHERE session_key = ?",
        )
        .bind(key)
        .fetch_optional(&mut *tx)
        .await?;
        let found = found.unwrap_or(0);
        return Err(Error::message(format!(
            "expected message index {expected_tail}, found session tail {found}"
        )));
    }
    let mut next = expected_tail;
    let mut last_checkpoint = None;
    let mut first_user = None;
    for record in records {
        let index = next;
        next = next
            .checked_add(1)
            .ok_or_else(|| Error::message("session message index overflow"))?;
        insert_record(&mut tx, key, index, record).await?;
        let role = record.get("role").and_then(Value::as_str).unwrap_or("");
        if role == "checkpoint" {
            last_checkpoint = Some(index);
        }
        if role == "user" && first_user.is_none() {
            first_user = Some(index);
        }
    }
    sqlx::query(
        "UPDATE session_journal SET canonical_tail = ?, last_checkpoint_index = COALESCE(?, last_checkpoint_index), first_user_index = COALESCE(first_user_index, ?) WHERE session_key = ?",
    )
    .bind(i64::try_from(next).map_err(|error| Error::message(error.to_string()))?)
    .bind(last_checkpoint.map(|index| i64::try_from(index).map_err(|error| Error::message(error.to_string()))).transpose()?)
    .bind(first_user.map(|index| i64::try_from(index).map_err(|error| Error::message(error.to_string()))).transpose()?)
    .bind(key)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(expected_tail)
}

async fn insert_record(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    key: &str,
    index: usize,
    record: &Value,
) -> Result<()> {
    let role = record.get("role").and_then(Value::as_str).unwrap_or("");
    let index_i64 = i64::try_from(index).map_err(|error| Error::message(error.to_string()))?;
    sqlx::query(
        "INSERT INTO session_records (session_key, record_index, role, payload) VALUES (?, ?, ?, ?)",
    )
    .bind(key)
    .bind(index_i64)
    .bind(role)
    .bind(serde_json::to_string(record)?)
    .execute(&mut **tx)
    .await?;
    match record_disclosure(record) {
        Ok(disclosure) => {
            for name in disclosure.names {
                sqlx::query(
                    "INSERT INTO session_tool_disclosures (session_key, tool_name, record_index) VALUES (?, ?, ?) ON CONFLICT(session_key, tool_name) DO NOTHING",
                )
                .bind(key)
                .bind(name)
                .bind(index_i64)
                .execute(&mut **tx)
                .await?;
            }
            if let Some(segment_id) = disclosure.segment_id {
                sqlx::query(
                    "INSERT INTO session_assistant_segments (session_key, record_index, segment_id) VALUES (?, ?, ?)",
                )
                .bind(key)
                .bind(index_i64)
                .bind(segment_id)
                .execute(&mut **tx)
                .await?;
            }
        },
        Err(_) => {
            sqlx::query(
                "INSERT INTO session_disclosure_errors (session_key, record_index) VALUES (?, ?)",
            )
            .bind(key)
            .bind(index_i64)
            .execute(&mut **tx)
            .await?;
        },
    }
    Ok(())
}

pub(crate) async fn with_active_records<F>(
    pool: &SqlitePool,
    key: &str,
    mut visit: F,
) -> Result<usize>
where
    F: FnMut(ActiveEvent) -> Result<()>,
{
    let mut tx = pool.begin().await?;
    let row = sqlx::query(
        "SELECT canonical_tail, last_checkpoint_index, first_user_index FROM session_journal WHERE session_key = ?",
    )
    .bind(key)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(row) = row else {
        ensure_absent_journal_is_empty(&mut tx, key).await?;
        visit(ActiveEvent::Start {
            tail: 0,
            segment_ids: HashSet::new(),
        })?;
        return Ok(0);
    };
    let pointers = pointers_from_row(&row)?;
    let tail = pointers.canonical_tail;
    let segment_ids = segment_ids(&mut tx, key, &pointers).await?;
    visit(ActiveEvent::Start { tail, segment_ids })?;
    if tail == 0 {
        return Ok(0);
    }
    if let Some(checkpoint) = pointers.last_checkpoint_index.filter(|index| *index < tail) {
        let payload = required_payload(&mut tx, key, checkpoint).await?;
        let start = tail_start(checkpoint, &payload);
        visit(ActiveEvent::Row {
            index: checkpoint,
            payload,
        })?;
        stream_range(&mut tx, key, start, checkpoint, true, &mut visit).await?;
        stream_range(
            &mut tx,
            key,
            checkpoint.saturating_add(1),
            tail,
            false,
            &mut visit,
        )
        .await?;
    } else {
        stream_range(&mut tx, key, 0, tail, false, &mut visit).await?;
    }
    Ok(tail)
}

fn tail_start(checkpoint_index: usize, payload: &Value) -> usize {
    payload
        .get("messagesSummarized")
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .filter(|start| *start <= checkpoint_index)
        .unwrap_or(checkpoint_index)
}

async fn segment_ids(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    key: &str,
    pointers: &JournalPointers,
) -> Result<HashSet<ProviderSegmentId>> {
    let tail = i64::try_from(pointers.canonical_tail)
        .map_err(|error| Error::message(error.to_string()))?;
    let mut ids = HashSet::new();
    let rows = match pointers.last_checkpoint_index.filter(|index| *index < pointers.canonical_tail) {
        None => {
            sqlx::query(
                "SELECT segment_id FROM session_assistant_segments WHERE session_key = ? AND record_index < ?",
            )
            .bind(key)
            .bind(tail)
            .fetch_all(&mut **tx)
            .await?
        },
        Some(checkpoint) => {
            let start = {
                let payload = required_payload(tx, key, checkpoint).await?;
                i64::try_from(tail_start(checkpoint, &payload)).map_err(|error| Error::message(error.to_string()))?
            };
            let checkpoint = i64::try_from(checkpoint).map_err(|error| Error::message(error.to_string()))?;
            let mut rows = sqlx::query(
                "SELECT segment_id FROM session_assistant_segments WHERE session_key = ? AND record_index >= ? AND record_index < ?",
            )
            .bind(key)
            .bind(start)
            .bind(checkpoint)
            .fetch_all(&mut **tx)
            .await?;
            let after = sqlx::query(
                "SELECT segment_id FROM session_assistant_segments WHERE session_key = ? AND record_index > ? AND record_index < ?",
            )
            .bind(key)
            .bind(checkpoint)
            .bind(tail)
            .fetch_all(&mut **tx)
            .await?;
            rows.extend(after);
            let before = sqlx::query(
                "SELECT segment_id FROM session_assistant_segments WHERE session_key = ? AND record_index < ? AND segment_id IN (SELECT json_extract(payload, '$.segmentId') FROM session_records WHERE session_key = ? AND role IN ('provider_update', 'provider_segment_close') AND record_index < ? AND ((record_index >= ? AND record_index < ?) OR record_index > ?))",
            )
            .bind(key)
            .bind(start)
            .bind(key)
            .bind(tail)
            .bind(start)
            .bind(checkpoint)
            .bind(checkpoint)
            .fetch_all(&mut **tx)
            .await?;
            rows.extend(before);
            rows
        },
    };
    for row in rows {
        let id: String = row.try_get("segment_id")?;
        ids.insert(ProviderSegmentId(id));
    }
    Ok(ids)
}

async fn stream_range<F>(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    key: &str,
    start: usize,
    end: usize,
    skip_checkpoints: bool,
    visit: &mut F,
) -> Result<()>
where
    F: FnMut(ActiveEvent) -> Result<()>,
{
    if start >= end {
        return Ok(());
    }
    let mut cursor = i64::try_from(start).map_err(|error| Error::message(error.to_string()))? - 1;
    let end_i = i64::try_from(end).map_err(|error| Error::message(error.to_string()))?;
    loop {
        let rows = if skip_checkpoints {
            sqlx::query(
                "SELECT record_index, payload FROM session_records WHERE session_key = ? AND record_index > ? AND record_index < ? AND role != 'checkpoint' ORDER BY record_index LIMIT ?",
            )
            .bind(key)
            .bind(cursor)
            .bind(end_i)
            .bind(PAGE)
            .fetch_all(&mut **tx)
            .await?
        } else {
            sqlx::query(
                "SELECT record_index, payload FROM session_records WHERE session_key = ? AND record_index > ? AND record_index < ? ORDER BY record_index LIMIT ?",
            )
            .bind(key)
            .bind(cursor)
            .bind(end_i)
            .bind(PAGE)
            .fetch_all(&mut **tx)
            .await?
        };
        if rows.is_empty() {
            break;
        }
        for row in &rows {
            let index = usize::try_from(unsigned(row.try_get("record_index")?)?)
                .map_err(|error| Error::message(error.to_string()))?;
            let payload = parse_payload(row.try_get("payload")?)?;
            cursor = i64::try_from(index).map_err(|error| Error::message(error.to_string()))?;
            visit(ActiveEvent::Row { index, payload })?;
        }
        if rows.len() < usize::try_from(PAGE).unwrap_or(usize::MAX) {
            break;
        }
    }
    Ok(())
}

pub(crate) async fn read_record(pool: &SqlitePool, key: &str, index: usize) -> Result<Value> {
    let mut tx = pool.begin().await?;
    required_payload(&mut tx, key, index).await
}

async fn required_payload(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    key: &str,
    index: usize,
) -> Result<Value> {
    let payload = sqlx::query_scalar::<_, String>(
        "SELECT payload FROM session_records WHERE session_key = ? AND record_index = ?",
    )
    .bind(key)
    .bind(i64::try_from(index).map_err(|error| Error::message(error.to_string()))?)
    .fetch_optional(&mut **tx)
    .await?;
    let Some(payload) = payload else {
        return Err(Error::message(format!(
            "message index {index} is outside session history"
        )));
    };
    parse_payload(payload)
}

pub(crate) async fn read_range(
    pool: &SqlitePool,
    key: &str,
    start: usize,
    end: usize,
) -> Result<Vec<Value>> {
    let mut rows = Vec::new();
    let mut tx = pool.begin().await?;
    stream_range(&mut tx, key, start, end, false, &mut |event| {
        if let ActiveEvent::Row { payload, .. } = event {
            rows.push(payload);
        }
        Ok(())
    })
    .await?;
    Ok(rows)
}

pub(crate) async fn read_last_n(pool: &SqlitePool, key: &str, count: usize) -> Result<Vec<Value>> {
    if count == 0 {
        return Ok(Vec::new());
    }
    let tail = pointers(pool, key).await?.canonical_tail;
    let start = tail.saturating_sub(count);
    read_range(pool, key, start, tail).await
}

pub(crate) async fn list_keys(pool: &SqlitePool) -> Result<Vec<String>> {
    Ok(
        sqlx::query_scalar("SELECT session_key FROM session_journal ORDER BY session_key")
            .fetch_all(pool)
            .await?,
    )
}

pub(crate) async fn visible_tool_names(pool: &SqlitePool, key: &str) -> Result<HashSet<String>> {
    let tail = i64::try_from(pointers(pool, key).await?.canonical_tail)
        .map_err(|error| Error::message(error.to_string()))?;
    let error_index = sqlx::query_scalar::<_, Option<i64>>(
        "SELECT MIN(record_index) FROM session_disclosure_errors WHERE session_key = ? AND record_index < ?",
    )
    .bind(key)
    .bind(tail)
    .fetch_one(pool)
    .await?;
    if let Some(index) = error_index {
        let index = usize::try_from(index).map_err(|error| Error::message(error.to_string()))?;
        let payload = read_record(pool, key, index).await?;
        return match record_disclosure(&payload) {
            Err(error) => Err(Error::from(error)),
            Ok(_) => Err(Error::message(format!(
                "session '{key}' disclosure error at record {index} decoded successfully"
            ))),
        };
    }
    let names = sqlx::query_scalar::<_, String>(
        "SELECT tool_name FROM session_tool_disclosures WHERE session_key = ? AND record_index < ?",
    )
    .bind(key)
    .bind(tail)
    .fetch_all(pool)
    .await?;
    Ok(names.into_iter().collect())
}

pub struct TokenTotals {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub last_assistant: Option<Value>,
}

pub(crate) async fn token_totals(pool: &SqlitePool, key: &str) -> Result<TokenTotals> {
    let row = sqlx::query(
        "SELECT COALESCE(SUM(json_extract(payload, '$.inputTokens')), 0), COALESCE(SUM(json_extract(payload, '$.outputTokens')), 0), COALESCE(SUM(json_extract(payload, '$.cacheReadTokens')), 0), COALESCE(SUM(json_extract(payload, '$.cacheWriteTokens')), 0) FROM session_records WHERE session_key = ?",
    )
    .bind(key)
    .fetch_one(pool)
    .await?;
    let last = sqlx::query_scalar::<_, String>(
        "SELECT payload FROM session_records WHERE session_key = ? AND role = 'assistant' ORDER BY record_index DESC LIMIT 1",
    )
    .bind(key)
    .fetch_optional(pool)
    .await?;
    Ok(TokenTotals {
        input_tokens: unsigned_sum(row.try_get(0)?)?,
        output_tokens: unsigned_sum(row.try_get(1)?)?,
        cache_read_tokens: unsigned_sum(row.try_get(2)?)?,
        cache_write_tokens: unsigned_sum(row.try_get(3)?)?,
        last_assistant: last.map(parse_payload).transpose()?,
    })
}

pub(crate) async fn assistant_payloads(pool: &SqlitePool, key: &str) -> Result<Vec<Value>> {
    let payloads = sqlx::query_scalar::<_, String>(
        "SELECT payload FROM session_records WHERE session_key = ? AND role = 'assistant' ORDER BY record_index",
    )
    .bind(key)
    .fetch_all(pool)
    .await?;
    payloads.into_iter().map(parse_payload).collect()
}

pub(crate) async fn child_suffix(pool: &SqlitePool, key: &str) -> Result<Vec<Value>> {
    let tail = pointers(pool, key).await?.canonical_tail;
    let last_user = max_role_index(pool, key, "user").await?;
    let last_assistant = max_role_index(pool, key, "assistant").await?;
    let start = match (last_user, last_assistant) {
        (Some(user), Some(assistant)) => user.min(assistant),
        (Some(index), None) | (None, Some(index)) => index,
        (None, None) => return Ok(Vec::new()),
    };
    read_range(pool, key, start, tail).await
}

async fn max_role_index(pool: &SqlitePool, key: &str, role: &str) -> Result<Option<usize>> {
    let index = sqlx::query_scalar::<_, Option<i64>>(
        "SELECT MAX(record_index) FROM session_records WHERE session_key = ? AND role = ?",
    )
    .bind(key)
    .bind(role)
    .fetch_one(pool)
    .await?;
    index
        .map(|index| usize::try_from(index).map_err(|error| Error::message(error.to_string())))
        .transpose()
}

pub(crate) async fn clear_journal(pool: &SqlitePool, key: &str) -> Result<()> {
    let mut tx = begin_immediate(pool).await?;
    for table in [
        "session_records",
        "session_tool_disclosures",
        "session_assistant_segments",
        "session_disclosure_errors",
    ] {
        sqlx::query(&format!("DELETE FROM {table} WHERE session_key = ?"))
            .bind(key)
            .execute(&mut *tx)
            .await?;
    }
    sqlx::query(
        "INSERT INTO session_journal (session_key, canonical_tail, last_checkpoint_index, first_user_index) VALUES (?, 0, NULL, NULL) ON CONFLICT(session_key) DO UPDATE SET canonical_tail = 0, last_checkpoint_index = NULL, first_user_index = NULL",
    )
    .bind(key)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

pub(crate) async fn truncate_journal(pool: &SqlitePool, key: &str, boundary: usize) -> Result<()> {
    let boundary_i = i64::try_from(boundary).map_err(|error| Error::message(error.to_string()))?;
    let mut tx = begin_immediate(pool).await?;
    for table in [
        "session_records",
        "session_tool_disclosures",
        "session_assistant_segments",
        "session_disclosure_errors",
    ] {
        sqlx::query(&format!(
            "DELETE FROM {table} WHERE session_key = ? AND record_index >= ?"
        ))
        .bind(key)
        .bind(boundary_i)
        .execute(&mut *tx)
        .await?;
    }
    sqlx::query(
        "UPDATE session_journal SET canonical_tail = ?, last_checkpoint_index = (SELECT MAX(record_index) FROM session_records WHERE session_key = ? AND role = 'checkpoint' AND record_index < ?), first_user_index = CASE WHEN first_user_index IS NOT NULL AND first_user_index < ? THEN first_user_index ELSE NULL END WHERE session_key = ?",
    )
    .bind(boundary_i)
    .bind(key)
    .bind(boundary_i)
    .bind(boundary_i)
    .bind(key)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

pub(crate) async fn copy_prefix(
    pool: &SqlitePool,
    parent: &str,
    destination: &str,
    boundary: usize,
) -> Result<()> {
    let boundary_i = i64::try_from(boundary).map_err(|error| Error::message(error.to_string()))?;
    let mut tx = begin_immediate(pool).await?;
    let existing = sqlx::query_scalar::<_, i64>(
        "SELECT canonical_tail FROM session_journal WHERE session_key = ?",
    )
    .bind(destination)
    .fetch_optional(&mut *tx)
    .await?;
    if existing.is_some_and(|tail| tail != 0) {
        return Err(Error::message("fork destination already has a journal"));
    }
    let parent_tail = sqlx::query_scalar::<_, i64>(
        "SELECT canonical_tail FROM session_journal WHERE session_key = ?",
    )
    .bind(parent)
    .fetch_optional(&mut *tx)
    .await?;
    let outside = match parent_tail {
        None => boundary_i > 0,
        Some(tail) => boundary_i > tail,
    };
    if outside {
        return Err(Error::message("fork boundary is outside canonical journal"));
    }
    sqlx::query(
        "INSERT INTO session_records (session_key, record_index, role, payload) SELECT ?, record_index, role, payload FROM session_records WHERE session_key = ? AND record_index < ?",
    )
    .bind(destination)
    .bind(parent)
    .bind(boundary_i)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO session_tool_disclosures (session_key, tool_name, record_index) SELECT ?, tool_name, record_index FROM session_tool_disclosures WHERE session_key = ? AND record_index < ?",
    )
    .bind(destination)
    .bind(parent)
    .bind(boundary_i)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO session_assistant_segments (session_key, record_index, segment_id) SELECT ?, record_index, segment_id FROM session_assistant_segments WHERE session_key = ? AND record_index < ?",
    )
    .bind(destination)
    .bind(parent)
    .bind(boundary_i)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO session_disclosure_errors (session_key, record_index) SELECT ?, record_index FROM session_disclosure_errors WHERE session_key = ? AND record_index < ?",
    )
    .bind(destination)
    .bind(parent)
    .bind(boundary_i)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO session_journal (session_key, canonical_tail, last_checkpoint_index, first_user_index) VALUES (?, ?, (SELECT MAX(record_index) FROM session_records WHERE session_key = ? AND role = 'checkpoint' AND record_index < ?), (SELECT first_user_index FROM session_journal WHERE session_key = ? AND first_user_index < ?)) ON CONFLICT(session_key) DO UPDATE SET canonical_tail = excluded.canonical_tail, last_checkpoint_index = excluded.last_checkpoint_index, first_user_index = excluded.first_user_index",
    )
    .bind(destination)
    .bind(boundary_i)
    .bind(destination)
    .bind(boundary_i)
    .bind(parent)
    .bind(boundary_i)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

pub(crate) async fn update_record(
    pool: &SqlitePool,
    key: &str,
    index: usize,
    updated: &Value,
) -> Result<()> {
    let mut tx = begin_immediate(pool).await?;
    let existing = required_payload(&mut tx, key, index).await?;
    match (record_disclosure(&existing), record_disclosure(updated)) {
        (Ok(before), Ok(after)) if before == after => {},
        (Err(_), Err(_)) => {},
        _ => {
            return Err(Error::message(
                "canonical update cannot change tool disclosure",
            ));
        },
    }
    let role = updated.get("role").and_then(Value::as_str).unwrap_or("");
    sqlx::query(
        "UPDATE session_records SET role = ?, payload = ? WHERE session_key = ? AND record_index = ?",
    )
    .bind(role)
    .bind(serde_json::to_string(updated)?)
    .bind(key)
    .bind(i64::try_from(index).map_err(|error| Error::message(error.to_string()))?)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

pub(crate) async fn latest_matching_assistant(
    pool: &SqlitePool,
    key: &str,
    tail: usize,
    expected_ids: &[&str],
) -> Result<Option<usize>> {
    let mut cursor = i64::try_from(tail).map_err(|error| Error::message(error.to_string()))?;
    let mut tx = pool.begin().await?;
    loop {
        let rows = sqlx::query(
            "SELECT record_index, payload FROM session_records WHERE session_key = ? AND role = 'assistant' AND record_index < ? ORDER BY record_index DESC LIMIT ?",
        )
        .bind(key)
        .bind(cursor)
        .bind(PAGE)
        .fetch_all(&mut *tx)
        .await?;
        if rows.is_empty() {
            return Ok(None);
        }
        for row in &rows {
            let index = usize::try_from(unsigned(row.try_get("record_index")?)?)
                .map_err(|error| Error::message(error.to_string()))?;
            cursor = i64::try_from(index).map_err(|error| Error::message(error.to_string()))?;
            let payload = parse_payload(row.try_get("payload")?)?;
            let ids = payload
                .get("tool_calls")
                .and_then(Value::as_array)
                .map(|calls| {
                    calls
                        .iter()
                        .filter_map(|call| call.get("id").and_then(Value::as_str))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            if ids == expected_ids {
                return Ok(Some(index));
            }
        }
        if rows.len() < usize::try_from(PAGE).unwrap_or(usize::MAX) {
            return Ok(None);
        }
    }
}

pub(crate) async fn latest_user_before(
    pool: &SqlitePool,
    key: &str,
    before: usize,
) -> Result<Option<usize>> {
    let index = sqlx::query_scalar::<_, Option<i64>>(
        "SELECT MAX(record_index) FROM session_records WHERE session_key = ? AND role = 'user' AND record_index < ?",
    )
    .bind(key)
    .bind(i64::try_from(before).map_err(|error| Error::message(error.to_string()))?)
    .fetch_one(pool)
    .await?;
    index
        .map(|index| usize::try_from(index).map_err(|error| Error::message(error.to_string())))
        .transpose()
}

pub(crate) async fn latest_user_text_contained_by(
    pool: &SqlitePool,
    key: &str,
    tail: usize,
    expected: &str,
) -> Result<Option<usize>> {
    let mut cursor = i64::try_from(tail).map_err(|error| Error::message(error.to_string()))?;
    let mut tx = pool.begin().await?;
    loop {
        let rows = sqlx::query(
            "SELECT record_index, payload FROM session_records WHERE session_key = ? AND role = 'user' AND record_index < ? ORDER BY record_index DESC LIMIT ?",
        )
        .bind(key)
        .bind(cursor)
        .bind(PAGE)
        .fetch_all(&mut *tx)
        .await?;
        if rows.is_empty() {
            return Ok(None);
        }
        for row in &rows {
            let index = usize::try_from(unsigned(row.try_get("record_index")?)?)
                .map_err(|error| Error::message(error.to_string()))?;
            cursor = i64::try_from(index).map_err(|error| Error::message(error.to_string()))?;
            let payload = parse_payload(row.try_get("payload")?)?;
            if payload
                .get("content")
                .and_then(Value::as_str)
                .is_some_and(|persisted| expected.contains(persisted))
            {
                return Ok(Some(index));
            }
        }
        if rows.len() < usize::try_from(PAGE).unwrap_or(usize::MAX) {
            return Ok(None);
        }
    }
}

pub(crate) async fn user_index_for_seq(pool: &SqlitePool, key: &str, seq: u64) -> Result<usize> {
    let index = sqlx::query_scalar::<_, Option<i64>>(
        "SELECT MIN(record_index) FROM session_records WHERE session_key = ? AND role = 'user' AND json_extract(payload, '$.seq') = ?",
    )
    .bind(key)
    .bind(i64::try_from(seq).map_err(|error| Error::message(error.to_string()))?)
    .fetch_one(pool)
    .await?;
    let Some(index) = index else {
        return Err(Error::message(format!(
            "user message with seq {seq} not found"
        )));
    };
    usize::try_from(index).map_err(|error| Error::message(error.to_string()))
}

pub(crate) async fn record_role(pool: &SqlitePool, key: &str, index: usize) -> Result<String> {
    let role = sqlx::query_scalar::<_, String>(
        "SELECT role FROM session_records WHERE session_key = ? AND record_index = ?",
    )
    .bind(key)
    .bind(i64::try_from(index).map_err(|error| Error::message(error.to_string()))?)
    .fetch_optional(pool)
    .await?;
    role.ok_or_else(|| Error::message(format!("message index {index} is outside session history")))
}

pub(crate) async fn import_attached_keys(conn: &mut SqliteConnection) -> Result<Vec<String>> {
    Ok(sqlx::query_scalar(
        "SELECT session_key FROM import_db.session_journal WHERE canonical_tail > 0",
    )
    .fetch_all(&mut *conn)
    .await?)
}

pub(crate) async fn import_one(
    conn: &mut SqliteConnection,
    key: &str,
    conflict: JournalImportConflict,
) -> Result<bool> {
    let mut tx = Connection::begin_with(&mut *conn, "BEGIN IMMEDIATE").await?;
    let journal_exists =
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM session_journal WHERE session_key = ?")
            .bind(key)
            .fetch_one(&mut *tx)
            .await?;
    let ui_exists = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM ui_history_sessions WHERE session_key = ?",
    )
    .bind(key)
    .fetch_one(&mut *tx)
    .await?;
    let exists = journal_exists > 0 || ui_exists > 0;
    if exists && conflict == JournalImportConflict::Skip {
        tx.rollback().await?;
        return Ok(false);
    }
    if exists {
        delete_session_rows(&mut tx, key).await?;
    }
    copy_imported_session(&mut tx, key).await?;
    tx.commit().await?;
    Ok(true)
}

async fn delete_session_rows(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    key: &str,
) -> Result<()> {
    for table in [
        "ui_history_snapshots",
        "ui_history_sessions",
        "session_records",
        "session_tool_disclosures",
        "session_assistant_segments",
        "session_disclosure_errors",
        "session_journal",
    ] {
        sqlx::query(&format!("DELETE FROM {table} WHERE session_key = ?"))
            .bind(key)
            .execute(&mut **tx)
            .await?;
    }
    Ok(())
}

async fn copy_imported_session(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    key: &str,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO session_journal (session_key, canonical_tail, last_checkpoint_index, first_user_index) SELECT session_key, canonical_tail, last_checkpoint_index, first_user_index FROM import_db.session_journal WHERE session_key = ?",
    )
    .bind(key)
    .execute(&mut **tx)
    .await?;
    sqlx::query(
        "INSERT INTO session_records (session_key, record_index, role, payload) SELECT session_key, record_index, role, payload FROM import_db.session_records WHERE session_key = ?",
    )
    .bind(key)
    .execute(&mut **tx)
    .await?;
    sqlx::query(
        "INSERT INTO session_tool_disclosures (session_key, tool_name, record_index) SELECT session_key, tool_name, record_index FROM import_db.session_tool_disclosures WHERE session_key = ?",
    )
    .bind(key)
    .execute(&mut **tx)
    .await?;
    sqlx::query(
        "INSERT INTO session_assistant_segments (session_key, record_index, segment_id) SELECT session_key, record_index, segment_id FROM import_db.session_assistant_segments WHERE session_key = ?",
    )
    .bind(key)
    .execute(&mut **tx)
    .await?;
    sqlx::query(
        "INSERT INTO session_disclosure_errors (session_key, record_index) SELECT session_key, record_index FROM import_db.session_disclosure_errors WHERE session_key = ?",
    )
    .bind(key)
    .execute(&mut **tx)
    .await?;
    sqlx::query(
        "INSERT INTO ui_history_sessions (session_key, generation, revision, next_position, total_messages, failure) SELECT session_key, generation, revision, next_position, total_messages, failure FROM import_db.ui_history_sessions WHERE session_key = ?",
    )
    .bind(key)
    .execute(&mut **tx)
    .await?;
    sqlx::query(
        "INSERT INTO ui_history_snapshots (session_key, message_id, position, revision, snapshot_json, search_text, run_id, canonical_start, canonical_end, canonical_record) SELECT session_key, message_id, position, revision, snapshot_json, search_text, run_id, canonical_start, canonical_end, canonical_record FROM import_db.ui_history_snapshots WHERE session_key = ?",
    )
    .bind(key)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

fn pointers_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<JournalPointers> {
    let tail = usize::try_from(unsigned(row.try_get("canonical_tail")?)?)
        .map_err(|error| Error::message(error.to_string()))?;
    let last_checkpoint_index = optional_index(row.try_get("last_checkpoint_index")?)?;
    let first_user_index = optional_index(row.try_get("first_user_index")?)?;
    Ok(JournalPointers {
        canonical_tail: tail,
        last_checkpoint_index,
        first_user_index,
    })
}

fn optional_index(value: Option<i64>) -> Result<Option<usize>> {
    value
        .map(|value| usize::try_from(value).map_err(|error| Error::message(error.to_string())))
        .transpose()
}

fn parse_payload(payload: String) -> Result<Value> {
    Ok(serde_json::from_str(&payload)?)
}

fn unsigned_sum(value: i64) -> Result<u64> {
    u64::try_from(value).map_err(|error| Error::message(error.to_string()))
}

async fn ensure_absent_journal_is_empty(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    key: &str,
) -> Result<()> {
    let total_messages = sqlx::query_scalar::<_, Option<i64>>(
        "SELECT total_messages FROM ui_history_sessions WHERE session_key = ?",
    )
    .bind(key)
    .fetch_optional(&mut **tx)
    .await?
    .flatten()
    .unwrap_or(0);
    let snapshots = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM ui_history_snapshots WHERE session_key = ?",
    )
    .bind(key)
    .fetch_one(&mut **tx)
    .await?;
    if total_messages > 0 || snapshots > 0 {
        return Err(Error::HistoryWithoutJournal {
            session_key: key.to_string(),
        });
    }
    Ok(())
}
