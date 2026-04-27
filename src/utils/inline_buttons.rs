use grammers_client::update::Message;
use grammers_tl_types as tl;

pub fn format_buttons(message: &Message) -> Option<String> {
    let markup = extract_reply_markup(&message.raw)?;
    let tl::enums::ReplyMarkup::ReplyInlineMarkup(inline) = markup else {
        return None;
    };

    let mut lines = Vec::new();
    for tl::enums::KeyboardButtonRow::Row(row) in &inline.rows {
        let buttons: Vec<String> = row.buttons.iter().map(format_button).collect();
        if !buttons.is_empty() {
            lines.push(buttons.join(" "));
        }
    }

    if lines.is_empty() { None } else { Some(lines.join("\n")) }
}

fn extract_reply_markup(update: &tl::enums::Update) -> Option<&tl::enums::ReplyMarkup> {
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

fn format_button(button: &tl::enums::KeyboardButton) -> String {
    use tl::enums::KeyboardButton::*;
    match button {
        Button(b) => format!("[{}]", b.text),
        Url(b) => format!("[{} → {}]", b.text, b.url),
        Callback(b) => format!("[{} → cb]", b.text),
        RequestPhone(b) => format!("[{} → phone]", b.text),
        RequestGeoLocation(b) => format!("[{} → geo]", b.text),
        SwitchInline(b) => {
            if b.query.is_empty() {
                format!("[{} → switch]", b.text)
            } else {
                format!("[{} → switch:{}]", b.text, b.query)
            }
        }
        Game(b) => format!("[{} → game]", b.text),
        Buy(b) => format!("[{} → buy]", b.text),
        UrlAuth(b) => format!("[{} → auth:{}]", b.text, b.url),
        InputKeyboardButtonUrlAuth(b) => format!("[{} → auth:{}]", b.text, b.url),
        RequestPoll(b) => format!("[{} → poll]", b.text),
        UserProfile(b) => format!("[{} → user:{}]", b.text, b.user_id),
        InputKeyboardButtonUserProfile(b) => format!("[{} → user]", b.text),
        WebView(b) => format!("[{} → webview:{}]", b.text, b.url),
        SimpleWebView(b) => format!("[{} → webview:{}]", b.text, b.url),
        RequestPeer(b) => format!("[{} → peer]", b.text),
        InputKeyboardButtonRequestPeer(b) => format!("[{} → peer]", b.text),
        Copy(b) => format!("[{} → copy:{}]", b.text, b.copy_text),
    }
}
