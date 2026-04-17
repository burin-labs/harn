//! Interface satisfaction (where-clause bounds + associated types).

use super::*;

#[test]
fn test_interface_constraint_return_type_mismatch() {
    let warns = iface_errors(
        r#"pipeline t(task) {
  interface Sizable {
fn size(self) -> int
  }
  struct Box { width: int }
  impl Box {
fn size(self) -> string { return "nope" }
  }
  fn measure<T>(item: T) where T: Sizable { log(item.size()) }
  measure(Box({width: 3}))
}"#,
    );
    assert_eq!(warns.len(), 1, "expected 1 warning, got: {:?}", warns);
    assert!(
        warns[0].contains("method 'size' returns 'string', expected 'int'"),
        "unexpected message: {}",
        warns[0]
    );
}

#[test]
fn test_interface_constraint_param_type_mismatch() {
    let warns = iface_errors(
        r#"pipeline t(task) {
  interface Processor {
fn process(self, x: int) -> string
  }
  struct MyProc { name: string }
  impl MyProc {
fn process(self, x: string) -> string { return x }
  }
  fn run_proc<T>(p: T) where T: Processor { log(p.process(42)) }
  run_proc(MyProc({name: "a"}))
}"#,
    );
    assert_eq!(warns.len(), 1, "expected 1 warning, got: {:?}", warns);
    assert!(
        warns[0].contains("method 'process' parameter 1 has type 'string', expected 'int'"),
        "unexpected message: {}",
        warns[0]
    );
}

#[test]
fn test_interface_constraint_missing_method() {
    let warns = iface_errors(
        r#"pipeline t(task) {
  interface Sizable {
fn size(self) -> int
  }
  struct Box { width: int }
  impl Box {
fn area(self) -> int { return self.width }
  }
  fn measure<T>(item: T) where T: Sizable { log(item.size()) }
  measure(Box({width: 3}))
}"#,
    );
    assert_eq!(warns.len(), 1, "expected 1 warning, got: {:?}", warns);
    assert!(
        warns[0].contains("missing method 'size'"),
        "unexpected message: {}",
        warns[0]
    );
}

#[test]
fn test_interface_constraint_param_count_mismatch() {
    let warns = iface_errors(
        r#"pipeline t(task) {
  interface Doubler {
fn double(self, x: int) -> int
  }
  struct Bad { v: int }
  impl Bad {
fn double(self) -> int { return self.v * 2 }
  }
  fn run_double<T>(d: T) where T: Doubler { log(d.double(3)) }
  run_double(Bad({v: 5}))
}"#,
    );
    assert_eq!(warns.len(), 1, "expected 1 warning, got: {:?}", warns);
    assert!(
        warns[0].contains("method 'double' has 0 parameter(s), expected 1"),
        "unexpected message: {}",
        warns[0]
    );
}

#[test]
fn test_interface_constraint_satisfied() {
    let warns = iface_errors(
        r#"pipeline t(task) {
  interface Sizable {
fn size(self) -> int
  }
  struct Box { width: int, height: int }
  impl Box {
fn size(self) -> int { return self.width * self.height }
  }
  fn measure<T>(item: T) where T: Sizable { log(item.size()) }
  measure(Box({width: 3, height: 4}))
}"#,
    );
    assert!(warns.is_empty(), "expected no warnings, got: {:?}", warns);
}

#[test]
fn test_interface_constraint_untyped_impl_compatible() {
    // Gradual typing: untyped impl return should not trigger warning
    let warns = iface_errors(
        r#"pipeline t(task) {
  interface Sizable {
fn size(self) -> int
  }
  struct Box { width: int }
  impl Box {
fn size(self) { return self.width }
  }
  fn measure<T>(item: T) where T: Sizable { log(item.size()) }
  measure(Box({width: 3}))
}"#,
    );
    assert!(warns.is_empty(), "expected no warnings, got: {:?}", warns);
}

#[test]
fn test_interface_constraint_int_float_covariance() {
    // int is compatible with float (covariance)
    let warns = iface_errors(
        r#"pipeline t(task) {
  interface Measurable {
fn value(self) -> float
  }
  struct Gauge { v: int }
  impl Gauge {
fn value(self) -> int { return self.v }
  }
  fn read_val<T>(g: T) where T: Measurable { log(g.value()) }
  read_val(Gauge({v: 42}))
}"#,
    );
    assert!(warns.is_empty(), "expected no warnings, got: {:?}", warns);
}

#[test]
fn test_interface_associated_type_constraint_satisfied() {
    let warns = iface_errors(
        r#"pipeline t(task) {
  interface Collection {
type Item
fn get(self, index: int) -> Item
  }
  struct Names {}
  impl Names {
fn get(self, index: int) -> string { return "ada" }
  }
  fn first<C>(collection: C) where C: Collection {
log(collection.get(0))
  }
  first(Names {})
}"#,
    );
    assert!(warns.is_empty(), "expected no warnings, got: {:?}", warns);
}

#[test]
fn test_interface_associated_type_default_mismatch() {
    let warns = iface_errors(
        r#"pipeline t(task) {
  interface IntCollection {
type Item = int
fn get(self, index: int) -> Item
  }
  struct Labels {}
  impl Labels {
fn get(self, index: int) -> string { return "oops" }
  }
  fn first<C>(collection: C) where C: IntCollection {
log(collection.get(0))
  }
  first(Labels {})
}"#,
    );
    assert_eq!(warns.len(), 1, "expected 1 warning, got: {:?}", warns);
    assert!(
        warns[0].contains("associated type 'Item' resolves to 'string', expected 'int'"),
        "unexpected message: {}",
        warns[0]
    );
}
