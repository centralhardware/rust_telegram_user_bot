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
                pieces.append(f"<del>{escape_html(hunk['removed'])}</del>")
            if hunk["added_len"]:
                pieces.append(f"<ins>{escape_html(hunk['added'])}</ins>")
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
