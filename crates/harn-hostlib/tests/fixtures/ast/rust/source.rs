struct Greeter {
    name: String,
}

enum Color {
    Red,
    Green,
    Blue,
}

trait Speak {
    fn speak(&self) -> String;
}

impl Greeter {
    fn greet(&self) -> String {
        format!("hello {}", self.name)
    }
}

fn shout(s: &str) -> String {
    s.to_uppercase()
}

type Counter = i64;
