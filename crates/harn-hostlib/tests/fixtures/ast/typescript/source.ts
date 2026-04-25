interface Greeter {
    greet(name: string): string;
}

class HelloGreeter implements Greeter {
    greet(name: string): string {
        return `hello ${name}`;
    }
}

function shout(message: string): string {
    return message.toUpperCase();
}

const yell = (message: string) => shout(message);

type Maybe<T> = T | null;
