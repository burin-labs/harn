// Demo seed for the conflict path. Both `Widget` and `Gadget` live in
// this file, so renaming Widget -> Gadget would create a duplicate
// identifier — the host short-circuits with `result: "conflict"` and
// never writes.

pub struct Widget {}
pub struct Gadget {}

fn main() {
    let _ = Widget {};
    let _ = Gadget {};
}
