#!/usr/bin/env python3
"""Render a stored edit patch, for ClickHouse to call as a function.

An edit row keeps the message as it now stands and a unified diff, counted in
words, against the text it replaced -- see `word_patch` in
`src/utils/diff.rs`, which is what writes them:

    @@ -7 +7 @@
    -cou
    +cpu

Nothing that stayed the same is written down, so the marked-up text and the
text the edit replaced are both put back together on read, out of the words
the patch names and the ones it skips over in `message`.

Two modes, one per function:

    html  the message once, with only the changed words marked -- what went in
          <del>, what replaced it in <ins>. What the boards print.
    prev  the text the edit replaced, which is why it is never stored.

ClickHouse speaks to this over stdin/stdout in JSONEachRow: one object per
row in, one object per row out, flushed as it goes because the pool keeps the
process alive between calls and a buffered answer would never arrive.
"""

import json
import sys

WRAPPER = ('<div style="white-space:pre-wrap">', "</div>")

# Red on a red tint for what the edit removed, green on a green tint for what
# replaced it -- the same two colours the console line has always used, so the
# log and the board read alike.
#
# The colours are inline because a Grafana cell has no stylesheet of ours: a
# <style> block is stripped from a panel unless disable_sanitize_html is on for
# the whole instance, while an inline style survives, which is how the wrapper
# above keeps its line breaks. They used to be left off for cost -- the markup
# was stored, so every colour was paid for again on every changed word of every
# row -- and that reason is gone: the row stores the patch now and this markup
# is made on read.
#
# The tags stay <del> and <ins> rather than two spans: if a sanitiser ever
# strips the style, the browser still strikes one through and underlines the
# other, which is the distinction being drawn. Both defaults are turned off
# here, since the colour says it better.
MARK = {
    "del": (
        '<del style="color:#e03131;background:rgba(224,49,49,.18);'
        'text-decoration:none">',
        "</del>",
    ),
    "ins": (
        '<ins style="color:#2f9e44;background:rgba(47,158,68,.18);'
        'text-decoration:none">',
        "</ins>",
    ),
}


def mark(kind, text):
    open_tag, close_tag = MARK[kind]
    return f"{open_tag}{escape_html(text)}{close_tag}"


def unescape(payload):
    """A payload as it was written down.

    The backslash goes first and lands on a byte no message carries, so an
    escaped backslash cannot be misread as the start of an escaped newline.
    """
    return payload.replace("\\\\", "\x01").replace("\\n", "\n").replace("\x01", "\\")


def escape_html(text):
    """A message prints into a table cell, so it is the message that is
    escaped -- never the <del>/<ins> put around it."""
    return text.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")


def hunks(patch):
    """The patch read back: where each hunk sits in the new text, and the
    words it took out and put in.

    A payload never carries a raw newline -- `word_patch` escapes it -- so the
    patch is read a line at a time. Anything that is not a patch (a row written
    before the format changed holds a rendered diff) yields nothing, and the
    caller hands the message back untouched.
    """
    out = []
    for line in patch.split("\n"):
        if line.startswith("@@ "):
            old_side, new_side = line[3:].split(" @@")[0].split(" +")
            start, _, length = new_side.partition(",")
            length = int(length) if length else 1
            removed_len = old_side.partition(",")[2]
            out.append(
                {
                    # 0-based: how many words of the new text stand before it.
                    "at": int(start) if length == 0 else int(start) - 1,
                    "added_len": length,
                    "removed": "",
                    # A side that removes nothing carries no line at all.
                    "removes": removed_len != "0",
                    "added": "",
                }
            )
        elif line.startswith("-") and out:
            out[-1]["removed"] = unescape(line[1:])
        elif line.startswith("+") and out:
            out[-1]["added"] = unescape(line[1:])
    return out


def render(message, patch, mode):
    """Walk the hunks, taking the untouched words out of the message as they
    come. A word is whatever sits between two single spaces, so a newline is
    *inside* a token and splitting on the space and joining on it again gives
    the message back byte for byte."""
    words = message.split(" ")
    if mode == "html":
        words = [escape_html(w) for w in words]

    pieces, cursor = [], 0
    for hunk in hunks(patch):
        pieces.extend(words[cursor : hunk["at"]])
        if mode == "html":
            if hunk["removes"]:
                pieces.append(mark("del", hunk["removed"]))
            if hunk["added_len"]:
                pieces.append(mark("ins", hunk["added"]))
        elif hunk["removes"]:
            # The other direction: the removed words go back in, the added
            # ones are left out.
            pieces.append(hunk["removed"])
        cursor = hunk["at"] + hunk["added_len"]
    pieces.extend(words[cursor:])

    body = " ".join(pieces)
    return f"{WRAPPER[0]}{body}{WRAPPER[1]}" if mode == "html" else body


def main():
    mode = sys.argv[1] if len(sys.argv) > 1 else "html"
    for line in sys.stdin:
        if not line.strip():
            continue
        row = json.loads(line)
        answer = render(row["message"], row["patch"], mode)
        sys.stdout.write(json.dumps({"result": answer}) + "\n")
        sys.stdout.flush()


if __name__ == "__main__":
    main()
