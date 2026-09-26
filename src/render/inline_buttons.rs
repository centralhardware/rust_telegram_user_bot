use grammers_client::update::Message;
use grammers_tl_types as tl;

pub fn format_buttons(message: &Message) -> Option<String> {
    format_markup(extract_reply_markup(&message.raw)?)
}

/// The same, for markup that did not arrive on a `Message` — an ephemeral
/// message carries its own `reply_markup`.
pub fn format_markup(markup: &tl::enums::ReplyMarkup) -> Option<String> {
    let tl::enums::ReplyMarkup::ReplyInlineMarkup(inline) = markup else {
        return None;
    };

    let mut lines = Vec::new();
    for tl::enums::KeyboardInlineButtonRow::Row(row) in &inline.rows {
        let buttons: Vec<String> = row.buttons.iter().map(format_button).collect();
        if !buttons.is_empty() {
            lines.push(buttons.join(" "));
        }
    }

    if lines.is_empty() {
        None
    } else {
        Some(lines.join("\n"))
    }
}

/// A keyboard as the log stores it, one line per row: `[text]`, or
/// `[text → payload]` for a button that carries one. What an edit's console
/// line diffs when the buttons change.
pub fn format_stored(keyboard: &[crate::telegram::entities::Button]) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut row = None;
    for (r, text, _kind, payload) in keyboard {
        let button = if payload.is_empty() {
            format!("[{text}]")
        } else {
            format!("[{text} → {payload}]")
        };
        if row == Some(*r) {
            let last = lines.last_mut().expect("a row has a line");
            last.push(' ');
            last.push_str(&button);
        } else {
            lines.push(button);
            row = Some(*r);
        }
    }
    lines.join("\n")
}

pub fn extract_reply_markup(update: &tl::enums::Update) -> Option<&tl::enums::ReplyMarkup> {
    let msg = match update {
        tl::enums::Update::NewMessage(u) => &u.message,
        tl::enums::Update::NewChannelMessage(u) => &u.message,
        tl::enums::Update::EditMessage(u) => &u.message,
        tl::enums::Update::EditChannelMessage(u) => &u.message,
        tl::enums::Update::NewScheduledMessage(u) => &u.message,
        _ => return None,
    };
    match msg {
        tl::enums::Message::Message(m) => m.reply_markup.as_ref(),
        _ => None,
    }
}

fn format_button(button: &tl::enums::KeyboardInlineButton) -> String {
    use tl::enums::InlineButtonType as T;
    let tl::enums::KeyboardInlineButton::Button(b) = button;
    match &b.r#type {
        T::Url(t) => format!("[{} → {}]", b.text, t.url),
        T::UrlAuth(t) => format!("[{} → auth:{}]", b.text, t.url),
        T::InputInlineButtonTypeUrlAuth(t) => format!("[{} → auth:{}]", b.text, t.url),
        T::WebView(t) => format!("[{} → webview:{}]", b.text, t.url),
        T::Callback(_) => format!("[{} → cb]", b.text),
        T::Game => format!("[{} → game]", b.text),
        T::Buy => format!("[{} → buy]", b.text),
        T::SwitchInline(t) => {
            if t.query.is_empty() {
                format!("[{} → switch]", b.text)
            } else {
                format!("[{} → switch:{}]", b.text, t.query)
            }
        }
        T::UserProfile(t) => format!("[{} → user:{}]", b.text, t.user_id),
        T::InputInlineButtonTypeUserProfile(_) => format!("[{} → user]", b.text),
        T::Copy(t) => format!("[{} → copy:{}]", b.text, t.copy_text),
        T::Disabled => format!("[{}]", b.text),
    }
}

#[cfg(test)]
mod tests {
    use super::format_stored;

    #[test]
    fn a_stored_keyboard_prints_one_line_per_row() {
        let keyboard = vec![
            (
                0,
                "Open".to_string(),
                "url".to_string(),
                "https://example.com".to_string(),
            ),
            (0, "Vote".to_string(), "callback".to_string(), String::new()),
            (1, "Next".to_string(), "callback".to_string(), String::new()),
        ];
        assert_eq!(
            format_stored(&keyboard),
            "[Open → https://example.com] [Vote]\n[Next]"
        );
    }
}
