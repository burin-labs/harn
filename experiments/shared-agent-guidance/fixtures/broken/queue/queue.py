import asyncio
from collections import deque


class QueueClosed(Exception):
    pass


class AsyncQueue:
    def __init__(self, capacity):
        self.capacity = capacity
        self.items = deque()
        self.putters = deque()
        self.getters = deque()
        self.closed = False

    async def put(self, value):
        if self.closed:
            raise QueueClosed()
        if len(self.items) >= self.capacity:
            waiter = asyncio.get_running_loop().create_future()
            self.putters.append(waiter)
            await waiter
        self.items.append(value)
        if self.getters:
            self.getters.popleft().set_result(None)

    async def get(self):
        if not self.items:
            if self.closed:
                raise QueueClosed()
            waiter = asyncio.get_running_loop().create_future()
            self.getters.append(waiter)
            await waiter
        value = self.items.popleft()
        if self.putters:
            self.putters.popleft().set_result(None)
        return value

    def close(self):
        self.closed = True
        if self.getters:
            self.getters.popleft().set_result(None)
