use console::Style as Ansi;
use similar::{ChangeTag, TextDiff};
use std::sync::LazyLock;

/// How a changed run is marked up: what wraps a removal, what wraps an
/// insertion, what wraps the diff as a whole, and whether the text between
/// needs escaping for the target.
pub struct Style {
    del: Mark,
    ins: Mark,
    /// Wrapped around the whole diff -- nothing for a terminal, the block that
    /// keeps line breaks and spacing for HTML.
    open: &'static str,
    close: &'static str,
    escape: bool,
}

/// The marking itself. Neither form is written by hand: the terminal one is a
/// `console::Style`, which knows the escape codes and where the reset goes, and
/// the HTML one is an element opened and closed around the run.
enum Mark {
    Ansi(Ansi),
    Tag(&'static str, &'static str),
}

/// Red on a red tint, struck through, for what the edit removed; green on a
/// green tint for what replaced it.
///
/// Styling is forced on: the log goes through `log` to stderr and is read back
/// with `docker logs`, so waiting for a terminal to be attached would leave it
/// colourless where it is actually looked at.
pub static ANSI: LazyLock<Style> = LazyLock::new(|| Style {
    del: Mark::Ansi(
        Ansi::new()
            .color256(203)
            .on_color256(52)
            .strikethrough()
            .force_styling(true),
    ),
    ins: Mark::Ansi(Ansi::new().color256(114).on_color256(22).force_styling(true)),
    open: "",
    close: "",
    escape: false,
});

/// The stored rendering: the two elements HTML already has for this, and
/// nothing else.
///
/// A mark is repeated on every changed word, so anything declared on it is paid
/// for again on every word of every stored row. There is nothing worth
/// declaring: a browser strikes `<del>` through and underlines `<ins>` on its
/// own, which is the distinction the diff is making. The colours the console
/// line uses cannot come back from Grafana's side either -- a markdown cell has
/// no stylesheet of ours, and the `<style>` block a text panel could hold is
/// stripped unless `disable_sanitize_html` is turned on for the whole instance.
///
/// The wrapper stays: `white-space: pre-wrap` is what keeps the message's own
/// line breaks in a table cell, the table has no option for it, and it is one
/// per row rather than one per word.
pub static HTML: LazyLock<Style> = LazyLock::new(|| Style {
    del: Mark::Tag("<del>", "</del>"),
    ins: Mark::Tag("<ins>", "</ins>"),
    open: "<div style=\"white-space:pre-wrap\">",
    close: "</div>",
    escape: true,
});

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
    push_run(out, removed, &style.del, style);
    // The words come without the space that used to sit between them, so the
    // two runs would touch: "wensdeyWensdey".
    if both && !out.ends_with(char::is_whitespace) {
        out.push(' ');
    }
    push_run(out, added, &style.ins, style);
}

/// Append one changed run, leaving the whitespace around it unmarked: a
/// struck-through newline paints the rest of the terminal line.
fn push_run(out: &mut String, run: &mut String, mark: &Mark, style: &Style) {
    if run.trim().is_empty() {
        push_text(out, run, style);
    } else {
        let head = run.len() - run.trim_start().len();
        let tail = run.trim_end().len();
        let (lead, body, trail) = (&run[..head], &run[head..tail], &run[tail..]);
        let mut marked = String::new();
        match mark {
            Mark::Ansi(ansi) => marked.push_str(&ansi.apply_to(body).to_string()),
            Mark::Tag(open, close) => {
                marked.push_str(open);
                push_text(&mut marked, body, style);
                marked.push_str(close);
            }
        }
        push_text(out, lead, style);
        out.push_str(&marked);
        push_text(out, trail, style);
    }
    run.clear();
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

    /// The terminal rendering with the escape codes spelled out, so a test
    /// reads as the marking and not as the palette. `console` decides what the
    /// codes are, so they are asked for rather than written down: styling an
    /// empty marker hands back the prefix and the reset around it.
    fn plain(s: &str) -> String {
        let mut out = s.to_string();
        for (mark, name) in [(&ANSI.del, "[-"), (&ANSI.ins, "{+")] {
            let Mark::Ansi(ansi) = mark else { continue };
            let sample = ansi.apply_to("\0").to_string();
            let (prefix, reset) = sample.split_once('\0').unwrap();
            out = out.replace(prefix, name).replace(reset, "|");
        }
        out
    }

    /// The HTML with its inline styles stripped back to bare tags, so a test
    /// reads as the markup and not as the palette.
    fn bare(s: &str) -> String {
        let mut out = s.replace(HTML.open, "").replace(HTML.close, "");
        for (mark, name) in [(&HTML.del, "del"), (&HTML.ins, "ins")] {
            let Mark::Tag(open, close) = mark else { continue };
            out = out
                .replace(open, &format!("<{name}>"))
                .replace(close, &format!("</{name}>"));
        }
        out
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

    /// The colours the console line has always had, and the styling forced on
    /// so they survive a log with no terminal attached. `console` is free to
    /// order the codes as it likes, hence the three separate checks.
    #[test]
    fn the_terminal_run_keeps_its_colours() {
        let out = inline_diff("a c", "a d");
        for code in ["\x1b[38;5;203m", "\x1b[48;5;52m", "\x1b[9m", "\x1b[38;5;114m", "\x1b[48;5;22m"] {
            assert!(out.contains(code), "{code:?} missing from {out:?}");
        }
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

    /// A mark is paid for again on every changed word of every stored row, so
    /// it stays the bare element. The colour and the tint it used to carry said
    /// what the strike-through and the underline already say, and cost ~85% of
    /// the markup in a row -- 825 bytes down to 319 on a real three-run edit.
    #[test]
    fn html_marks_are_bare_elements() {
        for (mark, name) in [(&HTML.del, "del"), (&HTML.ins, "ins")] {
            let Mark::Tag(open, close) = mark else {
                continue;
            };
            assert_eq!(
                (*open, *close),
                (&*format!("<{name}>"), &*format!("</{name}>"))
            );
        }
    }
}
