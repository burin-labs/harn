import asyncio
import importlib.util
import io
import json
import pathlib
import sys
import unittest

CELL, ROOT = sys.argv[1:3]
module_path = pathlib.Path(ROOT) / f"{CELL}.py"
spec = importlib.util.spec_from_file_location("repair_candidate", module_path)
candidate = importlib.util.module_from_spec(spec)
spec.loader.exec_module(candidate)


class LedgerTests(unittest.TestCase):
    def test_transfer_and_snapshot_isolation(self):
        original = {"a": 20, "b": 0}
        ledger = candidate.Ledger(original)
        original["a"] = 99
        self.assertEqual(ledger.balances(), {"a": 20, "b": 0})
        snapshot = ledger.balances()
        snapshot["a"] = 77
        self.assertEqual(ledger.transfer("x", "a", "b", 5), ("x", 15, 5))
        self.assertEqual(original, {"a": 99, "b": 0})

    def test_invalid_requests_do_not_mutate_or_consume_id(self):
        for source, destination, amount in [
            ("a", "b", 0), ("a", "b", -1), ("a", "b", True),
            ("a", "b", 1.5), ("a", "b", "2"), ("a", "b", 21),
            ("missing", "b", 1), ("a", "missing", 1), ("a", "a", 1),
        ]:
            with self.subTest(source=source, destination=destination, amount=amount):
                ledger = candidate.Ledger({"a": 20, "b": 0})
                with self.assertRaises(ValueError):
                    ledger.transfer("x", source, destination, amount)
                self.assertEqual(ledger.balances(), {"a": 20, "b": 0})
                self.assertEqual(ledger.transfer("x", "a", "b", 2), ("x", 18, 2))

    def test_retries_return_original_receipt(self):
        ledger = candidate.Ledger({"a": 20, "b": 0})
        receipt = ledger.transfer("x", "a", "b", 5)
        ledger.transfer("later", "b", "a", 2)
        self.assertEqual(ledger.transfer("x", "a", "b", 5), receipt)
        self.assertEqual(ledger.balances(), {"a": 17, "b": 3})
        for args in [("a", "b", 4), ("b", "a", 5), ("a", "a", 5), ("a", "b", True)]:
            with self.subTest(args=args), self.assertRaises(ValueError):
                ledger.transfer("x", *args)

    def test_batch_rolls_back_balances_and_receipts(self):
        ledger = candidate.Ledger({"a": 20, "b": 0, "c": 0})
        with self.assertRaises(ValueError):
            ledger.batch([("x", "a", "b", 5), ("y", "b", "c", 9)])
        self.assertEqual(ledger.balances(), {"a": 20, "b": 0, "c": 0})
        self.assertEqual(ledger.transfer("x", "a", "b", 3), ("x", 17, 3))

    def test_batch_includes_retry_history(self):
        ledger = candidate.Ledger({"a": 20, "b": 0})
        original = ledger.transfer("old", "a", "b", 2)
        with self.assertRaises(ValueError):
            ledger.batch([("new", "a", "b", 1), ("old", "a", "b", 3)])
        self.assertEqual(ledger.transfer("old", "a", "b", 2), original)
        self.assertEqual(ledger.transfer("new", "a", "b", 4), ("new", 14, 6))
        self.assertEqual(ledger.batch([]), [])

    def test_batch_success_and_internal_retry(self):
        ledger = candidate.Ledger({"a": 20, "b": 0})
        self.assertEqual(ledger.batch([("x", "a", "b", 2), ("x", "a", "b", 2), ("y", "b", "a", 1)]), [("x", 18, 2), ("x", 18, 2), ("y", 1, 19)])


class StreamTests(unittest.TestCase):
    def test_every_utf8_split(self):
        wire = '{"text":"雪☃"}\r\n42\n'.encode()
        for split in range(len(wire) + 1):
            with self.subTest(split=split):
                decoder = candidate.JsonLinesDecoder(100)
                values = decoder.feed(wire[:split]) + decoder.feed(wire[split:]) + decoder.finish()
                self.assertEqual(values, [{"text": "雪☃"}, 42])

    def test_many_short_lines_are_not_one_long_line(self):
        decoder = candidate.JsonLinesDecoder(2)
        self.assertEqual(decoder.feed(b'1\n2\r\n\n\r\n3\n'), [1, 2, 3])
        self.assertEqual(decoder.finish(), [])

    def test_exact_byte_limit_and_optional_cr(self):
        decoder = candidate.JsonLinesDecoder(5)
        self.assertEqual(decoder.feed('"雪"'.encode()), [])
        self.assertEqual(decoder.feed(b'\r'), [])
        self.assertEqual(decoder.feed(b'\n'), ["雪"])
        self.assertEqual(decoder.finish(), [])

    def test_multibyte_overflow_rolls_back(self):
        decoder = candidate.JsonLinesDecoder(4)
        with self.assertRaises(ValueError):
            decoder.feed('"雪"\n'.encode())
        self.assertEqual(decoder.feed(b'null\n'), [None])

    def test_bad_chunk_rolls_back_complete_lines_and_partial_state(self):
        for bad in [b'1\ninvalid\n', b'1\n\xff', b'1\n123456789']:
            with self.subTest(bad=bad):
                decoder = candidate.JsonLinesDecoder(8)
                self.assertEqual(decoder.feed(b' '), [])
                with self.assertRaises(ValueError):
                    decoder.feed(bad)
                self.assertEqual(decoder.feed(b'2\n'), [2])
                self.assertEqual(decoder.finish(), [])

    def test_partial_utf8_finish_can_be_repaired(self):
        decoder = candidate.JsonLinesDecoder(20)
        self.assertEqual(decoder.feed(b'"\xe9'), [])
        with self.assertRaises(ValueError):
            decoder.finish()
        self.assertEqual(decoder.feed(b'\x9b\xaa"'), [])
        self.assertEqual(decoder.finish(), ["雪"])

    def test_failed_json_finish_can_be_repaired(self):
        decoder = candidate.JsonLinesDecoder(20)
        decoder.feed(b'{"a":')
        with self.assertRaises(ValueError):
            decoder.finish()
        decoder.feed(b'1}')
        self.assertEqual(decoder.finish(), [{"a": 1}])
        for action in [lambda: decoder.feed(b''), decoder.finish]:
            with self.assertRaises(ValueError):
                action()

    def test_limits_are_positive_integers(self):
        for limit in [0, -1, True, 1.5, "5"]:
            with self.subTest(limit=limit), self.assertRaises(ValueError):
                candidate.JsonLinesDecoder(limit)


async def settle(task):
    for _ in range(16):
        if task.done():
            return task.result()
        await asyncio.sleep(0)
    raise AssertionError("waiter never made progress across 16 scheduler yields")


async def parked(coro):
    task = asyncio.create_task(coro)
    await asyncio.sleep(0)
    return task


class QueueTests(unittest.IsolatedAsyncioTestCase):
    async def test_fifo_and_backpressure(self):
        queue = candidate.AsyncQueue(1)
        await queue.put(1)
        first = await parked(queue.put(2))
        second = await parked(queue.put(3))
        self.assertFalse(first.done())
        self.assertFalse(second.done())
        self.assertEqual(await queue.get(), 1)
        await settle(first)
        self.assertFalse(second.done())
        self.assertEqual(await queue.get(), 2)
        await settle(second)
        self.assertEqual(await queue.get(), 3)

    async def test_cancel_blocked_producer(self):
        queue = candidate.AsyncQueue(1)
        await queue.put("first")
        cancelled = await parked(queue.put("cancelled"))
        survivor = await parked(queue.put("survivor"))
        cancelled.cancel()
        with self.assertRaises(asyncio.CancelledError):
            await cancelled
        self.assertEqual(await queue.get(), "first")
        await settle(survivor)
        self.assertEqual(await queue.get(), "survivor")

    async def test_cancel_notified_producer(self):
        queue = candidate.AsyncQueue(1)
        await queue.put(1)
        cancelled = await parked(queue.put(2))
        survivor = await parked(queue.put(3))
        self.assertEqual(await queue.get(), 1)
        cancelled.cancel()
        with self.assertRaises(asyncio.CancelledError):
            await cancelled
        await settle(survivor)
        self.assertEqual(await queue.get(), 3)

    async def test_cancel_notified_consumer(self):
        queue = candidate.AsyncQueue(1)
        cancelled = await parked(queue.get())
        survivor = await parked(queue.get())
        await queue.put("kept")
        cancelled.cancel()
        with self.assertRaises(asyncio.CancelledError):
            await cancelled
        self.assertEqual(await settle(survivor), "kept")

    async def test_cancel_blocked_consumer(self):
        queue = candidate.AsyncQueue(1)
        cancelled = await parked(queue.get())
        survivor = await parked(queue.get())
        cancelled.cancel()
        with self.assertRaises(asyncio.CancelledError):
            await cancelled
        await queue.put("kept")
        self.assertEqual(await settle(survivor), "kept")

    async def test_close_drains_and_rejects_putters(self):
        queue = candidate.AsyncQueue(2)
        await queue.put(1)
        await queue.put(2)
        first = await parked(queue.put(3))
        second = await parked(queue.put(4))
        queue.close()
        queue.close()
        for task in [first, second]:
            with self.assertRaises(candidate.QueueClosed):
                await settle(task)
        with self.assertRaises(candidate.QueueClosed):
            await queue.put(5)
        self.assertEqual(await queue.get(), 1)
        self.assertEqual(await queue.get(), 2)
        with self.assertRaises(candidate.QueueClosed):
            await queue.get()

    async def test_close_wakes_all_consumers(self):
        queue = candidate.AsyncQueue(1)
        tasks = [await parked(queue.get()) for _ in range(3)]
        queue.close()
        for task in tasks:
            with self.assertRaises(candidate.QueueClosed):
                await settle(task)

    async def test_capacity_validation(self):
        for capacity in [0, -1, True, 1.5, "2"]:
            with self.subTest(capacity=capacity), self.assertRaises(ValueError):
                candidate.AsyncQueue(capacity)


suite_type = {"ledger": LedgerTests, "stream": StreamTests, "queue": QueueTests}[CELL]
suite = unittest.defaultTestLoader.loadTestsFromTestCase(suite_type)
expected = {"ledger": 6, "stream": 8, "queue": 8}[CELL]
assert suite.countTestCases() == expected
output = io.StringIO()
result = unittest.TextTestRunner(stream=output, verbosity=2).run(suite)
print(json.dumps({"cell": CELL, "measured": result.testsRun, "expected": expected,
                  "failed": len(result.failures), "errors": len(result.errors),
                  "skipped": len(result.skipped), "passed": result.wasSuccessful(),
                  "details": output.getvalue()}))
sys.exit(0 if result.wasSuccessful() and result.testsRun == expected and not result.skipped else 1)
