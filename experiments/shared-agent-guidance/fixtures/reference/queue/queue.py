import asyncio


class QueueClosed(Exception):
    pass


class AsyncQueue:
    def __init__(self, capacity):
        if type(capacity) is not int or capacity <= 0:
            raise ValueError("invalid capacity")
        self._queue = asyncio.Queue(capacity)

    async def put(self, value):
        try:
            await self._queue.put(value)
        except asyncio.QueueShutDown:
            raise QueueClosed() from None

    async def get(self):
        try:
            return await self._queue.get()
        except asyncio.QueueShutDown:
            raise QueueClosed() from None

    def close(self):
        self._queue.shutdown()
