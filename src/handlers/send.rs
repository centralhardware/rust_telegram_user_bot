//! What a new message's send row and console line are made of, whoever sent
//! it. `incoming` and `outgoing` differ only in who the sender is and how the
//! chat gets its name; everything else is built here, once, so a new column
//! is added in one place.

use grammers_client::update::Message;
use log::info;

use crate::render::console::{LogLine, Tone};

use super::extract::ChatInfo;
use crate::db::Event;
use crate::app::App;

/// The body of a message, described once for the console line and the row
/// alike.
pub(super) struct Body {
    /// The text with its formatting rendered in, for the console.
    text: String,
    media_desc: Option<String>,
    /// A service message's action, spelled out. Only when there is no text.
    action_desc: Option<String>,
    buttons: Option<String>,
}

impl Body {
    /// `sender_id` and `sender_name` are who a service action is said to be
    /// performed by.
    pub(super) async fn of(
        app: &App,
        message: &Message,
        sender_id: Option<i64>,
        sender_name: Option<&str>,
    ) -> Self {
        let text = crate::render::format_entities::formatted_text(message);
        let game_title =
            crate::telegram::service_action::game_title(&app.tg, std::ops::Deref::deref(message)).await;
        let action_desc = match message.action() {
            Some(a) if text.is_empty() => Some(crate::telegram::service_action::format(
                a,
                sender_id,
                sender_name,
                game_title.as_deref(),
            )),
            _ => None,
        };
        Body {
            text,
            media_desc: crate::telegram::media_description::describe(message),
            action_desc,
            buttons: crate::render::inline_buttons::format_buttons(message),
        }
    }

    /// The message as the console line shows it: the media and the text, or
    /// the action, or the media alone, then the buttons.
    fn preview(&self) -> String {
        let mut preview = if !self.text.is_empty() {
            match &self.media_desc {
                Some(desc) => format!("{} {}", desc, self.text),
                None => self.text.clone(),
            }
        } else if let Some(desc) = &self.action_desc {
            desc.clone()
        } else {
            self.media_desc.clone().unwrap_or_default()
        };
        if let Some(b) = &self.buttons {
            if !preview.is_empty() {
                preview.push_str("\n\n");
            }
            preview.push_str(b);
        }
        preview
    }

    /// The message as its row keeps it: the text as the sender wrote it -- its
    /// formatting and its buttons are columns of their own, and the console
    /// line is where they are rendered back onto it -- or, with no text, the
    /// action or the media it carries.
    fn content(&self, message: &Message) -> String {
        let plain = crate::render::format_entities::plain_text(message);
        if !plain.is_empty() {
            plain
        } else if let Some(desc) = &self.action_desc {
            desc.clone()
        } else {
            self.media_desc.clone().unwrap_or_default()
        }
    }
}

/// Print the console line for a new message, and the line for what it
/// replies to above it.
pub(super) async fn print(
    app: &App,
    message: &Message,
    body: &Body,
    (label, tone): (&str, Tone),
    chat_title: &str,
    sender_name: &str,
) {
    let topic_name = crate::state::topic::topic_name(app, message).await;
    let title = if topic_name.is_empty() {
        chat_title.to_string()
    } else {
        format!("{chat_title} / {topic_name}")
    };

    let reply_line = crate::render::reply_preview::format_reply_line(app, message).await;
    if !reply_line.is_empty() {
        info!("{}", reply_line);
    }
    LogLine::new(tone, label, message.id())
        .chat(&title)
        .sender(sender_name)
        .body(&body.preview())
        .print();
}

/// The send row for a message, with everything but its sender filled in: the
/// caller sets who sent it.
///
/// A service message — a join, a title change, a call — is a message like any
/// other: an id, a sender, a date and a place in the history. It is logged as
/// one, and `action` is what says it announces something rather than carrying
/// what someone wrote.
pub(super) async fn event(app: &App, message: &Message, body: &Body, chat: ChatInfo) -> Event {
    let chat_id = message.peer_id().bare_id_unchecked();
    let mut reply = crate::telegram::reply_target::reply_info(message);
    let reply_to_user_id = crate::db::resolve_reply(&*app.db, chat_id, &mut reply).await;
    let (topic_id, topic_name) = crate::state::topic::topic_of(app, message).await;

    let meta = crate::telegram::media_description::media_meta(message).unwrap_or_default();
    let meta_msg = crate::telegram::message_meta::of(&std::ops::Deref::deref(message).raw);

    Event {
        date_time: message.date().as_second() as u32,
        message: body.content(message),
        entities: crate::telegram::entities::of_message(message),
        keyboard: crate::telegram::entities::keyboard_of_message(message),
        chat_title: chat.chat_title,
        chat_id,
        chat_usernames: chat.chat_usernames,
        community_id: chat.community_id,
        message_id: message.id() as i64,
        reply_to: reply.reply_to,
        reply_to_user_id,
        reply_to_chat_id: reply.reply_to_chat_id,
        quote_text: reply.quote_text,
        comment_to: reply.comment_to,
        topic_id,
        topic_name,
        raw: serde_json::to_string(&message.raw).unwrap_or_default(),
        media_type: meta.media_type,
        file_name: meta.file_name,
        mime_type: meta.mime_type,
        size: meta.size,
        fwd_from_user_id: meta_msg.fwd_from_user_id,
        fwd_from_chat_id: meta_msg.fwd_from_chat_id,
        fwd_from_msg_id: meta_msg.fwd_from_msg_id,
        fwd_from_name: meta_msg.fwd_from_name,
        fwd_date: meta_msg.fwd_date,
        action: meta_msg.action,
        grouped_id: meta_msg.grouped_id,
        via_bot_id: meta_msg.via_bot_id,
        guest_from_id: meta_msg.guest_from_id,
        post_author: meta_msg.post_author,
        pinned: meta_msg.pinned,
        silent: meta_msg.silent,
        noforwards: meta_msg.noforwards,
        ttl_period: meta_msg.ttl_period,
        duration: meta.duration,
        width: meta.width,
        height: meta.height,
        lat: meta.lat,
        lon: meta.lon,
        poll_question: meta.poll_question,
        poll_options: meta.poll_options,
        poll_id: meta.poll_id,
        ..Event::of(crate::db::EventKind::Send)
    }
}
