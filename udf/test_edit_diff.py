#!/usr/bin/env python3
"""The renderer against what the bot would have stored.

`fixtures.json` is written by the Rust side: for each pair of texts, the patch
`word_patch` produces and the markup `html_diff` used to store for it. So the
test is not "does the script agree with itself" -- it is the script against the
diff the boards used to print, on every shape that has ever mattered: a
newline inside a changed word, a doubled space, a backslash, an empty original,
a message replaced entirely.

    python3 udf/test_edit_diff.py
"""

import json
import pathlib
import subprocess
import sys
import unittest
import xml.etree.ElementTree as ET

HERE = pathlib.Path(__file__).parent
sys.path.insert(0, str(HERE))

from edit_diff import render  # noqa: E402

FIXTURES = json.loads((HERE / "fixtures.json").read_text())


class RenderTest(unittest.TestCase):
    def test_html_matches_the_diff_the_bot_used_to_store(self):
        for case in FIXTURES:
            with self.subTest(case["original"]):
                self.assertEqual(
                    render(case["message"], case["patch"], "html"), case["html"]
                )

    def test_prev_walks_back_to_the_text_the_edit_replaced(self):
        for case in FIXTURES:
            with self.subTest(case["original"]):
                self.assertEqual(
                    render(case["message"], case["patch"], "prev"), case["original"]
                )

    def test_a_row_written_before_the_format_changed_is_left_alone(self):
        """Those hold a rendered diff, not a patch. It has no hunks in it, so
        the message comes back untouched rather than mangled."""
        stored = "<div>already rendered</div>"
        self.assertEqual(
            render("the message", stored, "html"),
            '<div style="white-space:pre-wrap">the message</div>',
        )
        self.assertEqual(render("the message", stored, "prev"), "the message")

    def test_the_process_answers_row_by_row(self):
        """What ClickHouse actually does: JSONEachRow in, JSONEachRow out, one
        answer per row and flushed as it goes, since the pool holds the process
        open between calls."""
        rows = [
            json.dumps({"message": c["message"], "patch": c["patch"]}) for c in FIXTURES
        ]
        out = subprocess.run(
            [sys.executable, str(HERE / "edit_diff.py"), "html"],
            input="\n".join(rows) + "\n",
            capture_output=True,
            text=True,
            check=True,
        ).stdout.splitlines()

        self.assertEqual(len(out), len(FIXTURES))
        for line, case in zip(out, FIXTURES):
            self.assertEqual(json.loads(line)["result"], case["html"])


class DeclarationTest(unittest.TestCase):
    """The XML the server reads, checked for the things that make it refuse the
    file or call the wrong thing.

    ClickHouse reports a malformed declaration as a SAXParseException with a
    line and a column and nothing else, five seconds apart, forever -- which
    reads like the functions are broken rather than the file. The first version
    of this XML had a double hyphen in a prose comment, which XML forbids, and
    it cost a deploy to find.
    """

    ROOT = ET.parse(HERE / "edit_diff_function.xml").getroot()

    def test_it_declares_the_two_functions_the_view_calls(self):
        names = [f.findtext("name") for f in self.ROOT]
        self.assertEqual(names, ["edit_diff_html", "edit_prev_text"])

    def test_every_command_runs_this_script_in_a_mode_it_has(self):
        for function in self.ROOT:
            script, mode = function.findtext("command").split()
            self.assertEqual(script, "edit_diff.py")
            self.assertIn(mode, ("html", "prev"))

    def test_the_arguments_are_named_as_the_script_reads_them(self):
        for function in self.ROOT:
            self.assertEqual(
                [a.findtext("name") for a in function.findall("argument")],
                ["message", "patch"],
            )
            self.assertEqual(function.findtext("format"), "JSONEachRow")
            self.assertEqual(function.findtext("return_name"), "result")


if __name__ == "__main__":
    unittest.main()
