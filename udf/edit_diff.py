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


class Line:
    """The diff as it is put back together. A piece is normally separated from
    the one before it by the single space that stood between two tokens -- but
    a changed run can be cut open *inside* a token, at a newline, and there the
    whitespace is already part of the piece and no space is added."""

    def __init__(self, open_):
        self.out = open_
        self.empty = True

    def push(self, sep, text):
        if sep and not self.empty:
            self.out += " "
        self.out += text
        self.empty = False

    def finish(self, close):
        return self.out + close


def is_cut(s, at):
    """Whether a run may be opened at this point of a side: at either end of
    it, or where a space or a newline stands."""
    return at == 0 or at == len(s) or s[at].isspace() or s[at - 1].isspace()


def shared_head(a, b):
    """How much of a head the two sides share, cut back to a whitespace
    boundary both of them have -- the end of a string counts as one."""
    head = 0
    for x, y in zip(a, b):
        if x != y:
            break
        head += 1
    while head > 0 and not (is_cut(a, head) and is_cut(b, head)):
        head -= 1
    return head


def shared_tail(a, b):
    """The same for the tail, counted from the end of both."""
    tail = 0
    for x, y in zip(reversed(a), reversed(b)):
        if x != y:
            break
        tail += 1
    while tail > 0 and not (is_cut(a, len(a) - tail) and is_cut(b, len(b) - tail)):
        tail -= 1
    return tail


def refine(removed, added):
    """A changed run pared back to what actually changed.

    A word is whatever sits between two spaces, so a newline lives *inside* a
    token: appending a line to `... their reduction` makes one new token
    `reduction\nPS:`, which no longer equals the old `reduction` and drags the
    untouched word into the marking. The two sides of a replacement are
    therefore compared once more, character by character, and the head and tail
    they share are handed back plain -- but only when the cut falls on a
    whitespace boundary in both, so `cou` -> `cpu` stays one changed word
    rather than a marked `p` between a plain `c` and `u`.

    `None` is a side the edit did not touch, or one the head and the tail
    turned out to account for entirely -- which is not the same as a side whose
    only word is empty.
    """
    plain = ("", True, removed, added, True, "")
    # Only a replacement has two sides to share anything between.
    if not removed or not added:
        return plain

    head = shared_head(removed, added)
    d, i = removed[head:], added[head:]
    sep_after_prefix = True
    if head > 0:
        if d.startswith(" ") and i.startswith(" "):
            d, i = d[1:], i[1:]
        elif d.startswith(" ") and not i:
            d = d[1:]
        elif i.startswith(" ") and not d:
            i = i[1:]
        else:
            sep_after_prefix = False

    tail = shared_tail(d, i)
    suffix = d[len(d) - tail :] if tail else ""
    d, i = d[: len(d) - tail], i[: len(i) - tail]
    sep_before_suffix = True
    if tail > 0:
        if d.endswith(" ") and i.endswith(" "):
            d, i = d[:-1], i[:-1]
        elif d.endswith(" ") and not i:
            d = d[:-1]
        elif i.endswith(" ") and not d:
            i = i[:-1]
        else:
            sep_before_suffix = False

    return (removed[:head], sep_after_prefix, d or None, i or None, sep_before_suffix, suffix)


def render(message, patch, mode):
    """Walk the hunks, taking the untouched words out of the message as they
    come. A word is whatever sits between two single spaces, so a newline is
    *inside* a token and splitting on the space and joining on it again gives
    the message back byte for byte."""
    words = message.split(" ")
    if mode != "html":
        pieces, cursor = [], 0
        for hunk in hunks(patch):
            pieces.extend(words[cursor : hunk["at"]])
            if hunk["removes"]:
                # The other direction: the removed words go back in, the added
                # ones are left out.
                pieces.append(hunk["removed"])
            cursor = hunk["at"] + hunk["added_len"]
        pieces.extend(words[cursor:])
        return " ".join(pieces)

    line, cursor = Line(WRAPPER[0]), 0
    for hunk in hunks(patch):
        for word in words[cursor : hunk["at"]]:
            line.push(True, escape_html(word))
        prefix, sep_after, removed, added, sep_before, suffix = refine(
            hunk["removed"] if hunk["removes"] else None,
            hunk["added"] if hunk["added_len"] else None,
        )
        sep = True
        if prefix:
            line.push(sep, escape_html(prefix))
            sep = sep_after
        if removed is not None:
            line.push(sep, f"<del>{escape_html(removed)}</del>")
            sep = True
        if added is not None:
            line.push(sep, f"<ins>{escape_html(added)}</ins>")
        if suffix:
            line.push(sep_before, escape_html(suffix))
        cursor = hunk["at"] + hunk["added_len"]
    for word in words[cursor:]:
        line.push(True, escape_html(word))
    return line.finish(WRAPPER[1])


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
