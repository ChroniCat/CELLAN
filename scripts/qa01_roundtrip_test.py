#!/usr/bin/env python3
"""Unit checks for the QA-01 COM result comparison.

These do not call Office; they pin the type/error semantics that prevent the
runner from accepting True for 1, a number for text, or visible "#DIV/0!" text
for a real Excel error.
"""
from __future__ import annotations

import unittest

from qa01_roundtrip import compare


def cell(value, *, is_error=False, text=None, formula=None, error_code=None):
    result = {
        "value": value,
        "value_type": type(value).__name__,
        "is_error": is_error,
        "error_code": error_code,
        "text": text if text is not None else str(value),
    }
    if formula is not None:
        result["formula"] = formula
    return result


class CompareTests(unittest.TestCase):
    def test_boolean_is_not_accepted_as_number(self):
        actual = {"A1": cell(True)}
        expected = {"A1": {"type": "number", "value": 1}}
        self.assertTrue(compare(actual, expected))

    def test_number_is_not_accepted_as_text(self):
        actual = {"A1": cell(1)}
        expected = {"A1": {"type": "text", "value": "1"}}
        self.assertTrue(compare(actual, expected))

    def test_plain_error_looking_text_is_not_a_real_error(self):
        actual = {"A1": cell("#DIV/0!", is_error=False)}
        expected = {"A1": {"type": "error", "value": "#DIV/0!"}}
        self.assertTrue(compare(actual, expected))

    def test_real_error_passes_and_records_code(self):
        actual = {"A1": cell(-2146826281, is_error=True, text="#DIV/0!", error_code=-2146826281)}
        expected = {"A1": {"type": "error", "value": "#DIV/0!", "error_code": -2146826281}}
        self.assertEqual(compare(actual, expected), [])

    def test_wrong_formula_is_reported(self):
        actual = {"A1": cell(3, formula="=A2+2")}
        expected = {"A1": {"type": "number", "value": 3, "formula": "A1+2"}}
        self.assertTrue(compare(actual, expected))

    def test_boolean_type_is_checked(self):
        actual = {"A1": cell(1)}
        expected = {"A1": {"type": "boolean", "value": True}}
        self.assertTrue(compare(actual, expected))


if __name__ == "__main__":
    unittest.main()
