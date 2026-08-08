#include <stdio.h>

struct Greeter {
    const char *name;
};

enum Color {
    RED,
    GREEN,
    BLUE
};

typedef int Counter;

int add(int a, int b) {
    return a + b;
}

void greet(struct Greeter *g) {
    printf("hello %s\n", g->name);
}
