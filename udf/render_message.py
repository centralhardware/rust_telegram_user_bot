#!/usr/bin/env python3
"""Render a message out of its text, its entities and its keyboard, for
ClickHouse to call as a function.

`message` holds what the sender typed and nothing else; the formatting Telegram
draws over it is the `entities` array beside it, and the buttons under it the
`keyboard` array. Putting the three back together is a read-time job, and this
is it.

Both arrive as arrays of tuples, which JSONEachRow hands over as arrays:

    entities  ["bold", 0, 4, ""]
    keyboard  [0, "Open", "url", "https://..."]

A tuple whose elements are named arrives as an object instead, so both shapes
are read -- the columns are unnamed today only because the Rust client cannot
parse a named tuple out of the insert header.

`offset` and `length` are UTF-16 code units, as Telegram counts them, and
`payload` is the one thing an entity carries besides its span -- a link's
target, a mention's account, a code block's language -- empty for the many that
carry nothing.

Three modes, one per function:

    html      the text with its entities applied, as HTML
    keyboard  the buttons under it, as HTML
    text      the text with the entities that carry a payload spelled out --
              a link's target, a mention's id -- and the rest dropped, for a
              panel that prints no markup

ClickHouse speaks to this over stdin/stdout in JSONEachRow: one object per row
in, one object per row out, flushed as it goes because the pool keeps the
process alive between calls and a buffered answer would never arrive.
"""

import json
import sys

# Entities that wrap their span in a tag. Everything not named here -- a
# mention, a hashtag, a bare url, a phone number -- is already spelled out in
# the text itself, and Telegram only marks it so a client can make it tappable.
TAGS = {
    "bold": ("<b>", "</b>"),
    "italic": ("<i>", "</i>"),
    "underline": ("<u>", "</u>"),
    "strikethrough": ("<s>", "</s>"),
    "spoiler": ('<span class="tg-spoiler">', "</span>"),
    "code": ("<code>", "</code>"),
}


def escape(text):
    return text.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")


def units(text):
    """The text as a list of UTF-16 code units, which is what an offset counts.

    A character outside the BMP is two of them, so slicing the string itself
    would put a tag in the wrong place the moment a message carries an emoji.
    """
    raw = text.encode("utf-16-le", "surrogatepass")
    return [raw[i : i + 2] for i in range(0, len(raw), 2)]


def join(chunk):
    return b"".join(chunk).decode("utf-16-le", "surrogatepass")


def tree(entities, start, end):
    """The entities covering [start, end) as a forest of the outermost ones.

    Telegram nests entities rather than overlapping them, so an entity is either
    inside the one before it or beside it. One that does overlap -- which only a
    hand-written row can be -- is dropped rather than left to close a tag it
    never opened.
    """
    nodes = []
    cursor = start
    for index, entity in enumerate(entities):
        at = entity["offset"]
        to = at + entity["length"]
        if at < cursor or to > end:
            continue
        children = []
        for inner in entities[index + 1 :]:
            if inner["offset"] >= to:
                break
            children.append(inner)
        nodes.append((entity, children))
        cursor = to
    return nodes


def render_html(text, entities):
    body = units(text)
    return "".join(render_span(body, tree(entities, 0, len(body)), 0, len(body)))


def render_span(body, nodes, start, end):
    out = []
    cursor = start
    for entity, children in nodes:
        at = entity["offset"]
        to = at + entity["length"]
        out.append(escape(join(body[cursor:at])))
        inner = render_span(body, tree(children, at, to), at, to)
        out.append(wrap(entity, "".join(inner)))
        cursor = to
    out.append(escape(join(body[cursor:end])))
    return out


def wrap(entity, inner):
    kind = entity.get("type", "")
    if kind in TAGS:
        open_tag, close_tag = TAGS[kind]
        return open_tag + inner + close_tag
    payload = entity.get("payload", "")
    if kind == "pre":
        opened = f'<pre><code class="language-{escape(payload)}">' if payload else "<pre><code>"
        return opened + inner + "</code></pre>"
    if kind == "text_link":
        return f'<a href="{escape(payload)}">{inner}</a>'
    if kind == "text_mention":
        return f'<a href="tg://user?id={escape(payload)}">{inner}</a>'
    if kind == "blockquote":
        return "<blockquote>" + inner + "</blockquote>"
    return inner


def render_text(text, entities):
    """The text with only what the entities add that the text does not already
    say: where a link points, and who a mention names."""
    body = units(text)
    out = []
    cursor = 0
    for entity in sorted(entities, key=lambda e: (e["offset"], -e["length"])):
        at = entity["offset"]
        to = at + entity["length"]
        if at < cursor:
            continue
        out.append(join(body[cursor:at]))
        span = join(body[at:to])
        kind = entity.get("type", "")
        payload = entity.get("payload", "")
        if kind == "text_link":
            out.append(f"{span} ({payload})")
        elif kind == "text_mention":
            out.append(f"{span} (tg://user?id={payload})")
        else:
            out.append(span)
        cursor = to
    out.append(join(body[cursor:]))
    return "".join(out)


# Buttons whose payload is somewhere to go rather than something to send back.
LINKS = {"url", "login_url", "web_app"}


def render_keyboard(buttons):
    """The inline keyboard as HTML: one line per row -- the buttons carry the row
    they sit in -- a button that leads somewhere as a link, one that does not as a
    label."""
    lines = []
    for button in buttons:
        row = button.get("row", 0)
        while len(lines) <= row:
            lines.append([])
        text = escape(button.get("text", ""))
        payload = button.get("payload", "")
        if button.get("type", "") in LINKS and payload:
            lines[row].append(f'<a href="{escape(payload)}">{text}</a>')
        else:
            lines[row].append(f"<span>[{text}]</span>")
    drawn = [" ".join(line) for line in lines if line]
    if not drawn:
        return ""
    return '<div class="tg-keyboard">' + "<br>".join(drawn) + "</div>"


ENTITY_FIELDS = ("type", "offset", "length", "payload")
BUTTON_FIELDS = ("row", "text", "type", "payload")


def usable(value, fields):
    """The array as a list of dicts, whichever shape it arrived in: a tuple
    ClickHouse filled is a JSON array, one whose elements are named is a JSON
    object. Anything else in it -- which only a hand-written call can put
    there -- is dropped rather than raised over."""
    if not isinstance(value, list):
        return []
    rows = []
    for item in value:
        if isinstance(item, dict):
            rows.append(item)
        elif isinstance(item, list) and len(item) == len(fields):
            rows.append(dict(zip(fields, item)))
    return rows


def render(row, mode):
    if mode == "keyboard":
        return render_keyboard(usable(row.get("keyboard"), BUTTON_FIELDS))
    text = row.get("message", "")
    entities = [
        e
        for e in usable(row.get("entities"), ENTITY_FIELDS)
        if isinstance(e.get("offset"), int) and isinstance(e.get("length"), int)
    ]
    entities.sort(key=lambda e: (e["offset"], -e["length"]))
    if mode == "text":
        return render_text(text, entities)
    return render_html(text, entities)


def main():
    mode = sys.argv[1] if len(sys.argv) > 1 else "html"
    for line in sys.stdin:
        if not line.strip():
            continue
        answer = render(json.loads(line), mode)
        sys.stdout.write(json.dumps({"result": answer}) + "\n")
        sys.stdout.flush()


if __name__ == "__main__":
    main()
