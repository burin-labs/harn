import json


class JsonLinesDecoder:
    def __init__(self, max_line_bytes=1024):
        self.limit = max_line_bytes
        self.buffer = ""
        self.closed = False

    def feed(self, chunk):
        if self.closed:
            raise ValueError("closed")
        self.buffer += chunk.decode("utf-8")
        if len(self.buffer) > self.limit:
            raise ValueError("line too long")
        lines = self.buffer.split("\n")
        self.buffer = lines.pop()
        return [json.loads(line) for line in lines if line]

    def finish(self):
        self.closed = True
        return [json.loads(self.buffer)] if self.buffer else []
