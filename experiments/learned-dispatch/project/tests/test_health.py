import unittest

from app.http import App


class HealthTest(unittest.TestCase):
    def test_health(self) -> None:
        self.assertEqual(App().dispatch("GET", "/health"), (200, {"status": "ok"}))

    def test_unknown_resource(self) -> None:
        self.assertEqual(App().dispatch("GET", "/api/missing"), (404, {"error": "not_found"}))
