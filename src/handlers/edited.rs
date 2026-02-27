use clickhouse::Row;
use grammers_client::update::Message;
use serde::Serialize;

use crate::schedulers;

#[derive(Row, Serialize)]
struct EditedMessage {
    date_time: u32,
    chat_id: i64,
    message_id: i64,
    original_message: String,
    message: String,
    diff: String,
    user_id: i64,
    client_id: u64,
}

pub async fn save_edited(
    message: &Message,
    client_id: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let chat_id = message.peer_id().bare_id_unchecked();
    let msg_id = message.id() as i64;
    let message_content = message.text().to_string();

    if message_content.is_empty() {
        return Ok(());
    }

    let clickhouse_client = schedulers::clickhouse_client()?;

    // Try to get the original from edited_log first, then from chats_log
    let original = clickhouse_client
        .query(
            "SELECT message FROM edited_log WHERE chat_id = ? AND message_id = ? ORDER BY date_time DESC LIMIT 1",
        )
        .bind(chat_id)
        .bind(msg_id)
        .fetch_one::<String>()
        .await
        .ok();

    let original = match original {
        Some(o) if !o.is_empty() => o,
        _ => clickhouse_client
            .query(
                "SELECT message FROM chats_log WHERE chat_id = ? AND message_id = ? ORDER BY date_time DESC LIMIT 1",
            )
            .bind(chat_id)
            .bind(msg_id)
            .fetch_one::<String>()
            .await
            .unwrap_or_default(),
    };

    if original.is_empty() || original == message_content {
        return Ok(());
    }

    let diff = unified_diff(&original, &message_content);

    let user_id = message
        .sender()
        .and_then(|s| s.id().bare_id())
        .unwrap_or(0) as i64;

    let chat_name = message
        .peer()
        .map(|p| p.name().unwrap_or_default().to_string())
        .unwrap_or_default();

    println!(
        "\x1b[93m{:<15} {:>5} {:<25}\n{}\x1b[0m",
        "edited",
        message.id(),
        &chat_name[..chat_name.len().min(25)],
        diff,
    );

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs() as u32;

    let mut insert = clickhouse_client
        .insert::<EditedMessage>("edited_log")
        .await?;
    insert
        .write(&EditedMessage {
            date_time: now,
            chat_id,
            message_id: msg_id,
            original_message: original,
            message: message_content,
            diff,
            user_id,
            client_id,
        })
        .await?;
    insert.end().await?;

    Ok(())
}

fn unified_diff(original: &str, modified: &str) -> String {
    let original_lines: Vec<&str> = original.lines().collect();
    let modified_lines: Vec<&str> = modified.lines().collect();

    let mut result = Vec::new();
    result.push("--- original".to_string());
    result.push("+++ modified".to_string());

    // Simple line-by-line diff
    let max_len = original_lines.len().max(modified_lines.len());
    let mut has_changes = false;

    for i in 0..max_len {
        match (original_lines.get(i), modified_lines.get(i)) {
            (Some(o), Some(m)) if o == m => {
                result.push(format!(" {o}"));
            }
            (Some(o), Some(m)) => {
                result.push(format!("-{o}"));
                result.push(format!("+{m}"));
                has_changes = true;
            }
            (Some(o), None) => {
                result.push(format!("-{o}"));
                has_changes = true;
            }
            (None, Some(m)) => {
                result.push(format!("+{m}"));
                has_changes = true;
            }
            (None, None) => {}
        }
    }

    if has_changes {
        result.join("\n")
    } else {
        String::new()
    }
}
