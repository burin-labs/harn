pub struct PureCase {
    pub id: &'static str,
    pub source: &'static str,
    pub entry: &'static str,
    pub input_json: &'static str,
    pub expected_json: &'static str,
}

pub const PURE_CASES: &[PureCase] = &[
    PureCase {
        id: "named-record-reducer",
        source: r#"
            type State = {count: int, history: list<int>}
            type Event = {kind: string, amount: int}
            type Input = {state: State, event: Event}

            fn reduce(input: Input) -> State {
              if input.event.kind == "reset" {
                return {count: 0, history: input.state.history + [0]}
              }
              const next = input.state.count + input.event.amount
              return {count: next, history: input.state.history + [next]}
            }
        "#,
        entry: "reduce",
        input_json: r#"{"state":{"count":2,"history":[1,2]},"event":{"kind":"increment","amount":3}}"#,
        expected_json: r#"{"count":5,"history":[1,2,5]}"#,
    },
    PureCase {
        id: "closure-capture-and-control-flow",
        source: r#"
            fn transform(input) {
              const offset = input.offset
              const add_offset = {value -> value + offset}
              if input.enabled {
                return {value: add_offset(input.value), enabled: true}
              }
              return {value: input.value, enabled: false}
            }
        "#,
        entry: "transform",
        input_json: r#"{"value":7,"offset":4,"enabled":true}"#,
        expected_json: r#"{"enabled":true,"value":11}"#,
    },
    PureCase {
        id: "recursive-and-sibling-functions",
        source: r#"
            fn fib(n: int) -> int {
              if n <= 1 { return n }
              return fib(n - 1) + fib(n - 2)
            }
            fn solve(input: int) -> int { return fib(input) }
        "#,
        entry: "solve",
        input_json: "7",
        expected_json: "13",
    },
    PureCase {
        id: "forward-declared-capturing-function",
        source: r#"
            fn reduce(input: int) -> int {
              const offset = 3
              return add_offset(input)
              fn add_offset(value: int) -> int { return value + offset }
            }
        "#,
        entry: "reduce",
        input_json: "4",
        expected_json: "7",
    },
    PureCase {
        id: "typed-rest-and-sibling-call",
        source: r#"
            fn collect(...values: int) -> list<int> { return values }
            fn reduce(input: int) -> list<int> { return collect(input, 2) }
        "#,
        entry: "reduce",
        input_json: "7",
        expected_json: "[7,2]",
    },
    PureCase {
        id: "list-string-and-record-operations",
        source: r#"
            fn summarize(input) {
              return {
                items: input.left + input.right,
                title: input.prefix + input.name,
                found: input.left.contains(input.needle),
                fields: input.meta.count(),
              }
            }
        "#,
        entry: "summarize",
        input_json: r#"{"left":[1,2],"right":[3],"prefix":"Harn ","name":"Kernel","needle":2,"meta":{"a":1,"b":2}}"#,
        expected_json: r#"{"fields":2,"found":true,"items":[1,2,3],"title":"Harn Kernel"}"#,
    },
    PureCase {
        id: "list-ordering-runtime-and-constant-folding",
        source: r#"
            fn compare_lists(input) {
              return {
                runtime_less: input.left < input.right,
                runtime_equal: input.left <= input.left,
                constant_less: [1, 2] < [1, 3],
                constant_greater: [2] > [1, 9],
              }
            }
        "#,
        entry: "compare_lists",
        input_json: r#"{"left":[1,2],"right":[1,3]}"#,
        expected_json: r#"{"constant_greater":true,"constant_less":true,"runtime_equal":true,"runtime_less":true}"#,
    },
    PureCase {
        id: "structured-throw-catch",
        source: r#"
            fn validate(input) {
              try {
                if input.value < 0 {
                  throw {code: "negative", value: input.value}
                }
                return {ok: true, value: input.value}
              } catch error {
                return {ok: false, value: error.value, code: error.code}
              }
            }
        "#,
        entry: "validate",
        input_json: r#"{"value":-9}"#,
        expected_json: r#"{"code":"negative","ok":false,"value":-9}"#,
    },
    PureCase {
        id: "negative-index-and-slice",
        source: r#"
            fn choose(input) {
              return {
                last: input.values[-1],
                middle: input.values[-4:-1],
                suffix: input.text[-3:],
              }
            }
        "#,
        entry: "choose",
        input_json: r#"{"values":[1,2,3,4,5],"text":"kernel"}"#,
        expected_json: r#"{"last":5,"middle":[2,3,4],"suffix":"nel"}"#,
    },
];

pub struct InvalidCase {
    pub id: &'static str,
    pub source: &'static str,
    pub entry: &'static str,
    pub expected_code: &'static str,
}

pub const INVALID_CASES: &[InvalidCase] = &[
    InvalidCase {
        id: "frontend-syntax-error",
        source: "fn reduce( {",
        entry: "reduce",
        expected_code: "compile_frontend",
    },
    InvalidCase {
        id: "invalid-mutable-local-program",
        source: r#"
            fn reduce(input) {
              var value = input
              value = value + 1
              return value
            }
        "#,
        entry: "reduce",
        expected_code: "compile_frontend",
    },
];
