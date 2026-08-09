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
        source: r"
            fn transform(input) {
              const offset = input.offset
              const add_offset = {value -> value + offset}
              if input.enabled {
                return {value: add_offset(input.value), enabled: true}
              }
              return {value: input.value, enabled: false}
            }
        ",
        entry: "transform",
        input_json: r#"{"value":7,"offset":4,"enabled":true}"#,
        expected_json: r#"{"enabled":true,"value":11}"#,
    },
    PureCase {
        id: "recursive-and-sibling-functions",
        source: r"
            fn fib(n: int) -> int {
              if n <= 1 { return n }
              return fib(n - 1) + fib(n - 2)
            }
            fn solve(input: int) -> int { return fib(input) }
        ",
        entry: "solve",
        input_json: "7",
        expected_json: "13",
    },
    PureCase {
        id: "forward-declared-capturing-function",
        source: r"
            fn reduce(input: int) -> int {
              const offset = 3
              return add_offset(input)
              fn add_offset(value: int) -> int { return value + offset }
            }
        ",
        entry: "reduce",
        input_json: "4",
        expected_json: "7",
    },
    PureCase {
        id: "typed-rest-and-sibling-call",
        source: r"
            fn collect(...values: int) -> list<int> { return values }
            fn reduce(input: int) -> list<int> { return collect(input, 2) }
        ",
        entry: "reduce",
        input_json: "7",
        expected_json: "[7,2]",
    },
    PureCase {
        id: "list-string-and-record-operations",
        source: r"
            fn summarize(input) {
              return {
                items: input.left + input.right,
                title: input.prefix + input.name,
                found: input.left.contains(input.needle),
                fields: input.meta.count(),
              }
            }
        ",
        entry: "summarize",
        input_json: r#"{"left":[1,2],"right":[3],"prefix":"Harn ","name":"Kernel","needle":2,"meta":{"a":1,"b":2}}"#,
        expected_json: r#"{"fields":2,"found":true,"items":[1,2,3],"title":"Harn Kernel"}"#,
    },
    PureCase {
        id: "list-ordering-runtime-and-constant-folding",
        source: r"
            fn compare_lists(input) {
              return {
                runtime_less: input.left < input.right,
                runtime_equal: input.left <= input.left,
                constant_less: [1, 2] < [1, 3],
                constant_greater: [2] > [1, 9],
              }
            }
        ",
        entry: "compare_lists",
        input_json: r#"{"left":[1,2],"right":[1,3]}"#,
        expected_json: r#"{"constant_greater":true,"constant_less":true,"runtime_equal":true,"runtime_less":true}"#,
    },
    PureCase {
        id: "mixed-type-equality-is-structural-not-ordering",
        source: r"
            fn compare(input) {
              return {
                string_is_not_nil: input.value != nil,
                string_is_not_int: input.value != 7,
                nil_is_nil: nil == nil,
                numeric_cross_kind: 1 == 1.0,
              }
            }
        ",
        entry: "compare",
        input_json: r#"{"value":"ui://portable"}"#,
        expected_json: r#"{"nil_is_nil":true,"numeric_cross_kind":true,"string_is_not_int":true,"string_is_not_nil":true}"#,
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
        source: r"
            fn choose(input) {
              return {
                last: input.values[-1],
                middle: input.values[-4:-1],
                suffix: input.text[-3:],
              }
            }
        ",
        entry: "choose",
        input_json: r#"{"values":[1,2,3,4,5],"text":"kernel"}"#,
        expected_json: r#"{"last":5,"middle":[2,3,4],"suffix":"nel"}"#,
    },
    PureCase {
        id: "module-capture-property-mutation",
        source: r"
            let state = {count: 0}
            fn reduce(input) {
              state.count = input.count
              return {count: state.count}
            }
        ",
        entry: "reduce",
        input_json: r#"{"count":9}"#,
        expected_json: r#"{"count":9}"#,
    },
    PureCase {
        id: "iteration-and-copy-on-write-mutation",
        source: r#"
            fn reduce(input) {
              let state = input.state
              for value in input.values {
                state.count = state.count + value
              }
              state.tags[0] = "updated"
              return state
            }
        "#,
        entry: "reduce",
        input_json: r#"{"state":{"count":1,"tags":["old"]},"values":[2,3,4]}"#,
        expected_json: r#"{"count":10,"tags":["updated"]}"#,
    },
    PureCase {
        id: "renderer-string-and-option-primitives",
        source: r#"
            fn reduce(input) {
              const options = {allow_network: false}.merging(input.validation ?? {})
              const name = trim(input.name)
              return {
                encoded: replace(json_stringify(name), "<", "\\u003c"),
                portable: starts_with(name, "Portable"),
                options: options,
              }
            }
        "#,
        entry: "reduce",
        input_json: r#"{"name":"  Portable <Harn>  ","validation":{"allow_host_bridge":true}}"#,
        expected_json: r#"{"encoded":"\"Portable \\u003cHarn>\"","options":{"allow_host_bridge":true,"allow_network":false},"portable":true}"#,
    },
    PureCase {
        id: "artifact-regex-hash-and-secret-safety-primitives",
        source: r#"
            fn inspect(input: string) {
              const captures = regex_captures("(?is)<body\\b([^>]*)>(.*?)</body>", input)
              return {
                body: captures[0].groups[1],
                scripts: regex_match("(?is)<script\\b", input),
                text: trim(regex_replace("(?is)<[^>]+>", " ", input)),
                digest: sha256("abc"),
                clean: len(secret_scan(input)) == 0,
              }
            }
        "#,
        entry: "inspect",
        input_json: r#""<body class='app'>Portable <b>Harn</b></body>""#,
        expected_json: r#"{"body":"Portable <b>Harn</b>","clean":true,"digest":"ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad","scripts":null,"text":"Portable  Harn"}"#,
    },
    PureCase {
        id: "target-independent-path-joining",
        source: r#"
            fn paths(input) {
              return {
                host: path_join(input.root, ".harn", "state.json"),
                reset: path_join("ignored", "/absolute", "file"),
              }
            }
        "#,
        entry: "paths",
        input_json: r#"{"root":"C:\\workspace"}"#,
        expected_json: r#"{"host":"C:/workspace/.harn/state.json","reset":"/absolute/file"}"#,
    },
    PureCase {
        id: "result-enum-match-and-propagation",
        source: r#"
            fn divide(value: int, divisor: int) -> Result<int, string> {
              if divisor == 0 { return Result.Err("division by zero") }
              return Result.Ok(value / divisor)
            }
            fn halve(value: int, divisor: int) -> Result<int, string> {
              const divided: int = divide(value, divisor)?
              return Result.Ok(divided / 2)
            }
            fn reduce(input: int) {
              const result = halve(12, input)
              match result {
                Result.Ok(value) -> { return [result.variant, result.fields, value] }
                Result.Err(message) -> { return [result.variant, result.fields, message] }
              }
            }
        "#,
        entry: "reduce",
        input_json: "3",
        expected_json: r#"["Ok",[2],2]"#,
    },
    PureCase {
        // Widening: a whole int satisfies a float parameter. This is the
        // half of harn#6267 that payload schemas used to get wrong.
        id: "float-param-accepts-int",
        source: r"
            fn takes_float(x: float) -> float { return x + 0.0 }
            fn reduce(input: int) -> float { return takes_float(input) }
        ",
        entry: "reduce",
        input_json: "3",
        expected_json: "3.0",
    },
];

/// Runtime failures that every portable executor must agree on.
///
/// Unlike [`PURE_CASES`], these are expected to fail after a successful
/// compile. The int←float case is the drift that used to slip past
/// `browser_worker_matches_native_portable_corpus_exactly` (harn#6267).
pub struct RuntimeFailureCase {
    pub id: &'static str,
    pub source: &'static str,
    pub entry: &'static str,
    pub input_json: &'static str,
    pub expected_code: &'static str,
}

pub const RUNTIME_FAILURE_CASES: &[RuntimeFailureCase] = &[
    RuntimeFailureCase {
        id: "int-param-rejects-float",
        source: r"
            fn takes_int(n: int) -> int { return n }
            fn reduce(input) { return takes_int(input) }
        ",
        entry: "reduce",
        input_json: "2.5",
        expected_code: "argument_type",
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
        source: r"
            fn reduce(input) {
              var value = input
              value = value + 1
              return value
            }
        ",
        entry: "reduce",
        expected_code: "compile_frontend",
    },
];
