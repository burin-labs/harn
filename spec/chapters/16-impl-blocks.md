## Impl blocks

Impl blocks attach methods to a struct type.

### Syntax

```harn
impl TypeName {
  fn method_name(self, arg) {
    // body -- self refers to the struct instance
  }
}
```

The first parameter of each method must be `self`, which receives the
struct instance the method is called on.

### Method calls

Methods are called using dot syntax on struct instances:

```harn
struct Point {
  x: int
  y: int
}

impl Point {
  fn distance(self) {
    return sqrt(self.x * self.x + self.y * self.y)
  }
  fn translate(self, dx, dy) {
    return Point { x: self.x + dx, y: self.y + dy }
  }
}

const p = Point { x: 3, y: 4 }
log(p.distance())           // 5.0
const p2 = p.translate(10, 20)
log(p2.x)                   // 13
```

When `instance.method(args)` is called, the VM looks up methods registered
by the `impl` block for the instance's struct type. The instance is
automatically passed as the `self` argument.

