use console::Style as Ansi;
use similar::{ChangeTag, DiffOp, TextDiff};
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
    /// Only the tests build one: at runtime the row stores a patch and the
    /// markup is put together by ClickHouse.
    #[cfg_attr(not(test), allow(dead_code))]
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

/// What a board shows: red on a red tint for what the edit removed, green on a
/// green tint for what replaced it -- the same two colours the console line
/// above uses, so the log and the board read alike.
///
/// The colours are inline because a Grafana cell has no stylesheet of ours: a
/// `<style>` block is stripped from a panel unless `disable_sanitize_html` is
/// on for the whole instance, while an inline style survives, which is how the
/// wrapper below keeps its line breaks. They were dropped once for cost -- the
/// markup was stored, so a colour was paid for again on every changed word of
/// every row, ~85% of the markup in a row -- and that reason is gone: the row
/// stores the patch now and this markup is made on read.
///
/// The elements stay `<del>` and `<ins>` rather than two spans: should a
/// sanitiser ever strip the style, a browser still strikes one through and
/// underlines the other, which is the distinction being drawn. Both of those
/// defaults are turned off here, since the colour says it better.
///
/// This is only what the tests measure the renderer against --
/// `udf/edit_diff.py` is what actually prints it, and `udf/fixtures.json`
/// holds the two together.
#[cfg(test)]
pub static HTML: LazyLock<Style> = LazyLock::new(|| Style {
    del: Mark::Tag(
        "<del style=\"color:#e03131;background:rgba(224,49,49,.18);text-decoration:none\">",
        "</del>",
    ),
    ins: Mark::Tag(
        "<ins style=\"color:#2f9e44;background:rgba(47,158,68,.18);text-decoration:none\">",
        "</ins>",
    ),
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
    let (old, new) = (tokens(original), tokens(modified));
    let mut pieces: Vec<String> = Vec::new();
    let (mut removed, mut added): (Vec<&str>, Vec<&str>) = (Vec::new(), Vec::new());

    let diff = TextDiff::from_slices(&old, &new);
    for change in diff.iter_all_changes() {
        match change.tag() {
            // A run of removals and the insertions that replace it are held
            // back until the text goes equal again, so the two come out as one
            // "-old +new" pair instead of alternating word by word.
            ChangeTag::Delete => removed.push(change.value()),
            ChangeTag::Insert => added.push(change.value()),
            ChangeTag::Equal => {
                flush(&mut pieces, &mut removed, &mut added, style);
                pieces.push(escape(change.value(), style));
            }
        }
    }
    flush(&mut pieces, &mut removed, &mut added, style);

    let mut out = String::from(style.open);
    out.push_str(&pieces.join(" "));
    out.push_str(style.close);
    out
}

/// A message as the words between its single spaces. Anything else a word
/// carries -- a newline, a tab, the second space of a double space -- stays
/// inside the token, so `tokens(s).join(" ")` is `s` again, byte for byte.
/// That is what lets the renderer rebuild a message out of `message` and a
/// patch that names nothing but the words that changed.
fn tokens(s: &str) -> Vec<&str> {
    s.split(' ').collect()
}

/// The terminal rendering, for the console log.
pub fn inline_diff(original: &str, modified: &str) -> String {
    render_diff(original, modified, &ANSI)
}

/// The rendering the boards print. Not called at runtime -- the row stores a
/// patch now and ClickHouse renders it (`edit_diff_html`, migration 025) -- but
/// it is the definition of what that function has to produce, and the tests
/// hold the two to each other.
#[cfg(test)]
pub fn html_diff(original: &str, modified: &str) -> String {
    render_diff(original, modified, &HTML)
}

fn flush(pieces: &mut Vec<String>, removed: &mut Vec<&str>, added: &mut Vec<&str>, style: &Style) {
    push_run(pieces, removed, &style.del, style);
    push_run(pieces, added, &style.ins, style);
}

/// Append one changed run as a single marked piece -- one `<del>` for
/// everything the edit removed at that point, one `<ins>` for what replaced it,
/// rather than a pair per word.
fn push_run(pieces: &mut Vec<String>, run: &mut Vec<&str>, mark: &Mark, style: &Style) {
    if run.is_empty() {
        return;
    }
    pieces.push(mark_text(&run.join(" "), mark, style));
    run.clear();
}

/// The marking itself. The terminal one is applied per line: a strike-through
/// that spans a newline paints the rest of the terminal line with it.
fn mark_text(text: &str, mark: &Mark, style: &Style) -> String {
    match mark {
        Mark::Ansi(ansi) => text
            .split('\n')
            .map(|line| ansi.apply_to(line).to_string())
            .collect::<Vec<_>>()
            .join("\n"),
        Mark::Tag(open, close) => format!("{open}{}{close}", escape(text, style)),
    }
}

/// Text of the message itself, escaped when the target is markup so a message
/// containing `<b>` prints as `<b>` instead of turning the cell bold.
fn escape(text: &str, style: &Style) -> String {
    if !style.escape {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// The stored form.
// ---------------------------------------------------------------------------

/// An edit as a unified diff whose unit is a word rather than a line.
///
/// A message is usually edited to fix one word of it, and a line-based diff of
/// a one-line message is that whole line twice. Measured over the 470k edits in
/// `edited_log`: the texts themselves are 112 MiB, a rendered inline diff 177,
/// a line-based unified diff 141 -- and this 31, because nothing that did not
/// change is written down at all.
///
/// The header is `diff -u`'s, counted in words: `@@ -old,len +new,len @@`, with
/// `,len` left off when it is 1 and the side left off when it is empty. Then
/// the removed words, then the added ones, space-joined on one line each.
///
/// Payloads are escaped, because a word can carry a newline (`end.\nNext` is
/// one token) and a raw one would end the line early and orphan every hunk
/// after it: a backslash becomes `\\`, a newline becomes `\n`.
pub fn word_patch(original: &str, modified: &str) -> String {
    let (old, new) = (tokens(original), tokens(modified));
    let mut out = String::new();

    for op in TextDiff::from_slices(&old, &new).ops() {
        let (o, o_len, n, n_len) = match *op {
            DiffOp::Equal { .. } => continue,
            DiffOp::Delete {
                old_index,
                old_len,
                new_index,
            } => (old_index, old_len, new_index, 0),
            DiffOp::Insert {
                old_index,
                new_index,
                new_len,
            } => (old_index, 0, new_index, new_len),
            DiffOp::Replace {
                old_index,
                old_len,
                new_index,
                new_len,
            } => (old_index, old_len, new_index, new_len),
        };
        out.push_str(&format!(
            "@@ -{} +{} @@\n",
            range(o, o_len),
            range(n, n_len)
        ));
        if o_len > 0 {
            out.push_str(&format!(
                "-{}\n",
                escape_payload(&old[o..o + o_len].join(" "))
            ));
        }
        if n_len > 0 {
            out.push_str(&format!(
                "+{}\n",
                escape_payload(&new[n..n + n_len].join(" "))
            ));
        }
    }
    out
}

/// One side of a header. A side that removes or adds nothing points at the word
/// it sits behind, which is what `diff -u` does with an empty side.
fn range(index: usize, len: usize) -> String {
    let start = if len == 0 { index } else { index + 1 };
    if len == 1 {
        start.to_string()
    } else {
        format!("{start},{len}")
    }
}

fn escape_payload(text: &str) -> String {
    text.replace('\\', "\\\\").replace('\n', "\\n")
}

#[cfg(test)]
fn unescape_payload(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

/// The patch read back: one entry per hunk, as the renderer needs it.
///
/// Reading is ClickHouse's job -- `edit_diff_html` in migration 025 is what the
/// boards call, and nothing in the bot reads a patch back. This exists so the
/// tests can: a patch that cannot be walked back to the text it was taken
/// against is not a patch, and that has to fail here rather than on a board.
#[cfg(test)]
pub struct Hunk {
    /// Where the hunk starts in the *new* text, 0-based, i.e. how many words of
    /// it stand before this hunk.
    pub new_index: usize,
    pub removed: String,
    pub removed_len: usize,
    pub added: String,
    pub added_len: usize,
}

/// Parse a patch back into its hunks. `None` for anything that is not one --
/// an old row still holding a rendered diff, for instance.
#[cfg(test)]
pub fn parse_patch(patch: &str) -> Option<Vec<Hunk>> {
    let mut hunks: Vec<Hunk> = Vec::new();
    for line in patch.lines() {
        if let Some(rest) = line.strip_prefix("@@ ") {
            let (old_side, rest) = rest.split_once(' ')?;
            let (new_side, _) = rest.split_once(' ')?;
            let (start, len) = side(new_side.strip_prefix('+')?)?;
            let (_, removed_len) = side(old_side.strip_prefix('-')?)?;
            hunks.push(Hunk {
                new_index: if len == 0 { start } else { start - 1 },
                removed: String::new(),
                removed_len,
                added: String::new(),
                added_len: len,
            });
        } else if let Some(rest) = line.strip_prefix('-') {
            hunks.last_mut()?.removed = unescape_payload(rest);
        } else if let Some(rest) = line.strip_prefix('+') {
            hunks.last_mut()?.added = unescape_payload(rest);
        } else {
            return None;
        }
    }
    Some(hunks)
}

#[cfg(test)]
fn side(text: &str) -> Option<(usize, usize)> {
    match text.split_once(',') {
        Some((start, len)) => Some((start.parse().ok()?, len.parse().ok()?)),
        None => Some((text.parse().ok()?, 1)),
    }
}

/// The message a patch was taken against, rebuilt from the message it produced.
/// The edit row keeps the text as it now stands, so the text it replaced never
/// has to be stored: it is this.
#[cfg(test)]
pub fn previous_text(message: &str, patch: &str) -> Option<String> {
    let words = tokens(message);
    let hunks = parse_patch(patch)?;
    let mut out: Vec<&str> = Vec::new();
    let mut cursor = 0;

    for hunk in &hunks {
        out.extend(words.get(cursor..hunk.new_index)?);
        if hunk.removed_len > 0 {
            out.push(&hunk.removed);
        }
        cursor = hunk.new_index + hunk.added_len;
    }
    out.extend(words.get(cursor..)?);
    Some(out.join(" "))
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
            let Mark::Tag(open, close) = mark else {
                continue;
            };
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
        for code in [
            "\x1b[38;5;203m",
            "\x1b[48;5;52m",
            "\x1b[9m",
            "\x1b[38;5;114m",
            "\x1b[48;5;22m",
        ] {
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

    /// The marks carry their colour inline, since a Grafana cell has no
    /// stylesheet of ours, and turn off the strike-through and the underline
    /// the two elements come with -- the colour is the distinction now.
    #[test]
    fn html_marks_are_coloured_and_not_struck_through() {
        for (mark, colour) in [(&HTML.del, "#e03131"), (&HTML.ins, "#2f9e44")] {
            let Mark::Tag(open, _) = mark else { continue };
            assert!(open.contains(&format!("color:{colour}")), "{open}");
            assert!(open.contains("text-decoration:none"), "{open}");
        }
    }

    /// The cases that have to survive a round trip through the stored form:
    /// the ordinary ones, and every way whitespace can hide inside a token.
    const ROUND_TRIP: &[(&str, &str)] = &[
        ("meet at wensdey", "meet at Wensdey"),
        ("one two three four", "1 two three 4"),
        ("a c", "a b c"),
        ("a b c", "a c"),
        ("<b>a</b> & c", "<b>a</b> & d"),
        (
            "hello world\nsecond line here",
            "hello there\nsecond line here",
        ),
        (
            "first line\nsecond line",
            "first line\nsecond line\nthird line",
        ),
        ("double  spaced text", "double  spaced words"),
        ("trailing space ", "trailing space  "),
        ("a\\b backslash", "a\\b backslashes"),
        ("", "now it has text"),
        ("all of it goes", "replaced entirely"),
    ];

    /// The header is `diff -u`'s, counted in words.
    #[test]
    fn a_patch_names_only_the_words_that_changed() {
        assert_eq!(
            word_patch(
                "we did see a graph of cou being pinned",
                "we did see a graph of cpu being pinned"
            ),
            "@@ -7 +7 @@\n-cou\n+cpu\n"
        );
    }

    /// An empty side points at the word it sits behind and carries no line, so
    /// an insertion costs its own words and nothing else.
    #[test]
    fn an_insertion_has_no_removed_side() {
        assert_eq!(
            word_patch(
                "proper per-chat queues but",
                "proper per-chat queues (like teloxide) but"
            ),
            "@@ -3,0 +4,2 @@\n+(like teloxide)\n"
        );
        assert_eq!(word_patch("a b c", "a c"), "@@ -2 +1,0 @@\n-b\n");
    }

    /// Two changes stay two hunks, so nothing between them is written down.
    #[test]
    fn each_change_is_its_own_hunk() {
        assert_eq!(
            word_patch("one two three four", "1 two three 4"),
            "@@ -1 +1 @@\n-one\n+1\n@@ -4 +4 @@\n-four\n+4\n"
        );
    }

    /// A word can carry a newline -- `end.\nNext` is one token -- and a raw one
    /// in a payload would end the line early and orphan every hunk after it.
    #[test]
    fn a_newline_inside_a_changed_word_is_escaped() {
        let patch = word_patch("hello world\nsecond line", "hello there\nsecond line");
        assert_eq!(patch, "@@ -2 +2 @@\n-world\\nsecond\n+there\\nsecond\n");
        assert_eq!(patch.lines().count(), 3);
    }

    /// The text the edit replaced is never stored: it comes back out of the
    /// message the edit produced and the patch against it.
    #[test]
    fn the_patch_walks_back_to_the_text_it_replaced() {
        for (original, modified) in ROUND_TRIP {
            assert_eq!(
                previous_text(modified, &word_patch(original, modified)).as_deref(),
                Some(*original),
                "{original:?} -> {modified:?}"
            );
        }
    }

    /// `udf/fixtures.json` is what the Python renderer is tested against: for
    /// each pair of texts, the patch this module produces and the markup it
    /// used to store. Which is only worth anything while it says what this
    /// module actually does -- change `word_patch` and this fails until the
    /// file is written again:
    ///
    ///     cargo test write_the_fixtures -- --ignored
    #[test]
    fn the_fixtures_the_renderer_is_tested_against_are_current() {
        assert_eq!(
            std::fs::read_to_string(FIXTURES).expect("udf/fixtures.json"),
            fixtures(),
            "udf/fixtures.json is stale, see the note above"
        );
    }

    #[test]
    #[ignore]
    fn write_the_fixtures() {
        std::fs::write(FIXTURES, fixtures()).unwrap();
    }

    const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/udf/fixtures.json");

    fn fixtures() -> String {
        let cases: Vec<serde_json::Value> = ROUND_TRIP
            .iter()
            .map(|(original, modified)| {
                serde_json::json!({
                    "original": original,
                    "message": modified,
                    "patch": word_patch(original, modified),
                    "html": html_diff(original, modified),
                })
            })
            .collect();
        serde_json::to_string_pretty(&cases).unwrap() + "\n"
    }
}
