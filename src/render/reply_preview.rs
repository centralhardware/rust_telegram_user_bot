use crate::app::App;
use crate::render::console::{LogLine, Tone, emphasize};
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
/// One line, the message replied to: a comment that answers another comment is
/// an ordinary reply, and printing the post above it as well reads as if it
/// commented on the post instead. Only a top-level comment names the post, and
/// then the post *is* the target.
pub async fn format_reply_line(app: &App, message: &Message) -> String {
    let reply_id = match crate::telegram::reply_target::reply_target(message) {
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
    let target_chat_id = match crate::telegram::reply_target::reply_info(message).reply_to_chat_id {
        0 => message.peer_id().bare_id_unchecked(),
        foreign => foreign,
    };

    let target = lookup(app, target_chat_id, reply_id).await;

    render(app, reply_id, &target, header.quote_text.as_deref()).await
}

/// One preview line: the id in the message-id column, the chat it is in, who
/// sent it, and its text.
async fn render(app: &App, id: i32, target: &Target, quote_text: Option<&str>) -> String {
    let chat = source_title(app, target).await;
    let sender = if target.is_post() {
        "post"
    } else {
        target.sender.as_str()
    };
    let marker = if target.is_post() { "»" } else { ">" };

    let body = if target.text.is_empty() {
        format!("{marker} [{id}]")
    } else {
        // If there's a quote, highlight that portion within the full text.
        let text = match quote_text {
            Some(qt) => highlight_quote(&target.text, qt),
            None => target.text.clone(),
        };
        format!("{marker} {text}")
    };

    // No kind: the id sits in the message-id column of the line below it.
    LogLine::new(Tone::Muted, "", id)
        .chat(&chat)
        .sender(sender)
        .body(&body)
        .render()
}

/// What to call the chat a previewed message came from: for a copied channel
/// post that is the channel that published it, not the discussion group the
/// copy sits in.
async fn source_title(app: &App, target: &Target) -> String {
    if !target.is_post() {
        return target.chat_title.clone();
    }
    // peer_names is keyed by Bot API dialog id; the log keeps bare ids.
    let dialog_id = -1_000_000_000_000 - target.post_from_chat_id;
    match crate::state::peer_names::load(app, dialog_id).await {
        Some(n) if !n.title.is_empty() => n.title,
        _ => target.chat_title.clone(),
    }
}

async fn lookup(app: &App, chat_id: i64, message_id: i32) -> Target {
    // Incoming and outgoing now share one table, so one query covers both. The row
    // says who sent it, `peer_names` says what they are called.
    let Some(row) = app.db.find_reply_row(chat_id, message_id as i64).await else {
        return Target::default();
    };
    let crate::db::ReplyRow {
        message: text,
        user_id,
        chat_title,
        fwd_from_chat_id: fwd_chat,
        fwd_from_msg_id: fwd_msg,
    } = row;

    let sender = match crate::state::peer_names::load(app, user_id as i64).await {
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
            format!("{before}{}{after}", emphasize(quote, Tone::Muted))
        }
        None => text.to_string(),
    }
}
