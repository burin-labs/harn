public class Greeter {
    private final String name;

    public Greeter(String name) {
        this.name = name;
    }

    public String greet() {
        return "hello " + name;
    }
}

interface Speaker {
    String speak();
}

enum Color {
    RED,
    GREEN,
    BLUE
}
