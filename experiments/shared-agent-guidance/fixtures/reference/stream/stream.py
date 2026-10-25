import codecs
import json


class JsonLinesDecoder:
    def __init__(self, max_line_bytes=1024):
        if type(max_line_bytes) is not int or max_line_bytes <= 0:
            raise ValueError("invalid limit")
        self.limit = max_line_bytes
        self.buffer = b""
        self.closed = False

    def _bound(self, line):
        if len(line.removesuffix(b"\r")) > self.limit:
            raise ValueError("line too long")

    def feed(self, chunk):
        if self.closed:
            raise ValueError("closed")
        lines = (self.buffer + chunk).split(b"\n")
        pending = lines.pop()
        values = []
        for line in lines:
            self._bound(line)
            text = line.decode("utf-8").removesuffix("\r")
            if text:
                values.append(json.loads(text))
        self._bound(pending)
        codecs.getincrementaldecoder("utf-8")().decode(pending, final=False)
        self.buffer = pending
        return values

    def finish(self):
        if self.closed:
            raise ValueError("closed")
        text = self.buffer.decode("utf-8").removesuffix("\r")
        values = [json.loads(text)] if text else []
        self.closed = True
        self.buffer = b""
        return values
