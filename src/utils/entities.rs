//! Entities and inline keyboards, stored as they came rather than baked into the
//! text.
//!
//! `message` holds what the sender typed, byte for byte. What Telegram draws over
//! it — bold, a link, a spoiler — is the `entities` array beside it, and the
//! buttons under it the `keyboard` array. Rendering the three back together is a
//! read-time job: the console line does it with `format_entities`, a board does it
//! with the `render_message_html` UDF.
//!
//! Both are ClickHouse arrays of named tuples rather than JSON in a String, so a
//! query can reach into them — `arrayExists(e -> e.type = 'spoiler', entities)` —
//! without parsing anything. Offsets and lengths are UTF-16 code units, as
//! Telegram counts them, and the type names are the Bot API's. Whatever the four
//! columns of a tuple cannot hold stays in `raw`, which keeps the MTProto object
//! for every row.

use grammers_tl_types as tl;

/// One entity: what it is, the span it covers, and the one thing it carries
/// besides — a link's target, a mention's account, a code block's language.
/// Empty for the entities that carry nothing, which is most of them.
pub type Entity = (String, u32, u32, String);

/// One button: the row it sits in, its label, what it does, and where it leads.
pub type Button = (u8, String, String, String);

/// The entity list as the log stores it.
pub fn entities(entities: Option<&[tl::enums::MessageEntity]>) -> Vec<Entity> {
    entities
        .unwrap_or_default()
        .iter()
        .map(|entity| {
            (
                type_name(entity).to_string(),
                entity.offset().max(0) as u32,
                entity.length().max(0) as u32,
                payload(entity),
            )
        })
        .collect()
}

/// The entities of a message. A rich message (layer 228+) carries its body as
/// PageBlocks and its `entities` describe only the plain fallback, so it has
/// none: what `message` holds there is already the rendered rich text.
pub fn of_message(message: &grammers_client::message::Message) -> Vec<Entity> {
    if crate::utils::rich_message::rich_text(message).is_some() {
        return Vec::new();
    }
    entities(message.fmt_entities().map(Vec::as_slice))
}

fn payload(entity: &tl::enums::MessageEntity) -> String {
    use tl::enums::MessageEntity as E;
    match entity {
        E::Pre(e) => e.language.clone(),
        E::TextUrl(e) => e.url.clone(),
        E::MentionName(e) => e.user_id.to_string(),
        E::InputMessageEntityMentionName(_) => String::new(),
        E::CustomEmoji(e) => e.document_id.to_string(),
        E::Blockquote(e) if e.collapsed => "collapsed".to_string(),
        _ => String::new(),
    }
}

fn type_name(entity: &tl::enums::MessageEntity) -> &'static str {
    use tl::enums::MessageEntity as E;
    match entity {
        E::Mention(_) => "mention",
        E::Hashtag(_) => "hashtag",
        E::BotCommand(_) => "bot_command",
        E::Url(_) => "url",
        E::Email(_) => "email",
        E::Bold(_) => "bold",
        E::Italic(_) => "italic",
        E::Code(_) => "code",
        E::Pre(_) => "pre",
        E::TextUrl(_) => "text_link",
        E::MentionName(_) | E::InputMessageEntityMentionName(_) => "text_mention",
        E::Phone(_) => "phone_number",
        E::Cashtag(_) => "cashtag",
        E::Underline(_) => "underline",
        E::Strike(_) => "strikethrough",
        E::BankCard(_) => "bank_card",
        E::Spoiler(_) => "spoiler",
        E::CustomEmoji(_) => "custom_emoji",
        E::Blockquote(_) => "blockquote",
        E::FormattedDate(_) => "formatted_date",
        E::DiffInsert(_) => "diff_insert",
        E::DiffReplace(_) => "diff_replace",
        E::DiffDelete(_) => "diff_delete",
        E::Unknown(_) => "unknown",
    }
}

/// The inline keyboard as the log stores it — the rows flattened, each button
/// naming the row it sits in. Only inline keyboards: a reply keyboard belongs to
/// the chat rather than to the message.
pub fn keyboard(markup: Option<&tl::enums::ReplyMarkup>) -> Vec<Button> {
    let Some(tl::enums::ReplyMarkup::ReplyInlineMarkup(inline)) = markup else {
        return Vec::new();
    };

    inline
        .rows
        .iter()
        .enumerate()
        .flat_map(|(index, tl::enums::KeyboardInlineButtonRow::Row(row))| {
            row.buttons.iter().map(move |button| {
                let tl::enums::KeyboardInlineButton::Button(b) = button;
                let (kind, payload) = button_kind(&b.r#type);
                (index.min(u8::MAX as usize) as u8, b.text.clone(), kind.to_string(), payload)
            })
        })
        .collect()
}

/// The keyboard of a message that arrived on an update.
pub fn keyboard_of_message(message: &grammers_client::update::Message) -> Vec<Button> {
    keyboard(crate::utils::inline_buttons::extract_reply_markup(&message.raw))
}

/// The same, for a message that did not arrive on an update — a backfill fetches
/// the message object on its own.
pub fn keyboard_of_raw(msg: &tl::enums::Message) -> Vec<Button> {
    match msg {
        tl::enums::Message::Message(m) => keyboard(m.reply_markup.as_ref()),
        _ => Vec::new(),
    }
}

fn button_kind(kind: &tl::enums::InlineButtonType) -> (&'static str, String) {
    use tl::enums::InlineButtonType as T;
    match kind {
        T::Url(t) => ("url", t.url.clone()),
        T::UrlAuth(t) => ("login_url", t.url.clone()),
        T::InputInlineButtonTypeUrlAuth(t) => ("login_url", t.url.clone()),
        T::WebView(t) => ("web_app", t.url.clone()),
        T::Callback(_) => ("callback", String::new()),
        T::Game => ("game", String::new()),
        T::Buy => ("buy", String::new()),
        T::SwitchInline(t) => ("switch_inline", t.query.clone()),
        T::UserProfile(t) => ("user", t.user_id.to_string()),
        T::InputInlineButtonTypeUserProfile(_) => ("user", String::new()),
        T::Copy(t) => ("copy", t.copy_text.clone()),
        T::Disabled => ("disabled", String::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bold(offset: i32, length: i32) -> tl::enums::MessageEntity {
        tl::types::MessageEntityBold { offset, length }.into()
    }

    #[test]
    fn no_entities_is_an_empty_array() {
        assert!(entities(None).is_empty());
        assert!(entities(Some(&[])).is_empty());
    }

    #[test]
    fn a_plain_entity_carries_nothing_besides_its_span() {
        assert_eq!(
            entities(Some(&[bold(2, 3)])),
            vec![("bold".to_string(), 2, 3, String::new())]
        );
    }

    #[test]
    fn a_text_url_carries_its_target() {
        let link: tl::enums::MessageEntity = tl::types::MessageEntityTextUrl {
            offset: 0,
            length: 2,
            url: "https://example.com".into(),
        }
        .into();
        assert_eq!(
            entities(Some(&[link])),
            vec![("text_link".to_string(), 0, 2, "https://example.com".to_string())]
        );
    }

    #[test]
    fn a_mention_carries_the_account_it_names() {
        let mention: tl::enums::MessageEntity = tl::types::MessageEntityMentionName {
            offset: 0,
            length: 3,
            user_id: 42,
        }
        .into();
        assert_eq!(
            entities(Some(&[mention])),
            vec![("text_mention".to_string(), 0, 3, "42".to_string())]
        );
    }

    #[test]
    fn no_markup_is_an_empty_array() {
        assert!(keyboard(None).is_empty());
    }

    #[test]
    fn a_keyboard_remembers_which_row_a_button_sits_in() {
        let row = |text: &str, kind: tl::enums::InlineButtonType| {
            tl::enums::KeyboardInlineButtonRow::from(tl::types::KeyboardInlineButtonRow {
                buttons: vec![tl::types::KeyboardInlineButton {
                    style: None,
                    text: text.into(),
                    r#type: kind,
                }
                .into()],
            })
        };
        let markup: tl::enums::ReplyMarkup = tl::types::ReplyInlineMarkup {
            force_reply: false,
            rows: vec![
                row(
                    "Open",
                    tl::types::InlineButtonTypeUrl {
                        url: "https://example.com".into(),
                    }
                    .into(),
                ),
                row(
                    "Nope",
                    tl::types::InlineButtonTypeCallback {
                        requires_password: false,
                        data: vec![1],
                    }
                    .into(),
                ),
            ],
        }
        .into();
        assert_eq!(
            keyboard(Some(&markup)),
            vec![
                (0, "Open".to_string(), "url".to_string(), "https://example.com".to_string()),
                (1, "Nope".to_string(), "callback".to_string(), String::new()),
            ]
        );
    }
}
