#!/usr/bin/env python3
from __future__ import annotations

import importlib.util
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "check_generated_registry.py"
SPEC = importlib.util.spec_from_file_location("check_generated_registry", SCRIPT)
assert SPEC is not None
registry = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(registry)


class GeneratedRegistryTests(unittest.TestCase):
    def test_whole_target_rejects_prefixed_target(self) -> None:
        self.assertFalse(
            registry.whole_target_search(
                "check-provider-catalog",
                "make check-provider-catalog-drift\n",
            )
        )

    def test_whole_target_accepts_exact_target_at_end(self) -> None:
        self.assertTrue(registry.whole_target_search("check-x", "run check-x"))

    def test_makefile_targets_skip_recipe_lines(self) -> None:
        makefile = "zeta:\n\tx\nreal:\n\tnope: indented\n  alsonope: spaced\n"
        self.assertEqual(registry.makefile_targets(makefile), ["real", "zeta"])

    def test_make_all_recipe_stops_at_next_target(self) -> None:
        makefile = "all:\n\tone\n\n\ttwo\nnext:\n\tx\n"
        self.assertEqual(registry.make_all_recipe(makefile), "\tone\n\ttwo")

    def test_validate_reports_unregistered_check(self) -> None:
        errors = registry.validate(
            artifacts=[],
            exempt=[],
            targets=["check-new"],
            all_recipe="",
            workflow_text="",
            missing_outputs=set(),
        )
        self.assertEqual(len(errors), 1)
        self.assertIn("neither registered", errors[0])

    def test_validate_accepts_registered_artifact(self) -> None:
        errors = registry.validate(
            artifacts=[
                {
                    "id": "x",
                    "gen": "gen-x",
                    "check": "check-x",
                    "ci": True,
                    "make_all": True,
                    "outputs": ["generated.txt"],
                }
            ],
            exempt=[],
            targets=["all", "gen-x", "check-x"],
            all_recipe="\tmake check-x\n",
            workflow_text="make check-x\n",
            missing_outputs=set(),
        )
        self.assertEqual(errors, [])


if __name__ == "__main__":
    unittest.main()
