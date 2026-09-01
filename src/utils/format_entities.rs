use grammers_client::message::Message;
use grammers_tl_types as tl;

/// Extract text with markdown-like formatting markers applied from Telegram entities.
/// `code`, ```lang\n...\n```, s̶t̶r̶i̶k̶e̶, u̲n̲d̲e̲r̲l̲i̲n̲e̲, ||spoiler||, etc.
pub fn formatted_text(message: &Message) -> String {
    // Rich messages (layer 228+) carry their body as PageBlocks; `message`/`entities`
    // only hold a plain fallback, so render the rich payload when it is there.
    if let Some(rich) = crate::utils::rich_message::rich_text(message) {
        return rich;
    }

    let text = message.text();
    if text.is_empty() {
        return String::new();
    }

    render(text, message.fmt_entities().map(Vec::as_slice))
}

/// Combining marks that draw over the preceding character, so the style survives as
/// plain text instead of being carried by `~~`/`__` markers. Unicode has no combining
/// equivalent for bold, italic or spoiler, so those keep their markers.
const STRIKE_OVERLAY: char = '\u{0336}'; // COMBINING LONG STROKE OVERLAY
const UNDERLINE_OVERLAY: char = '\u{0332}'; // COMBINING LOW LINE

/// Apply a combining `mark` to every character of `text`. Line breaks are left alone —
/// an overlay on a newline renders as a stray dash at the end of the line.
fn overlay(text: &str, mark: char) -> String {
    let mut out = String::with_capacity(text.len() * 3);
    for c in text.chars() {
        out.push(c);
        if c != '\n' && c != '\r' {
            out.push(mark);
        }
    }
    out
}

pub fn strike(text: &str) -> String {
    overlay(text, STRIKE_OVERLAY)
}

pub fn underline(text: &str) -> String {
    overlay(text, UNDERLINE_OVERLAY)
}

/// The same, for text that did not arrive on a `Message` — an ephemeral message
/// carries its own `message` and `entities`.
pub fn render(text: &str, entities: Option<&[tl::enums::MessageEntity]>) -> String {
    match entities {
        Some(entities) if !entities.is_empty() => apply_entities(text, entities),
        _ => text.to_string(),
    }
}

/// Returns (offset, length, open_marker, close_marker, nesting_priority).
/// Lower priority = outer wrapper (opens first, closes last).
fn entity_markers(entity: &tl::enums::MessageEntity) -> Option<(i32, i32, String, String, i32)> {
    match entity {
        tl::enums::MessageEntity::Blockquote(e) => Some((e.offset, e.length, "> ".into(), String::new(), 0)),
        tl::enums::MessageEntity::Spoiler(e) => Some((e.offset, e.length, "||".into(), "||".into(), 5)),
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
    // Strike and underline are not wrappers: they are applied per character, so they
    // are kept apart from the marker table as UTF-16 ranges carrying a combining mark.
    let mut overlays: Vec<(usize, usize, char)> = Vec::new();

    for entity in entities {
        match entity {
            tl::enums::MessageEntity::Strike(e) => overlays.push((
                e.offset as usize,
                (e.offset + e.length) as usize,
                STRIKE_OVERLAY,
            )),
            tl::enums::MessageEntity::Underline(e) => overlays.push((
                e.offset as usize,
                (e.offset + e.length) as usize,
                UNDERLINE_OVERLAY,
            )),
            _ => {}
        }
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
            result.push_str(&marks_at(&overlays, pos));
            pos += 2;
        } else {
            let c = char::from_u32(utf16[pos] as u32).unwrap_or('\u{FFFD}');
            result.push(c);
            if c != '\n' && c != '\r' {
                result.push_str(&marks_at(&overlays, pos));
            }
            pos += 1;
        }
    }

    result
}

/// Every combining mark covering this UTF-16 position, in entity order — a span that is
/// both struck and underlined gets both marks stacked on each character.
fn marks_at(overlays: &[(usize, usize, char)], pos: usize) -> String {
    overlays
        .iter()
        .filter(|&&(start, end, _)| pos >= start && pos < end)
        .map(|&(_, _, mark)| mark)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strike_entity(offset: i32, length: i32) -> tl::enums::MessageEntity {
        tl::types::MessageEntityStrike { offset, length }.into()
    }

    fn underline_entity(offset: i32, length: i32) -> tl::enums::MessageEntity {
        tl::types::MessageEntityUnderline { offset, length }.into()
    }

    #[test]
    fn strikes_each_character_in_the_span() {
        assert_eq!(render("ab cd", Some(&[strike_entity(0, 2)])), "a\u{336}b\u{336} cd");
    }

    #[test]
    fn stacks_strike_and_underline_on_the_same_span() {
        assert_eq!(
            render("hi", Some(&[strike_entity(0, 2), underline_entity(0, 2)])),
            "h\u{336}\u{332}i\u{336}\u{332}"
        );
    }

    #[test]
    fn counts_offsets_in_utf16_across_a_surrogate_pair() {
        // "🙂" is two UTF-16 units, so the struck span starts at offset 2, not 1.
        assert_eq!(render("🙂ok", Some(&[strike_entity(2, 2)])), "🙂o\u{336}k\u{336}");
    }

    #[test]
    fn leaves_line_breaks_unstruck() {
        assert_eq!(render("a\nb", Some(&[strike_entity(0, 3)])), "a\u{336}\nb\u{336}");
    }

    #[test]
    fn keeps_markers_for_styles_without_a_combining_mark() {
        let spoiler: tl::enums::MessageEntity =
            tl::types::MessageEntitySpoiler { offset: 0, length: 2 }.into();
        assert_eq!(render("hi", Some(&[spoiler])), "||hi||");
    }
}
