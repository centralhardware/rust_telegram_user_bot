use grammers_client::message::Message;
use grammers_tl_types as tl;

/// A logged message, as the preview above a reply needs it.
#[derive(Default)]
pub struct Target {
    pub text: String,
    /// Who sent it — empty for a channel post, which has no sender.
    pub sender: String,
    /// The chat it lives in, as the log names it.
    pub chat_title: String,
    /// The channel a copied post came from, 0 when the message is not one.
    /// Telegram builds a comment section out of replies to the copy of the post
    /// in the discussion group, so this is what a comment ultimately answers.
    pub post_from_chat_id: i64,
}

impl Target {
    fn is_post(&self) -> bool {
        self.post_from_chat_id != 0
    }
}

/// Format reply line for log messages.
/// Returns a line to print *above* the message, or empty string if no reply.
///
/// A comment on a channel post gets two lines: the post it comments on, then
/// the comment it answers — the post is what gives the rest its meaning, and
/// with only the ids the thread reads as a pair of bare numbers.
pub async fn format_reply_line(message: &Message) -> String {
    let reply_id = match crate::utils::reply_target::reply_target(message) {
        Some(id) => id,
        None => return String::new(),
    };
    let header = match message.reply_header() {
        Some(tl::enums::MessageReplyHeader::Header(header)) => header,
        _ => return String::new(),
    };

    // The target lives in the chat the header names when it names one — a
    // comment sent from the Replies pseudo-chat, or a quote of another chat —
    // and in this chat otherwise. Looking it up here would find nothing.
    let target_chat_id = match crate::utils::reply_target::reply_info(message).reply_to_chat_id {
        0 => message.peer_id().bare_id_unchecked(),
        foreign => foreign,
    };

    let target = lookup(target_chat_id, reply_id).await;

    let mut lines = Vec::new();

    // The post above the comment, when the comment answers another comment and
    // the post itself is a message further up the thread.
    if let Some(top) = header.reply_to_top_id.filter(|top| *top != reply_id) {
        let post = lookup(target_chat_id, top).await;
        if post.is_post() {
            lines.push(render(top, &post, None).await);
        }
    }

    lines.push(render(reply_id, &target, header.quote_text.as_deref()).await);
    lines.join("\n")
}

/// One preview line: the id in the message-id column, the chat it is in, who
/// sent it, and its text.
async fn render(id: i32, target: &Target, quote_text: Option<&str>) -> String {
    // Place the id in the same {:>8} column as the message id in incoming log
    // lines. Layout: {:<8}(8) + ' '(1) + {:>8}(8) + ' '(1) + {:<25}(25) + ' '(1)
    // = 44 before first │. Text column starts at
    // 44 + │(1) + ' '(1) + {:<10}(10) + ' '(1) + │(1) + ' '(1) + '> '(2) = 61
    let chat_short: String = source_title(target).await.chars().take(25).collect();
    let id_col = format!("{:<8} \x1b[90m{:>8}\x1b[0m {:<25} ", "", id, chat_short);
    let pad_text = " ".repeat(60);

    let sender_short: String = if target.is_post() {
        "post".to_string()
    } else {
        target.sender.chars().take(10).collect()
    };
    let marker = if target.is_post() { "»" } else { ">" };

    if target.text.is_empty() {
        return format!("{id_col}\x1b[90m│ {sender_short:<10} │ {marker} [{id}]\x1b[0m");
    }

    // If there's a quote, highlight that portion within the full text
    let highlighted = match quote_text {
        Some(qt) => highlight_quote(&target.text, qt),
        None => target.text.clone(),
    };
    let formatted = highlighted
        .lines()
        .enumerate()
        .map(|(i, line)| {
            if i == 0 {
                format!("{id_col}\x1b[90m│ {sender_short:<10} │ {marker} {line}")
            } else {
                format!("{pad_text}\x1b[90m    {line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!("{formatted}\x1b[0m")
}

/// What to call the chat a previewed message came from: for a copied channel
/// post that is the channel that published it, not the discussion group the
/// copy sits in.
async fn source_title(target: &Target) -> String {
    if !target.is_post() {
        return target.chat_title.clone();
    }
    // peer_names is keyed by Bot API dialog id; the log keeps bare ids.
    let dialog_id = -1_000_000_000_000 - target.post_from_chat_id;
    match crate::utils::peer_names::load(dialog_id).await {
        Some(n) if !n.title.is_empty() => n.title,
        _ => target.chat_title.clone(),
    }
}

async fn lookup(chat_id: i64, message_id: i32) -> Target {
    // Check the unflushed buffer first
    let from_buf = crate::db::EVENTS_BUF
        .find_last(|m| {
            if m.event == crate::db::SEND && m.chat_id == chat_id && m.message_id == message_id as i64 {
                let sender = if m.second_name.is_empty() {
                    m.first_name.clone()
                } else {
                    format!("{} {}", m.first_name, m.second_name)
                };
                Some(Target {
                    text: m.message.clone(),
                    sender,
                    chat_title: m.chat_title.clone(),
                    post_from_chat_id: post_from(m.user_id, m.fwd_from_chat_id, m.fwd_from_msg_id),
                })
            } else {
                None
            }
        })
        .await;
    if let Some(target) = from_buf {
        return target;
    }

    // Incoming and outgoing now share one table, so one query covers both. The row
    // says who sent it, `peer_names` says what they are called.
    let Ok((text, user_id, chat_title, fwd_chat, fwd_msg)) = crate::db::clickhouse()
        .query(
            "SELECT message, user_id, chat_title, fwd_from_chat_id, fwd_from_msg_id \
             FROM events_log \
             WHERE chat_id = ? AND message_id = ? AND event = ? \
             ORDER BY date_time DESC LIMIT 1",
        )
        .bind(chat_id)
        .bind(message_id as i64)
        .bind(crate::db::SEND)
        .fetch_one::<(String, u64, String, i64, i64)>()
        .await
    else {
        return Target::default();
    };

    let sender = match crate::utils::peer_names::load(user_id as i64).await {
        Some(n) if !n.last_name.is_empty() => format!("{} {}", n.first_name, n.last_name),
        Some(n) => n.first_name,
        None => String::new(),
    };
    Target {
        text,
        sender,
        chat_title,
        post_from_chat_id: post_from(user_id, fwd_chat, fwd_msg),
    }
}

/// The channel a message was copied from, when the message is the copy of a
/// channel post in its discussion group — no sender, and a forward header
/// naming both the channel and the post.
fn post_from(user_id: u64, fwd_chat_id: i64, fwd_msg_id: i64) -> i64 {
    if user_id == 0 && fwd_chat_id != 0 && fwd_msg_id != 0 {
        fwd_chat_id
    } else {
        0
    }
}

/// Highlight the quote portion within the full text using cyan color
fn highlight_quote(text: &str, quote: &str) -> String {
    match text.find(quote) {
        Some(pos) => {
            let before = &text[..pos];
            let after = &text[pos + quote.len()..];
            format!("{before}\x1b[96m{quote}\x1b[90m{after}")
        }
        None => text.to_string(),
    }
}
