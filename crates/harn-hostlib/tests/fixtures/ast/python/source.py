class Greeter:
    def __init__(self, name):
        self.name = name

    def greet(self):
        return f"hello {self.name}"


def shout(message):
    return message.upper()


async def fetch(url):
    return url
