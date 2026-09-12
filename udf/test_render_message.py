#!/usr/bin/env python3
"""The message renderer, against the shapes Telegram actually sends.

    python3 udf/test_render_message.py
"""

import pathlib
import sys
import unittest
import xml.etree.ElementTree as ET

HERE = pathlib.Path(__file__).parent
sys.path.insert(0, str(HERE))

from render_message import render  # noqa: E402


def html(message, entities):
    return render({"message": message, "entities": entities}, "html")


def ent(kind, offset, length, payload=""):
    return {"type": kind, "offset": offset, "length": length, "payload": payload}


class RenderTest(unittest.TestCase):
    def test_no_entities_is_the_text_itself(self):
        self.assertEqual(render({"message": "hi there"}, "html"), "hi there")

    def test_text_is_escaped(self):
        self.assertEqual(html("a < b & c", []), "a &lt; b &amp; c")

    def test_a_span_is_wrapped(self):
        self.assertEqual(html("bold text", [ent("bold", 0, 4)]), "<b>bold</b> text")

    def test_offsets_are_counted_in_utf16(self):
        # The emoji is two code units, so the entity starts at 2, not at 1.
        self.assertEqual(html("🙂ok", [ent("bold", 2, 2)]), "🙂<b>ok</b>")

    def test_nested_entities_nest(self):
        self.assertEqual(
            html("bold italic", [ent("bold", 0, 11), ent("italic", 5, 6)]),
            "<b>bold <i>italic</i></b>",
        )

    def test_two_entities_over_the_same_span(self):
        self.assertEqual(
            html("hi", [ent("bold", 0, 2), ent("italic", 0, 2)]),
            "<b><i>hi</i></b>",
        )

    def test_a_link_keeps_its_target(self):
        self.assertEqual(
            html("see this", [ent("text_link", 4, 4, "https://e.com")]),
            'see <a href="https://e.com">this</a>',
        )

    def test_a_mention_points_at_the_account(self):
        self.assertEqual(
            html("Sam", [ent("text_mention", 0, 3, "42")]),
            '<a href="tg://user?id=42">Sam</a>',
        )

    def test_pre_carries_its_language(self):
        self.assertEqual(
            html("x = 1", [ent("pre", 0, 5, "python")]),
            '<pre><code class="language-python">x = 1</code></pre>',
        )

    def test_an_entity_the_text_already_spells_out_adds_nothing(self):
        self.assertEqual(html("@sam", [ent("mention", 0, 4)]), "@sam")

    def test_an_overlapping_entity_is_dropped_rather_than_crossing_tags(self):
        self.assertEqual(
            html("abcd", [ent("bold", 0, 3), ent("italic", 2, 2)]),
            "<b>abc</b>d",
        )

    def test_text_mode_spells_out_what_the_text_does_not_say(self):
        self.assertEqual(
            render(
                {"message": "see this", "entities": [ent("text_link", 4, 4, "https://e.com")]},
                "text",
            ),
            "see this (https://e.com)",
        )

    def test_keyboard_rows(self):
        keyboard = [
            {"row": 0, "text": "Open", "type": "url", "payload": "https://e.com"},
            {"row": 0, "text": "No", "type": "callback", "payload": ""},
            {"row": 1, "text": "Next", "type": "callback", "payload": ""},
        ]
        self.assertEqual(
            render({"keyboard": keyboard}, "keyboard"),
            '<div class="tg-keyboard"><a href="https://e.com">Open</a> '
            "<span>[No]</span><br><span>[Next]</span></div>",
        )

    def test_no_keyboard_is_nothing(self):
        self.assertEqual(render({"keyboard": []}, "keyboard"), "")


class TupleShapeTest(unittest.TestCase):
    """What ClickHouse actually sends: an unnamed tuple arrives as a JSON array."""

    def test_entities_as_arrays(self):
        self.assertEqual(
            render({"message": "bold text", "entities": [["bold", 0, 4, ""]]}, "html"),
            "<b>bold</b> text",
        )

    def test_keyboard_as_arrays(self):
        self.assertEqual(
            render({"keyboard": [[0, "Open", "url", "https://e.com"]]}, "keyboard"),
            '<div class="tg-keyboard"><a href="https://e.com">Open</a></div>',
        )

    def test_a_tuple_of_the_wrong_width_is_dropped(self):
        self.assertEqual(render({"message": "hi", "entities": [["bold", 0]]}, "html"), "hi")


class DeclarationTest(unittest.TestCase):
    """The XML the server reads, parsed the way the server parses it -- a double
    hyphen inside a comment is enough for it to refuse the whole file."""

    def test_the_xml_declares_the_functions_this_script_serves(self):
        root = ET.parse(HERE / "render_message_function.xml").getroot()
        names = {f.findtext("name") for f in root.findall("function")}
        self.assertEqual(
            names, {"render_message_html", "render_message_text", "render_keyboard_html"}
        )


if __name__ == "__main__":
    unittest.main()
