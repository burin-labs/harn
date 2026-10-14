//! The builtin value-method table. Every consumer (VM dispatch drift checks,
//! the portable kernel, the typechecker, the linter, the language server, and
//! the generated reference in `docs/src/builtins.md`) reads these rows.
//!
//! Signature conventions: `T` is the element type of a list, set, iter, or
//! generator receiver; `K`/`V` are a dict receiver's key and value types.
//! Those parameters are bound from the receiver, so each signature declares
//! them alongside any it binds from arguments.

use harn_builtin_macros::harn_method_sig as sig;
use harn_builtin_meta::MethodReceiver::{Dict, Generator, Iter, List, Range, Set, String};
use harn_builtin_meta::{MethodDescriptor as M, RemovedMethod as R};

pub static BUILTIN_METHODS: &[M] = &[
    // ---- string ----
    M::pure(String, sig!("len() -> int"), "Number of characters."),
    M::pure(
        String,
        sig!("empty() -> bool"),
        "True when the string has no characters.",
    ),
    M::pure(
        String,
        sig!("contains(needle: string) -> bool"),
        "True when `needle` occurs in the string.",
    ),
    M::pure(
        String,
        sig!("replace(old: string, new: string) -> string"),
        "Replace every occurrence of `old` with `new`.",
    ),
    M::pure(
        String,
        sig!("split(separator: string) -> list<string>"),
        "Split on every occurrence of `separator`.",
    ),
    M::pure(
        String,
        sig!("trim() -> string"),
        "Remove leading and trailing whitespace.",
    ),
    M::pure(
        String,
        sig!("trim_start() -> string"),
        "Remove leading whitespace.",
    ),
    M::pure(
        String,
        sig!("trim_end() -> string"),
        "Remove trailing whitespace.",
    ),
    M::pure(
        String,
        sig!("starts_with(prefix: string) -> bool"),
        "True when the string begins with `prefix`.",
    ),
    M::pure(
        String,
        sig!("ends_with(suffix: string) -> bool"),
        "True when the string ends with `suffix`.",
    ),
    M::pure(String, sig!("lower() -> string"), "Lowercase copy."),
    M::pure(String, sig!("upper() -> string"), "Uppercase copy."),
    M::pure(
        String,
        sig!("substring(start: int, end?: int) -> string"),
        "Characters from `start` up to `end` (default: the end).",
    ),
    M::pure(
        String,
        sig!("slice(start: int, end?: int) -> string"),
        "Characters from `start` up to `end`; negative indices count from the end.",
    ),
    M::pure(
        String,
        sig!("index_of(needle: string) -> int"),
        "Character offset of the first `needle`, or -1.",
    ),
    M::pure(
        String,
        sig!("last_index_of(needle: string) -> int"),
        "Character offset of the last `needle`, or -1.",
    ),
    M::pure(
        String,
        sig!("chars() -> list<string>"),
        "List of single-character strings.",
    ),
    M::pure(
        String,
        sig!("repeat(count: int) -> string"),
        "The string repeated `count` times.",
    ),
    M::pure(
        String,
        sig!("reversed() -> string"),
        "Characters in reverse order.",
    ),
    M::pure(
        String,
        sig!("pad_left(width: int, fill?: string) -> string"),
        "Pad on the left to `width` characters with `fill` (default: space).",
    ),
    M::pure(
        String,
        sig!("pad_right(width: int, fill?: string) -> string"),
        "Pad on the right to `width` characters with `fill` (default: space).",
    ),
    M::pure(String, sig!("lines() -> list<string>"), "Split into lines."),
    M::pure(
        String,
        sig!("char_at(index: int) -> string?"),
        "Character at `index`, or nil when out of range.",
    ),
    M::pure(
        String,
        sig!("iter() -> iter<string>"),
        "Lazy iterator over the characters.",
    ),
    // ---- list ----
    M::pure(List, sig!("len() -> int"), "Number of elements."),
    M::pure(
        List,
        sig!("empty() -> bool"),
        "True when the list has no elements.",
    ),
    M::pure(
        List,
        sig!("<T> first() -> T?"),
        "First element, or nil when empty.",
    ),
    M::pure(
        List,
        sig!("<T> last() -> T?"),
        "Last element, or nil when empty.",
    ),
    M::pure(
        List,
        sig!("<T, U> map(f: (T) -> U) -> list<U>"),
        "Apply `f` to each element.",
    ),
    M::pure(
        List,
        sig!("<T> filter(predicate: (T) -> bool) -> list<T>"),
        "Elements for which `predicate` is truthy.",
    ),
    M::pure(
        List,
        sig!("<T, U> flat_map(f: (T) -> list<U>) -> list<U>"),
        "Apply `f` and flatten one level.",
    ),
    M::pure(
        List,
        sig!("<T, U> reduce(initial: U, f: (U, T) -> U) -> U"),
        "Fold the elements into an accumulator.",
    ),
    M::pure(
        List,
        sig!("<T> find(predicate: (T) -> bool) -> T?"),
        "First element matching `predicate`, or nil.",
    ),
    M::pure(
        List,
        sig!("<T> find_index(predicate: (T) -> bool) -> int"),
        "Index of the first match, or -1.",
    ),
    M::pure(
        List,
        sig!("<T> any(predicate: (T) -> bool) -> bool"),
        "True when any element matches.",
    ),
    M::pure(
        List,
        sig!("<T> all(predicate: (T) -> bool) -> bool"),
        "True when every element matches.",
    ),
    M::pure(
        List,
        sig!("<T> none(predicate?: (T) -> bool) -> bool"),
        "True when no element matches; with no argument, true when empty.",
    ),
    M::pure(
        List,
        sig!("<T> partition(predicate: (T) -> bool) -> list<list<T>>"),
        "Split into `[matching, rest]`.",
    ),
    M::pure(
        List,
        sig!("<T, U> group_by(key: (T) -> U) -> dict<string, list<T>>"),
        "Group elements by `key`.",
    ),
    M::pure(
        List,
        sig!("<T, U> count_by(key: (T) -> U) -> dict<string, int>"),
        "Count elements per `key`.",
    ),
    M::pure(
        List,
        sig!("<T> sorted() -> list<T>"),
        "Sorted copy (stable, natural order).",
    ),
    M::pure(
        List,
        sig!("<T, U> sorted_by(key: (T) -> U) -> list<T>"),
        "Sorted copy ordered by `key`; list keys order lexicographically.",
    ),
    M::pure(
        List,
        sig!("<T> reversed() -> list<T>"),
        "Copy in reverse order.",
    ),
    M::pure(
        List,
        sig!("join(separator?: string) -> string"),
        "Join the elements' display text with `separator`.",
    ),
    M::pure(
        List,
        sig!("<T> contains(item: T) -> bool"),
        "True when an element equals `item`.",
    ),
    M::pure(
        List,
        sig!("<T> index_of(item: T) -> int"),
        "Index of the first element equal to `item`, or -1.",
    ),
    M::pure(
        List,
        sig!("<T> enumerate() -> list<{index: int, value: T}>"),
        "Pair each element with its index.",
    ),
    M::pure(
        List,
        sig!("<T, U> zip(other: list<U>) -> list<list<T | U>>"),
        "Pair elements with `other`, stopping at the shorter list.",
    ),
    M::pure(
        List,
        sig!("<T> slice(start: int, end?: int) -> list<T>"),
        "Elements from `start` up to `end`; negative indices count from the end.",
    ),
    M::pure(
        List,
        sig!("<T> take(count: int) -> list<T>"),
        "The first `count` elements.",
    ),
    M::pure(
        List,
        sig!("<T> skip(count: int) -> list<T>"),
        "All but the first `count` elements.",
    ),
    M::pure(
        List,
        sig!("<T> take_while(predicate: (T) -> bool) -> list<T>"),
        "Leading elements while `predicate` holds.",
    ),
    M::pure(
        List,
        sig!("<T> drop_while(predicate: (T) -> bool) -> list<T>"),
        "Elements after the leading run where `predicate` holds.",
    ),
    M::pure(
        List,
        sig!("<T> unique() -> list<T>"),
        "Copy without duplicates, keeping first occurrences.",
    ),
    M::pure(
        List,
        sig!("<T> compact() -> list<T>"),
        "Copy without nil elements.",
    ),
    M::pure(
        List,
        sig!("flatten() -> list<any>"),
        "Flatten one level of nesting.",
    ),
    M::pure(
        List,
        sig!("<T> chunks(size: int) -> list<list<T>>"),
        "Consecutive non-overlapping chunks of `size`.",
    ),
    M::pure(
        List,
        sig!("<T> windows(size: int, step?: int) -> list<list<T>>"),
        "Sliding windows of `size`, advancing by `step` (default 1).",
    ),
    M::pure(List, sig!("sum() -> any"), "Sum of the numeric elements."),
    M::pure(
        List,
        sig!("<T> min() -> T?"),
        "Smallest element, or nil when empty.",
    ),
    M::pure(
        List,
        sig!("<T> max() -> T?"),
        "Largest element, or nil when empty.",
    ),
    M::pure(
        List,
        sig!("<T, U> min_by(key: (T) -> U) -> T?"),
        "Element with the smallest `key`, or nil when empty.",
    ),
    M::pure(
        List,
        sig!("<T, U> max_by(key: (T) -> U) -> T?"),
        "Element with the largest `key`, or nil when empty.",
    ),
    M::pure(
        List,
        sig!("tally() -> dict<string, int>"),
        "Count occurrences of each string element.",
    ),
    M::pure(List, sig!("<T> to_list() -> list<T>"), "The list itself."),
    M::pure(List, sig!("<T> to_set() -> set<T>"), "Set of the elements."),
    M::pure(
        List,
        sig!("<T> iter() -> iter<T>"),
        "Lazy iterator over the elements.",
    ),
    M::mutating(
        List,
        sig!("<T> push(item: T) -> nil"),
        "Append `item` to the end.",
    ),
    M::mutating(
        List,
        sig!("<T> pop() -> T?"),
        "Remove and return the last element, or nil when empty.",
    ),
    M::mutating(
        List,
        sig!("<T> insert(index: int, item: T) -> nil"),
        "Insert `item` before `index`; out-of-range indices clamp to the ends.",
    ),
    M::mutating(
        List,
        sig!("<T> extend(items: list<T>) -> nil"),
        "Append every element of `items`.",
    ),
    M::mutating(List, sig!("clear() -> nil"), "Remove every element."),
    M::mutating(
        List,
        sig!("sort() -> nil"),
        "Sort in place (stable, natural order).",
    ),
    M::mutating(
        List,
        sig!("<T, U> sort_by(key: (T) -> U) -> nil"),
        "Sort in place ordered by `key`.",
    ),
    M::mutating(List, sig!("reverse() -> nil"), "Reverse in place."),
    // ---- set ----
    M::pure(Set, sig!("len() -> int"), "Number of members."),
    M::pure(
        Set,
        sig!("empty() -> bool"),
        "True when the set has no members.",
    ),
    M::pure(
        Set,
        sig!("<T> contains(item: T) -> bool"),
        "True when `item` is a member.",
    ),
    M::pure(
        Set,
        sig!("<T> union(other: set<T>) -> set<T>"),
        "Members of either set.",
    ),
    M::pure(
        Set,
        sig!("<T> intersection(other: set<T>) -> set<T>"),
        "Members of both sets.",
    ),
    M::pure(
        Set,
        sig!("<T> difference(other: set<T>) -> set<T>"),
        "Members not in `other`.",
    ),
    M::pure(
        Set,
        sig!("<T> symmetric_difference(other: set<T>) -> set<T>"),
        "Members of exactly one set.",
    ),
    M::pure(
        Set,
        sig!("<T> is_subset(other: set<T>) -> bool"),
        "True when every member is in `other`.",
    ),
    M::pure(
        Set,
        sig!("<T> is_superset(other: set<T>) -> bool"),
        "True when every member of `other` is a member.",
    ),
    M::pure(
        Set,
        sig!("<T> is_disjoint(other: set<T>) -> bool"),
        "True when the sets share no member.",
    ),
    M::pure(
        Set,
        sig!("<T, U> map(f: (T) -> U) -> set<U>"),
        "Apply `f` to each member.",
    ),
    M::pure(
        Set,
        sig!("<T> filter(predicate: (T) -> bool) -> set<T>"),
        "Members for which `predicate` is truthy.",
    ),
    M::pure(
        Set,
        sig!("<T> any(predicate: (T) -> bool) -> bool"),
        "True when any member matches.",
    ),
    M::pure(
        Set,
        sig!("<T> all(predicate: (T) -> bool) -> bool"),
        "True when every member matches.",
    ),
    M::pure(
        Set,
        sig!("<T> to_list() -> list<T>"),
        "Members in insertion order.",
    ),
    M::pure(Set, sig!("<T> to_set() -> set<T>"), "The set itself."),
    M::pure(
        Set,
        sig!("<T> iter() -> iter<T>"),
        "Lazy iterator over the members.",
    ),
    M::mutating(Set, sig!("<T> add(item: T) -> nil"), "Add `item`."),
    M::mutating(
        Set,
        sig!("<T> remove(item: T) -> bool"),
        "Remove `item`; true when it was a member.",
    ),
    M::mutating(Set, sig!("clear() -> nil"), "Remove every member."),
    // ---- dict ----
    M::pure(Dict, sig!("len() -> int"), "Number of entries."),
    M::pure(Dict, sig!("<K> keys() -> list<K>"), "Keys in sorted order."),
    M::pure(
        Dict,
        sig!("<V> values() -> list<V>"),
        "Values in key order.",
    ),
    M::pure(
        Dict,
        sig!("<K, V> entries() -> list<{key: K, value: V}>"),
        "`{key, value}` records in key order.",
    ),
    M::pure(
        Dict,
        sig!("has(key: string) -> bool"),
        "True when `key` is present.",
    ),
    // The result is `V` with a default and `V?` without one; a signature
    // cannot say both, so it stays gradual.
    M::pure(
        Dict,
        sig!("get(key: string, default?: any) -> any"),
        "Value at `key`, else `default` (nil when omitted).",
    ),
    M::pure(
        Dict,
        sig!("<K, V> merge(other: dict<K, V>) -> dict<K, V>"),
        "Copy with `other`'s entries added; `other` wins on conflicts.",
    ),
    M::pure(
        Dict,
        sig!("<K, V, U> map_values(f: (V) -> U) -> dict<K, U>"),
        "Apply `f` to each value.",
    ),
    M::pure(
        Dict,
        sig!("<K, V> map_keys(f: (K) -> string) -> dict<string, V>"),
        "Rename each key to `f(key)`.",
    ),
    M::pure(
        Dict,
        sig!("<K, V> filter(predicate: (V) -> bool) -> dict<K, V>"),
        "Entries whose value satisfies `predicate`.",
    ),
    M::pure(
        Dict,
        sig!("<K, V> to_dict() -> dict<K, V>"),
        "The dict itself.",
    ),
    M::pure(
        Dict,
        sig!("<K, V> to_list() -> list<{key: K, value: V}>"),
        "Same as `entries()`.",
    ),
    M::pure(
        Dict,
        sig!("<K, V> iter() -> iter<Pair<K, V>>"),
        "Lazy iterator over `Pair(key, value)`.",
    ),
    M::mutating(
        Dict,
        sig!("<V> remove(key: string) -> V?"),
        "Remove `key` and return its value, or nil when absent.",
    ),
    M::mutating(Dict, sig!("clear() -> nil"), "Remove every entry."),
    // ---- range ----
    M::pure(
        Range,
        sig!("len() -> int"),
        "Number of integers in the range.",
    ),
    M::pure(
        Range,
        sig!("empty() -> bool"),
        "True when the range is empty.",
    ),
    M::pure(
        Range,
        sig!("contains(value: int) -> bool"),
        "True when `value` is in the range.",
    ),
    M::pure(
        Range,
        sig!("first() -> int?"),
        "First integer, or nil when empty.",
    ),
    M::pure(
        Range,
        sig!("last() -> int?"),
        "Last integer, or nil when empty.",
    ),
    M::pure(
        Range,
        sig!("to_string() -> string"),
        "Source form such as `1..=5`.",
    ),
    M::pure(
        Range,
        sig!("iter() -> iter<int>"),
        "Lazy iterator; other iterator methods lift the range automatically.",
    ),
    // ---- iter ----
    M::pure(Iter, sig!("<T> iter() -> iter<T>"), "The iterator itself."),
    M::pure(
        Iter,
        sig!("<T, U> map(f: (T) -> U) -> iter<U>"),
        "Lazily apply `f` to each item.",
    ),
    M::pure(
        Iter,
        sig!("<T, U> flat_map(f: (T) -> list<U>) -> iter<U>"),
        "Lazily apply `f` and flatten.",
    ),
    M::pure(
        Iter,
        sig!("<T> filter(predicate: (T) -> bool) -> iter<T>"),
        "Lazily keep items matching `predicate`.",
    ),
    M::pure(
        Iter,
        sig!("<T> take(count: int) -> iter<T>"),
        "The first `count` items.",
    ),
    M::pure(
        Iter,
        sig!("<T> skip(count: int) -> iter<T>"),
        "Drop the first `count` items.",
    ),
    M::pure(
        Iter,
        sig!("<T> take_while(predicate: (T) -> bool) -> iter<T>"),
        "Items until `predicate` first fails.",
    ),
    M::pure(
        Iter,
        sig!("<T> skip_while(predicate: (T) -> bool) -> iter<T>"),
        "Drop items while `predicate` holds.",
    ),
    M::pure(
        Iter,
        sig!("<T, U> zip(other: iter<U>) -> iter<Pair<T, U>>"),
        "Pair items with `other`, stopping at the shorter.",
    ),
    M::pure(
        Iter,
        sig!("<T> enumerate() -> iter<Pair<int, T>>"),
        "Pair each item with a 0-based index.",
    ),
    M::pure(
        Iter,
        sig!("<T> chain(other: iter<T>) -> iter<T>"),
        "Items of this iterator, then of `other`.",
    ),
    M::pure(
        Iter,
        sig!("<T> chunks(size: int) -> iter<list<T>>"),
        "Non-overlapping chunks of `size`.",
    ),
    M::pure(
        Iter,
        sig!("<T> windows(size: int) -> iter<list<T>>"),
        "Sliding windows of `size`.",
    ),
    M::pure(Iter, sig!("<T> to_list() -> list<T>"), "Drain into a list."),
    M::pure(Iter, sig!("<T> to_set() -> set<T>"), "Drain into a set."),
    M::pure(
        Iter,
        sig!("to_dict() -> dict<string, any>"),
        "Drain `Pair(key, value)` items into a dict.",
    ),
    M::pure(Iter, sig!("count() -> int"), "Drain and count the items."),
    M::pure(
        Iter,
        sig!("sum() -> number"),
        "Drain and sum numeric items.",
    ),
    M::pure(
        Iter,
        sig!("<T> min() -> T?"),
        "Drain and return the smallest item.",
    ),
    M::pure(
        Iter,
        sig!("<T> max() -> T?"),
        "Drain and return the largest item.",
    ),
    M::pure(
        Iter,
        sig!("<T> first() -> T?"),
        "The next item, or nil when exhausted.",
    ),
    M::pure(
        Iter,
        sig!("<T> last() -> T?"),
        "Drain and return the final item.",
    ),
    M::pure(
        Iter,
        sig!("<T> find(predicate: (T) -> bool) -> T?"),
        "First item matching `predicate`.",
    ),
    M::pure(
        Iter,
        sig!("<T> any(predicate: (T) -> bool) -> bool"),
        "True when any item matches.",
    ),
    M::pure(
        Iter,
        sig!("<T> all(predicate: (T) -> bool) -> bool"),
        "True when every item matches.",
    ),
    M::pure(
        Iter,
        sig!("<T> for_each(f: (T) -> any) -> nil"),
        "Drain, calling `f` on each item.",
    ),
    M::pure(
        Iter,
        sig!("<T, U> reduce(initial: U, f: (U, T) -> U) -> U"),
        "Drain into an accumulator.",
    ),
    // ---- generator ----
    M::pure(
        Generator,
        sig!("<T> iter() -> iter<T>"),
        "Lazy iterator over the yielded values.",
    ),
    M::pure(
        Generator,
        sig!("<T> next() -> {done: bool, value: T?}"),
        "Resume and return the next yielded value.",
    ),
];

pub static REMOVED_METHODS: &[R] = &[
    R::renamed(String, "count", "len"),
    R::renamed(String, "includes", "contains"),
    R::renamed(String, "lowercase", "lower"),
    R::renamed(String, "to_lower", "lower"),
    R::renamed(String, "uppercase", "upper"),
    R::renamed(String, "to_upper", "upper"),
    R::renamed(String, "reverse", "reversed"),
    R::renamed(String, "rfind", "last_index_of"),
    R::renamed(List, "count", "len"),
    R::renamed(List, "includes", "contains"),
    R::renamed(List, "every", "all"),
    R::renamed(List, "chunk", "chunks"),
    R::renamed(List, "each_slice", "chunks"),
    R::renamed(List, "window", "windows"),
    R::renamed(List, "each_cons", "windows"),
    R::renamed(List, "sliding_window", "windows"),
    R {
        receiver: List,
        name: "appending",
        migration: harn_builtin_meta::MethodMigration::AppendingToPush,
    },
    R::mutating_when_self_assigned(
        List,
        "dropping_last",
        "pop",
        "use `xs.pop()` to remove the last element in place, or `xs.slice(0, -1)` for a new list",
    ),
    R::renamed(Set, "count", "len"),
    R::renamed(Set, "includes", "contains"),
    R::renamed(Set, "every", "all"),
    R::renamed(Set, "intersect", "intersection"),
    R::mutating_when_self_assigned(
        Set,
        "adding",
        "add",
        "use `s.add(v)` on a `let` binding, or `s.union(set([v]))` for a new set",
    ),
    R::mutating_when_self_assigned(
        Set,
        "removing",
        "remove",
        "use `s.remove(v)` on a `let` binding, or `s.difference(set([v]))` for a new set",
    ),
    R::mutating_when_self_assigned(
        Set,
        "delete",
        "remove",
        "use `s.remove(v)` on a `let` binding, or `s.difference(set([v]))` for a new set",
    ),
    R::renamed(Dict, "count", "len"),
    R::renamed(Dict, "merging", "merge"),
    R::renamed(Dict, "rekeyed", "map_keys"),
    R::renamed(Dict, "rekey", "map_keys"),
    R::mutating_when_self_assigned(
        Dict,
        "removing",
        "remove",
        "use `d.remove(key)` on a `let` binding, or `omit(d, [key])` from `std/json` for a new dict",
    ),
    R::renamed(Range, "count", "len"),
    R::renamed(Range, "includes", "contains"),
];
