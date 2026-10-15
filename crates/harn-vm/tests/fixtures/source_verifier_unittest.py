from __future__ import annotations

from dataclasses import dataclass
import runpy
import sys
import unittest


class Calculation(unittest.TestCase):
    def test_calculation(self):
        self.assertIs(sys.modules["__main__"].__dict__, globals())
        self.assertNotIn("witness_fd", globals())

        @dataclass
        class Result:
            value: int

        self.assertEqual(Result(4).value, 4)
        runpy.run_path(sys.argv[1])


result = unittest.main(argv=[sys.argv[0]], exit=False).result
raise SystemExit(0 if result.wasSuccessful() else 1)
