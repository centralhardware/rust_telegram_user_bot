use similar::{ChangeTag, TextDiff};

/// Red on a red tint, struck through, for what the edit removed.
const DEL: &str = "\x1b[9;38;5;203;48;5;52m";
/// Green on a green tint for what replaced it.
const INS: &str = "\x1b[38;5;114;48;5;22m";
const OFF: &str = "\x1b[0m";

/// An edit as an inline diff: the message printed **once**, with only the words
/// that changed marked -- removed struck through in red, their replacement in
/// green, everything untouched left plain.
///
/// This replaced a coloured unified diff, which repeated every changed line
/// twice and left the eye to find the one word that actually moved. It is the
/// same form the "Recent edits" panel of the Telegram messages dashboard uses,
/// so the log and the board read alike.
pub fn inline_diff(original: &str, modified: &str) -> String {
    let diff = TextDiff::from_words(original, modified);
    let mut out = String::new();
    let (mut removed, mut added) = (String::new(), String::new());

    for change in diff.iter_all_changes() {
        match change.tag() {
            // A run of removals and the insertions that replace it are held
            // back until the text goes equal again, so the two come out as one
            // "-old +new" pair instead of alternating word by word.
            ChangeTag::Delete => removed.push_str(change.value()),
            ChangeTag::Insert => added.push_str(change.value()),
            ChangeTag::Equal => {
                flush(&mut out, &mut removed, &mut added);
                out.push_str(change.value());
            }
        }
    }
    flush(&mut out, &mut removed, &mut added);
    out
}

fn flush(out: &mut String, removed: &mut String, added: &mut String) {
    let both = !removed.trim().is_empty() && !added.trim().is_empty();
    push_run(out, removed, DEL);
    // The words come without the space that used to sit between them, so the
    // two runs would touch: "wensdeyWensdey".
    if both && !out.ends_with(char::is_whitespace) {
        out.push(' ');
    }
    push_run(out, added, INS);
}

/// Append one changed run, leaving the whitespace around it uncoloured: a
/// struck-through newline paints the rest of the terminal line.
fn push_run(out: &mut String, run: &mut String, style: &str) {
    let body = run.trim();
    if body.is_empty() {
        out.push_str(run);
    } else {
        let head = run.len() - run.trim_start().len();
        let tail = run.trim_end().len();
        out.push_str(&run[..head]);
        out.push_str(style);
        out.push_str(&run[head..tail]);
        out.push_str(OFF);
        out.push_str(&run[tail..]);
    }
    run.clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(s: &str) -> String {
        s.replace(DEL, "[-").replace(INS, "{+").replace(OFF, "|")
    }

    #[test]
    fn marks_only_the_changed_word() {
        assert_eq!(
            plain(&inline_diff("meet at wensdey", "meet at Wensdey")),
            "meet at [-wensdey| {+Wensdey|"
        );
    }

    #[test]
    fn keeps_line_breaks_and_unchanged_text() {
        assert_eq!(
            plain(&inline_diff("one\ntwo three", "one\ntwo four")),
            "one\ntwo [-three| {+four|"
        );
    }

    #[test]
    fn a_pure_insertion_has_no_removed_run() {
        assert_eq!(plain(&inline_diff("a c", "a b c")), "a {+b| c");
    }
}
