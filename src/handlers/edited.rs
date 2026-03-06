use grammers_client::update::Message;
use log::info;

use crate::db::EditedMessage;

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

    // Check buffers first — data may not be flushed to DB yet
    let original = if let Some(msg) = crate::db::EDITED_BUF
        .find_last(|e| {
            (e.chat_id == chat_id && e.message_id == msg_id).then(|| e.message.clone())
        })
        .await
    {
        msg
    } else {
        let ch = crate::db::clickhouse();
        let db_result = ch
            .query(
                "SELECT message FROM (\
                    SELECT message, 1 AS p, date_time FROM edited_log WHERE chat_id = ? AND message_id = ? \
                    UNION ALL \
                    SELECT message, 2 AS p, date_time FROM chats_log WHERE chat_id = ? AND message_id = ? \
                ) ORDER BY p, date_time DESC LIMIT 1",
            )
            .bind(chat_id)
            .bind(msg_id)
            .bind(chat_id)
            .bind(msg_id)
            .fetch_one::<String>()
            .await
            .unwrap_or_default();

        if db_result.is_empty() {
            // Message might still be in the incoming buffer
            crate::db::INCOMING_BUF
                .find_last(|m| {
                    (m.chat_id == chat_id && m.message_id == msg_id).then(|| m.message.clone())
                })
                .await
                .unwrap_or_default()
        } else {
            db_result
        }
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

    let chat_name_short: String = chat_name.chars().take(25).collect();
    let colored = colorize_unified_diff(&diff, &original, &message_content);
    info!(
        "\x1b[93m{:<15} {:>5} {:<25}\x1b[0m\n{}",
        "edited",
        message.id(),
        chat_name_short,
        colored,
    );

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs() as u32;

    crate::db::EDITED_BUF.push(EditedMessage {
        date_time: now,
        chat_id,
        message_id: msg_id,
        original_message: original,
        message: message_content,
        diff,
        user_id,
        client_id,
    }).await;

    Ok(())
}

fn unified_diff(original: &str, modified: &str) -> String {
    similar::TextDiff::from_lines(original, modified)
        .unified_diff()
        .missing_newline_hint(false)
        .to_string()
}

fn colorize_unified_diff(diff: &str, original: &str, modified: &str) -> String {
    use similar::{ChangeTag, TextDiff};

    let lines: Vec<&str> = diff.lines().collect();
    let mut result = String::new();

    // Collect paired old/new lines from original and modified for char-level diff
    let orig_lines: Vec<&str> = original.lines().collect();
    let mod_lines: Vec<&str> = modified.lines().collect();
    let line_diff = TextDiff::from_lines(original, modified);
    let changes: Vec<_> = line_diff.iter_all_changes().collect();

    // Build a map: for paired del/ins lines, store char-level colored versions
    let mut colored_del: Vec<String> = Vec::new();
    let mut colored_ins: Vec<String> = Vec::new();

    let mut i = 0;
    while i < changes.len() {
        match changes[i].tag() {
            ChangeTag::Equal => { i += 1; }
            ChangeTag::Delete => {
                let del_start = i;
                while i < changes.len() && changes[i].tag() == ChangeTag::Delete { i += 1; }
                let ins_start = i;
                while i < changes.len() && changes[i].tag() == ChangeTag::Insert { i += 1; }

                let dels = &changes[del_start..ins_start];
                let inss = &changes[ins_start..i];
                let pair_count = dels.len().min(inss.len());

                for j in 0..pair_count {
                    let old_val = dels[j].value().trim_end_matches('\n');
                    let new_val = inss[j].value().trim_end_matches('\n');
                    let char_diff = TextDiff::from_chars(old_val, new_val);

                    let mut old_buf = String::new();
                    let mut new_buf = String::new();
                    for c in char_diff.iter_all_changes() {
                        match c.tag() {
                            ChangeTag::Equal => {
                                old_buf += c.value();
                                new_buf += c.value();
                            }
                            ChangeTag::Delete => {
                                old_buf += "\x1b[31m";
                                old_buf += c.value();
                                old_buf += "\x1b[0m";
                            }
                            ChangeTag::Insert => {
                                new_buf += "\x1b[32m";
                                new_buf += c.value();
                                new_buf += "\x1b[0m";
                            }
                        }
                    }
                    colored_del.push(format!("-{old_buf}"));
                    colored_ins.push(format!("+{new_buf}"));
                }
                for j in pair_count..dels.len() {
                    colored_del.push(format!(
                        "-\x1b[31m{}\x1b[0m",
                        dels[j].value().trim_end_matches('\n')
                    ));
                }
                for j in pair_count..inss.len() {
                    colored_ins.push(format!(
                        "+\x1b[32m{}\x1b[0m",
                        inss[j].value().trim_end_matches('\n')
                    ));
                }
            }
            ChangeTag::Insert => {
                colored_ins.push(format!(
                    "+\x1b[32m{}\x1b[0m",
                    changes[i].value().trim_end_matches('\n')
                ));
                i += 1;
            }
        }
    }

    // Now walk the unified diff lines and replace -/+ lines with colored versions
    let mut del_idx = 0;
    let mut ins_idx = 0;
    for line in &lines {
        if line.starts_with('-') && !line.starts_with("---") {
            if del_idx < colored_del.len() {
                result += &colored_del[del_idx];
                del_idx += 1;
            } else {
                result += line;
            }
        } else if line.starts_with('+') && !line.starts_with("+++") {
            if ins_idx < colored_ins.len() {
                result += &colored_ins[ins_idx];
                ins_idx += 1;
            } else {
                result += line;
            }
        } else {
            result += line;
        }
        result += "\n";
    }

    result.trim_end().to_string()
}
