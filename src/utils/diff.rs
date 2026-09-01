use similar::{ChangeTag, TextDiff};

/// How a changed run is marked up: what opens a removal, an insertion, and what
/// closes either, plus whether the text between needs escaping for the target.
pub struct Style {
    del: &'static str,
    ins: &'static str,
    off: &'static str,
    /// Wrapped around the whole diff -- nothing for a terminal, the block that
    /// keeps line breaks and spacing for HTML.
    open: &'static str,
    close: &'static str,
    escape: bool,
}

/// Red on a red tint, struck through, for what the edit removed; green on a
/// green tint for what replaced it.
pub const ANSI: Style = Style {
    del: "\x1b[9;38;5;203;48;5;52m",
    ins: "\x1b[38;5;114;48;5;22m",
    off: "\x1b[0m",
    open: "",
    close: "",
    escape: false,
};

/// The same two colours as the terminal, in the form the Grafana table cell
/// wants: inline styles, because the cell has no stylesheet of ours, and
/// `white-space: pre-wrap` so the message's own line breaks survive.
pub const HTML: Style = Style {
    del: "<del style=\"color:#f85149;background:rgba(248,81,73,0.20);text-decoration:line-through\">",
    ins: "<ins style=\"color:#3fb950;background:rgba(46,160,67,0.20);text-decoration:none\">",
    off: "",
    open: "<div style=\"white-space:pre-wrap;font-family:monospace,monospace\">",
    close: "</div>",
    escape: true,
};

/// An edit as an inline diff: the message printed **once**, with only the words
/// that changed marked -- removed struck through in red, their replacement in
/// green, everything untouched left plain.
///
/// This replaced a coloured unified diff, which repeated every changed line
/// twice and left the eye to find the one word that actually moved. The stored
/// copy is the same rendering in HTML, so the log, the dashboards and this
/// function can never drift apart -- the boards used to word-diff in SQL.
pub fn render_diff(original: &str, modified: &str, style: &Style) -> String {
    let diff = TextDiff::from_words(original, modified);
    let mut out = String::from(style.open);
    let (mut removed, mut added) = (String::new(), String::new());

    for change in diff.iter_all_changes() {
        match change.tag() {
            // A run of removals and the insertions that replace it are held
            // back until the text goes equal again, so the two come out as one
            // "-old +new" pair instead of alternating word by word.
            ChangeTag::Delete => removed.push_str(change.value()),
            ChangeTag::Insert => added.push_str(change.value()),
            ChangeTag::Equal => {
                flush(&mut out, &mut removed, &mut added, style);
                push_text(&mut out, change.value(), style);
            }
        }
    }
    flush(&mut out, &mut removed, &mut added, style);
    out.push_str(style.close);
    out
}

/// The terminal rendering, for the console log.
pub fn inline_diff(original: &str, modified: &str) -> String {
    render_diff(original, modified, &ANSI)
}

/// The stored rendering, for the `diff` column and the boards that print it.
pub fn html_diff(original: &str, modified: &str) -> String {
    render_diff(original, modified, &HTML)
}

fn flush(out: &mut String, removed: &mut String, added: &mut String, style: &Style) {
    let both = !removed.trim().is_empty() && !added.trim().is_empty();
    push_run(out, removed, style.del, style);
    // The words come without the space that used to sit between them, so the
    // two runs would touch: "wensdeyWensdey".
    if both && !out.ends_with(char::is_whitespace) {
        out.push(' ');
    }
    push_run(out, added, style.ins, style);
}

/// Append one changed run, leaving the whitespace around it unmarked: a
/// struck-through newline paints the rest of the terminal line.
fn push_run(out: &mut String, run: &mut String, open: &str, style: &Style) {
    if run.trim().is_empty() {
        push_text(out, run, style);
    } else {
        let head = run.len() - run.trim_start().len();
        let tail = run.trim_end().len();
        let (lead, body, trail) = (
            run[..head].to_string(),
            run[head..tail].to_string(),
            run[tail..].to_string(),
        );
        push_text(out, &lead, style);
        out.push_str(open);
        push_text(out, &body, style);
        out.push_str(if style.escape { close_of(open) } else { style.off });
        push_text(out, &trail, style);
    }
    run.clear();
}

/// The closing tag for a run: `<del …>` and `<ins …>` are the only two markers
/// that open one, so the tag name in the opener decides it.
fn close_of(open: &str) -> &'static str {
    if open.starts_with("<del") {
        "</del>"
    } else {
        "</ins>"
    }
}

/// Text of the message itself, escaped when the target is markup so a message
/// containing `<b>` prints as `<b>` instead of turning the cell bold.
fn push_text(out: &mut String, text: &str, style: &Style) {
    if !style.escape {
        out.push_str(text);
        return;
    }
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(s: &str) -> String {
        s.replace(ANSI.del, "[-").replace(ANSI.ins, "{+").replace(ANSI.off, "|")
    }

    /// The HTML with its inline styles stripped back to bare tags, so a test
    /// reads as the markup and not as the palette.
    fn bare(s: &str) -> String {
        s.replace(HTML.open, "")
            .replace(HTML.close, "")
            .replace(HTML.del, "<del>")
            .replace(HTML.ins, "<ins>")
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

    #[test]
    fn html_marks_the_same_words_the_terminal_does() {
        assert_eq!(
            bare(&html_diff("meet at wensdey", "meet at Wensdey")),
            "meet at <del>wensdey</del> <ins>Wensdey</ins>"
        );
    }

    #[test]
    fn html_wraps_the_whole_diff_once() {
        let out = html_diff("a c", "a b c");
        assert!(out.starts_with(HTML.open) && out.ends_with(HTML.close));
        assert_eq!(bare(&out), "a <ins>b</ins> c");
    }

    #[test]
    fn html_escapes_the_message_and_not_its_own_tags() {
        assert_eq!(
            bare(&html_diff("<b>a</b> & c", "<b>a</b> & d")),
            "&lt;b&gt;a&lt;/b&gt; &amp; <del>c</del> <ins>d</ins>"
        );
    }

    /// Two separate changes, which the word-diff in SQL used to smear into one
    /// run reaching from the first to the last.
    #[test]
    fn html_keeps_two_changes_apart() {
        assert_eq!(
            bare(&html_diff("one two three four", "1 two three 4")),
            "<del>one</del> <ins>1</ins> two three <del>four</del> <ins>4</ins>"
        );
    }
}
