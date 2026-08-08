namespace fixture {

class Greeter {
public:
    Greeter(const char *name) : name_(name) {}
    std::string greet() const { return std::string("hello ") + name_; }

private:
    const char *name_;
};

struct Point {
    int x;
    int y;
};

enum Color { RED, GREEN, BLUE };

int add(int a, int b) {
    return a + b;
}

}
