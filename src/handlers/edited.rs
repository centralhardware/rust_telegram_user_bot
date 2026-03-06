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
    let colored = colored_inline_diff(&original, &message_content);
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

fn colored_inline_diff(original: &str, modified: &str) -> String {
    use similar::{ChangeTag, TextDiff};

    let line_diff = TextDiff::from_lines(original, modified);
    let changes: Vec<_> = line_diff.iter_all_changes().collect();
    let mut result = String::new();

    let mut i = 0;
    while i < changes.len() {
        match changes[i].tag() {
            ChangeTag::Equal => {
                i += 1;
            }
            ChangeTag::Delete => {
                let del_start = i;
                while i < changes.len() && changes[i].tag() == ChangeTag::Delete {
                    i += 1;
                }
                let ins_start = i;
                while i < changes.len() && changes[i].tag() == ChangeTag::Insert {
                    i += 1;
                }
                let del_lines = &changes[del_start..ins_start];
                let ins_lines = &changes[ins_start..i];
                let pair_count = del_lines.len().min(ins_lines.len());

                for j in 0..pair_count {
                    let old_val = del_lines[j].value().trim_end_matches('\n');
                    let new_val = ins_lines[j].value().trim_end_matches('\n');
                    let char_diff = TextDiff::from_chars(old_val, new_val);

                    let mut old_buf = String::from("- ");
                    let mut new_buf = String::from("+ ");

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

                    old_buf += "\n";
                    new_buf += "\n";
                    result += &old_buf;
                    result += &new_buf;
                }

                for j in pair_count..del_lines.len() {
                    result += &format!(
                        "- \x1b[31m{}\x1b[0m\n",
                        del_lines[j].value().trim_end_matches('\n')
                    );
                }
                for j in pair_count..ins_lines.len() {
                    result += &format!(
                        "+ \x1b[32m{}\x1b[0m\n",
                        ins_lines[j].value().trim_end_matches('\n')
                    );
                }
            }
            ChangeTag::Insert => {
                result += &format!(
                    "+ \x1b[32m{}\x1b[0m\n",
                    changes[i].value().trim_end_matches('\n')
                );
                i += 1;
            }
        }
    }

    result.trim_end().to_string()
}
