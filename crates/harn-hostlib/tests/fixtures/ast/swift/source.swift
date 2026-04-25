protocol Speaker {
    func speak() -> String
}

struct Point {
    var x: Int
    var y: Int
}

enum Color {
    case red
    case green
    case blue
}

class Greeter: Speaker {
    let name: String

    init(name: String) {
        self.name = name
    }

    func speak() -> String {
        return "hello \(name)"
    }
}

func shout(_ s: String) -> String {
    return s.uppercased()
}
