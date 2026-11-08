use std::path::Path;

use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde_json::{Value, json};

pub fn load(path: &Path, session_id: &str) -> Result<Vec<Value>, String> {
    let mut connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(sqlite_error)?;
    let snapshot = connection.transaction().map_err(sqlite_error)?;
    let (prompt, lineage): (Option<String>, bool) = snapshot
        .query_row(
            "SELECT COALESCE(p.prompt, s.system_prompt),
                s.parent_session_id IS NOT NULL OR EXISTS (
                    SELECT 1 FROM sessions WHERE parent_session_id = s.id
                )
         FROM sessions s LEFT JOIN system_prompts p ON p.hash = s.system_prompt_hash
         WHERE s.id = ?1",
            [session_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(sqlite_error)?
        .ok_or("SQLite session ID was not found")?;
    if lineage {
        return Err("SQLite sessions with lineage require the Python resume path".into());
    }

    let mut messages = Vec::new();
    if let Some(prompt) = prompt.filter(|prompt| !prompt.is_empty()) {
        messages.push(json!({"role": "system", "content": prompt}));
    }
    let mut rows = snapshot
        .prepare(
            "SELECT role, content,
                tool_call_id, tool_calls, _compressed_summary, compacted, api_content,
                COALESCE(reasoning, reasoning_content, reasoning_details, codex_reasoning_items, codex_message_items)
         FROM messages WHERE session_id = ?1 AND (active = 1 OR compacted = 1)
           AND role != 'session_meta' ORDER BY id LIMIT 10001",
        )
        .map_err(sqlite_error)?;
    let mut rows = rows.query([session_id]).map_err(sqlite_error)?;
    let mut expected = "user";
    let mut pending = std::collections::HashSet::new();
    let mut count = 0;
    while let Some(row) = rows.next().map_err(sqlite_error)? {
        count += 1;
        if count > 10_000 {
            return Err("SQLite session exceeds the Rust replay limit".into());
        }
        let role: String = row.get(0).map_err(sqlite_error)?;
        let content: Option<String> = row.get(1).map_err(sqlite_error)?;
        let tool_id: Option<String> = row.get(2).map_err(sqlite_error)?;
        let tool_calls: Option<String> = row.get(3).map_err(sqlite_error)?;
        if row.get::<_, bool>(4).map_err(sqlite_error)?
            || row.get::<_, bool>(5).map_err(sqlite_error)?
        {
            return Err("compacted SQLite sessions require the Python resume path".into());
        }
        if role != expected {
            return Err("SQLite history requires transcript repair before Rust replay".into());
        }
        if row
            .get::<_, Option<String>>(7)
            .map_err(sqlite_error)?
            .is_some()
        {
            return Err(
                "SQLite history with provider-specific reasoning requires the Python resume path"
                    .into(),
            );
        }
        let api_content = row.get::<_, Option<String>>(6).map_err(sqlite_error)?;
        let content = replay_content(&role, content, api_content)?;
        let mut message = json!({"role": role, "content": content});
        expected = match role.as_str() {
            "user" => "assistant",
            "assistant" => {
                if let Some(calls) = tool_calls {
                    let calls: Vec<Value> = serde_json::from_str(&calls)
                        .map_err(|_| "invalid SQLite tool-call data")?;
                    for call in &calls {
                        let id = call
                            .get("id")
                            .and_then(Value::as_str)
                            .filter(|id| !id.is_empty())
                            .ok_or("SQLite tool call is missing its ID")?;
                        if !pending.insert(id.to_owned()) {
                            return Err("duplicate SQLite tool-call ID".into());
                        }
                    }
                    if !calls.is_empty() {
                        message["tool_calls"] = json!(calls);
                    }
                }
                if pending.is_empty() { "user" } else { "tool" }
            }
            "tool" => {
                let id = tool_id
                    .filter(|id| pending.remove(id))
                    .ok_or("SQLite tool result has no matching call")?;
                message["tool_call_id"] = json!(id);
                if pending.is_empty() {
                    "assistant"
                } else {
                    "tool"
                }
            }
            _ => return Err("unsupported SQLite message role".into()),
        };
        messages.push(message);
    }
    if expected != "user" {
        return Err("SQLite history ends in an incomplete turn".into());
    }
    Ok(messages)
}

fn replay_content(
    role: &str,
    content: Option<String>,
    api_content: Option<String>,
) -> Result<Value, String> {
    let mut decoded = decode_content(content);
    if let Some(text) = decoded
        .as_str()
        .filter(|_| matches!(role, "user" | "assistant"))
    {
        let normalized = text.to_ascii_lowercase();
        if normalized.contains("memory-context")
            || normalized.contains("[system note:")
            || normalized
                .trim_start()
                .starts_with("review the conversation above and")
        {
            return Err(
                "SQLite history with internal context requires the Python resume path".into(),
            );
        }
        decoded = json!(text.trim());
    }
    Ok(api_content
        .filter(|text| !text.is_empty())
        .map(Value::String)
        .unwrap_or(decoded))
}

fn decode_content(content: Option<String>) -> Value {
    match content {
        Some(text) => text
            .strip_prefix("\0json:")
            .and_then(|data| serde_json::from_str(data).ok())
            .unwrap_or(Value::String(text)),
        None => Value::Null,
    }
}

fn sqlite_error(error: rusqlite::Error) -> String {
    format!("cannot read Hermes SQLite history: {error}")
}
