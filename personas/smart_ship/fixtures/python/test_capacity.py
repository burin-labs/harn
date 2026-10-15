import os
import unittest

from capacity import IDLE_CAPACITY, capacity_for
from workers import WORKERS


class CapacityTests(unittest.TestCase):
    def test_every_worker_has_capacity(self):
        self.assertGreaterEqual(capacity_for(WORKERS), WORKERS)

    def test_capacity_stays_within_budget(self):
        self.assertLessEqual(capacity_for(WORKERS), 10)

    def test_reviewed_idle_limit_is_preserved(self):
        expected = int(os.environ["EXPECTED_IDLE_CAPACITY"])
        self.assertEqual(IDLE_CAPACITY, expected)
        self.assertEqual(capacity_for(1), expected)

    def test_controller_environment_does_not_reach_tests(self):
        self.assertIsNone(
            os.environ.get("SMART_SHIP_PARENT_ONLY_CANARY"),
            "controller environment must be absent",
        )
