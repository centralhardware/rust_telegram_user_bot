//! What an edit changed in a message's formatting, for the console line.
//!
//! An edit that leaves the text alone but adds a link or makes a word bold has
//! an empty text diff; this names the entities that came and went, each with
//! the text it covers and, for a link, where it leads.

use crate::telegram::entities::Entity;

/// `+text_link "KL bird park" → https://…`, one per entity added or removed,
/// joined by `, `. Empty when the two lists hold the same entities.
pub fn describe(text: &str, before: &[Entity], after: &[Entity]) -> String {
    let removed = before
        .iter()
        .filter(|e| !after.contains(e))
        .map(|e| ('-', e));
    let added = after
        .iter()
        .filter(|e| !before.contains(e))
        .map(|e| ('+', e));
    removed
        .chain(added)
        .map(|(sign, entity)| one(sign, text, entity))
        .collect::<Vec<_>>()
        .join(", ")
}

fn one(sign: char, text: &str, (kind, offset, length, payload): &Entity) -> String {
    let covered = utf16_slice(text, *offset, *length);
    let mut out = format!("{sign}{kind} \"{covered}\"");
    if kind == "text_link" && !payload.is_empty() {
        out.push_str(&format!(" → {payload}"));
    }
    out
}

/// The span of `text` an entity covers; offsets are UTF-16 code units.
fn utf16_slice(text: &str, offset: u32, length: u32) -> String {
    let units: Vec<u16> = text.encode_utf16().collect();
    let start = (offset as usize).min(units.len());
    let end = (start + length as usize).min(units.len());
    String::from_utf16_lossy(&units[start..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(kind: &str, offset: u32, length: u32, payload: &str) -> Entity {
        (kind.into(), offset, length, payload.into())
    }

    #[test]
    fn added_link_names_text_and_target() {
        let text = "KL bird park\n\n#Malaysia";
        let tag = e("hashtag", 14, 9, "");
        let link = e("text_link", 0, 12, "https://example.com/park");
        assert_eq!(
            describe(text, &[tag.clone()], &[link, tag]),
            "+text_link \"KL bird park\" → https://example.com/park"
        );
    }

    #[test]
    fn removed_bold_and_utf16_offsets() {
        let text = "🇲🇾 hi there";
        assert_eq!(describe(text, &[e("bold", 5, 2, "")], &[]), "-bold \"hi\"");
    }

    #[test]
    fn same_entities_describe_nothing() {
        let x = [e("bold", 0, 1, "")];
        assert_eq!(describe("a", &x, &x), "");
    }
}
