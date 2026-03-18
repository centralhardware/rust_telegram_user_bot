use grammers_client::update::Message;
use grammers_tl_types as tl;

/// Extract text with markdown-like formatting markers applied from Telegram entities.
/// `code`, ```lang\n...\n```, ~~strike~~, __underline__, ||spoiler||, etc.
pub fn formatted_text(message: &Message) -> String {
    let text = message.text();
    if text.is_empty() {
        return String::new();
    }

    let entities = match message.fmt_entities() {
        Some(e) if !e.is_empty() => e,
        _ => return text.to_string(),
    };

    apply_entities(text, entities)
}

/// Returns (offset, length, open_marker, close_marker, nesting_priority).
/// Lower priority = outer wrapper (opens first, closes last).
fn entity_markers(entity: &tl::enums::MessageEntity) -> Option<(i32, i32, String, String, i32)> {
    match entity {
        tl::enums::MessageEntity::Blockquote(e) => Some((e.offset, e.length, "> ".into(), String::new(), 0)),
        tl::enums::MessageEntity::Spoiler(e) => Some((e.offset, e.length, "||".into(), "||".into(), 5)),
        tl::enums::MessageEntity::Strike(e) => Some((e.offset, e.length, "~~".into(), "~~".into(), 10)),
        tl::enums::MessageEntity::Underline(e) => Some((e.offset, e.length, "__".into(), "__".into(), 15)),
        tl::enums::MessageEntity::TextUrl(e) => Some((e.offset, e.length, "[".into(), format!("]({})", e.url), 60)),
        tl::enums::MessageEntity::Code(e) => Some((e.offset, e.length, "`".into(), "`".into(), 70)),
        tl::enums::MessageEntity::Pre(e) => {
            Some((e.offset, e.length, "```\n".into(), "\n```".into(), 80))
        }
        _ => None,
    }
}

fn apply_entities(text: &str, entities: &[tl::enums::MessageEntity]) -> String {
    // Telegram entities use UTF-16 offsets
    let utf16: Vec<u16> = text.encode_utf16().collect();

    // Collect tags to insert, keyed by UTF-16 position
    // (position, is_open, nesting_order, tag)
    // Nesting order ensures proper markdown nesting:
    //   opens:  outer entities first (longer span, then lower priority)
    //   closes: inner entities first (shorter span, then higher priority)
    let mut insertions: Vec<(usize, u8, i64, String)> = Vec::new();

    for entity in entities {
        if let Some((offset, length, open, close, priority)) = entity_markers(entity) {
            let start = offset as usize;
            let end = (offset + length) as usize;
            let len = length as i64;
            // opens: longer span first, then lower priority first → negate
            insertions.push((start, 1, -(len * 100 - priority as i64), open));
            if !close.is_empty() {
                // closes: shorter span first, then higher priority first
                insertions.push((end, 0, len * 100 - priority as i64, close));
            }
        }
    }

    insertions.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));

    // Build result by iterating UTF-16 positions
    let mut result = String::new();
    let mut ins_idx = 0;
    let mut pos: usize = 0;

    while pos <= utf16.len() {
        // Insert all markers at this position
        while ins_idx < insertions.len() && insertions[ins_idx].0 == pos {
            result.push_str(&insertions[ins_idx].3);
            ins_idx += 1;
        }

        if pos >= utf16.len() {
            break;
        }

        // Decode UTF-16 → char
        if (0xD800..=0xDBFF).contains(&utf16[pos])
            && pos + 1 < utf16.len()
            && (0xDC00..=0xDFFF).contains(&utf16[pos + 1])
        {
            let cp = 0x10000
                + ((utf16[pos] as u32 - 0xD800) << 10)
                + (utf16[pos + 1] as u32 - 0xDC00);
            result.push(char::from_u32(cp).unwrap_or('\u{FFFD}'));
            pos += 2;
        } else {
            result.push(char::from_u32(utf16[pos] as u32).unwrap_or('\u{FFFD}'));
            pos += 1;
        }
    }

    result
}
