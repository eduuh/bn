#!/usr/bin/env python3
"""Regression tests for skill command extraction, using the current bn help tree."""
from pathlib import Path
import runpy
import unittest

validator = runpy.run_path(str(Path(__file__).with_name("check-skill-commands.py")))


class SkillCommandTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.root = validator["load"](
            str(Path(__file__).resolve().parents[1] / "target/debug/bn"), [], {}
        )

    def test_double_quoted_comment_is_not_a_command(self):
        snippet = (
            'gh pr close <n> --comment "Superseded by #<newer> (bn dep-prs)" '
            '--delete-branch'
        )
        self.assertEqual(validator["check"](self.root, snippet), [])

    def test_single_quoted_comment_is_not_a_command(self):
        snippet = "gh pr close <n> --comment '(bn dep-prs)' --delete-branch"
        self.assertEqual(validator["check"](self.root, snippet), [])

    def test_escaped_quotes_do_not_end_a_quoted_comment(self):
        snippet = r'echo "say \"hello\" (bn dep-prs)" --delete-branch'
        self.assertEqual(validator["check"](self.root, snippet), [])

    def test_quoted_command_substitution_is_still_checked(self):
        snippet = 'echo "$(bn task not-a-command)"'
        self.assertEqual(
            validator["check"](self.root, snippet),
            ["`bn task` has no subcommand 'not-a-command'"],
        )

    def test_single_quoted_substitution_is_literal(self):
        snippet = "echo '$(bn task not-a-command)'"
        self.assertEqual(validator["check"](self.root, snippet), [])

    def test_real_subshell_is_still_checked(self):
        self.assertEqual(
            validator["check"](self.root, "(bn task not-a-command)"),
            ["`bn task` has no subcommand 'not-a-command'"],
        )

    def test_real_flag_is_still_checked(self):
        self.assertEqual(
            validator["check"](self.root, "bn --not-a-flag"),
            ["`bn ` has no flag --not-a-flag"],
        )

    def test_valid_quoted_argument_is_accepted(self):
        self.assertEqual(
            validator["check"](self.root, 'bn task add "a task"'), []
        )

    def test_quoted_prose_does_not_hide_a_later_command(self):
        snippet = 'echo "(bn dep-prs)" && bn task not-a-command'
        self.assertEqual(
            validator["check"](self.root, snippet),
            ["`bn task` has no subcommand 'not-a-command'"],
        )

    def test_command_after_a_valid_command_is_checked(self):
        snippet = "bn task --help && bn task not-a-command"
        self.assertEqual(
            validator["check"](self.root, snippet),
            ["`bn task` has no subcommand 'not-a-command'"],
        )


if __name__ == "__main__":
    unittest.main()
