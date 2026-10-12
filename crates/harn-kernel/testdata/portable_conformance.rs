pub struct PureCase {
    pub id: &'static str,
    pub source: &'static str,
    pub entry: &'static str,
    pub input_json: &'static str,
    pub expected_json: &'static str,
}

pub const PURE_CASES: &[PureCase] = &[
    PureCase {
        id: "truthiness-empty-bytes",
        source: r"
            fn truth(input: bytes) {
              if input { return [!input, input ? true : false] }
              return [!input, false]
            }
        ",
        entry: "truth",
        input_json: r#"{"$bytes":[]}"#,
        expected_json: "[true,false]",
    },
    PureCase {
        id: "truthiness-nonempty-bytes-with-zero-byte",
        source: r"
            fn truth(input: bytes) {
              if input { return [!input, input ? true : false] }
              return [!input, false]
            }
        ",
        entry: "truth",
        input_json: r#"{"$bytes":[0]}"#,
        expected_json: "[false,true]",
    },
    PureCase {
        id: "truthiness-nan-folding-and-runtime-branches",
        source: r"
            fn truth(input: float) {
              let nan = input / input
              let branch = false
              if nan { branch = true }
              return [!(0.0 / 0.0), !nan, (0.0 / 0.0) ? true : false,
                nan ? true : false, (0.0 / 0.0) && true, nan && true,
                (0.0 / 0.0) || false, nan || false, branch, !input]
            }
        ",
        entry: "truth",
        input_json: "0.0",
        expected_json: "[false,false,true,true,true,true,true,true,true,true]",
    },
    PureCase {
        id: "truthiness-duration-and-empty-values",
        source: r"
            fn truth(input: {zero: int, one: int, text: string, items: list<int>, record: dict<string, int>}) {
              return [!0ms, !1ms, !input.zero, !input.one, !input.text, !input.items, !input.record,
                !nil, !false, !true, ![], !({}), !0.0, !1.0]
            }
        ",
        entry: "truth",
        input_json: r#"{"zero":0,"one":1,"text":"","items":[],"record":{}}"#,
        expected_json:
            "[true,false,true,false,true,true,true,true,true,false,true,true,true,false]",
    },
    PureCase {
        id: "runtime-string-repetition",
        source: r#"
            fn repeat(input: {text: string, count: int}) {
              return [input.text * input.count, input.count * input.text, input.text * 0, input.text * -1, "" * input.count]
            }
        "#,
        entry: "repeat",
        input_json: r#"{"text":"λx","count":3}"#,
        expected_json: r#"["λxλxλx","λxλxλx","","",""]"#,
    },
    PureCase {
        id: "runtime-empty-string-repetition-with-huge-count",
        source: r"fn repeat(input: {text: string, count: int}) { return input.text * input.count }",
        entry: "repeat",
        input_json: r#"{"text":"","count":9223372036854775807}"#,
        expected_json: r#""""#,
    },
    PureCase {
        id: "runtime-integer-exponent-promotes-beyond-u32",
        source: r"fn power(input: int) { return [1 ** input, (-1) ** input, 2 ** -input] }",
        entry: "power",
        input_json: "4294967296",
        expected_json: "[1.0,1.0,0.0]",
    },
    PureCase {
        id: "runtime-float-integer-exponent-matches-folded-result",
        source: r"
            fn power(input: {base: float, exponent: int}) {
              return [input.base ** input.exponent == 1.1 ** 7, input.base ** -input.exponent == 1.1 ** -7]
            }
        ",
        entry: "power",
        input_json: r#"{"base":1.1,"exponent":7}"#,
        expected_json: "[true,true]",
    },
    PureCase {
        id: "runtime-numeric-modulo-with-nonzero-divisors",
        source: r"
            fn modulo(input: {integer: int, floating: float}) {
              return [input.integer % 3, input.integer % 2.5, input.floating % 3, input.floating % 2.5]
            }
        ",
        entry: "modulo",
        input_json: r#"{"integer":7,"floating":7.5}"#,
        expected_json: "[1,2.0,1.5,0.0]",
    },
    PureCase {
        id: "typed-record-pick-preserves-nil-and-copies-values",
        source: r#"
            type Input = {name: string, age: int, absent: nil, nested: {count: int}}
            fn project(input: Input) -> {name: string, absent: nil, original: int, copied: int} {
              let selected = pick(input, ["name", "absent", "nested", "name"])
              selected.nested.count = 99
              return {name: selected.name, absent: selected.absent, original: input.nested.count, copied: selected.nested.count}
            }
        "#,
        entry: "project",
        input_json: r#"{"name":"Ada","age":37,"absent":null,"nested":{"count":1}}"#,
        expected_json: r#"{"name":"Ada","absent":null,"original":1,"copied":99}"#,
    },
    PureCase {
        id: "typed-record-pick-runtime-keys-and-missing-values",
        source: r"
            fn project(input: {data: dict<string, int>, keys: list<string>}) -> dict<string, int> {
              return pick(input.data, input.keys)
            }
        ",
        entry: "project",
        input_json: r#"{"data":{"a":1,"b":2},"keys":["b","missing","b"]}"#,
        expected_json: r#"{"b":2}"#,
    },
    PureCase {
        id: "typed-record-pick-empty",
        source: r"fn project(input: {name: string}) -> {} { return pick(input, []) }",
        entry: "project",
        input_json: r#"{"name":"Ada"}"#,
        expected_json: "{}",
    },
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
            type TransformInput = {value: int, offset: int, enabled: bool}
            type TransformResult = {value: int, enabled: bool}
            fn transform(input: TransformInput) -> TransformResult {
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
            type SummaryInput = {
              left: list<int>,
              right: list<int>,
              prefix: string,
              name: string,
              needle: int,
              meta: dict<string, int>,
            }
            fn summarize(input: SummaryInput) {
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
            type ListComparisonInput = {left: list<int>, right: list<int>}
            fn compare_lists(input: ListComparisonInput) {
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
            fn compare(input: {value: string}) {
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
            fn validate(input: {value: int}) {
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
            type ChoiceInput = {values: list<int>, text: string}
            fn choose(input: ChoiceInput) {
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
            fn reduce(input: {count: int}) {
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
            type IterationState = {count: int, tags: list<string>}
            type IterationInput = {state: IterationState, values: list<int>}
            fn reduce(input: IterationInput) -> IterationState {
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
            type RenderInput = {name: string, validation?: dict<string, bool>}
            fn reduce(input: RenderInput) {
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
            fn paths(input: {root: string}) {
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
    PureCase {
        // harn#9189: cleanup runs once for errors raised by a callee, not
        // only for an inline `throw`. Runtime failures such as division by
        // zero are terminal in the portable kernel, so only throws appear.
        id: "cleanup-runs-once-for-callee-errors",
        source: r#"
            fn fail(message: string) -> int { throw message }
            fn relay(message: string) -> int { return fail(message) + 1 }
            fn reduce(input: list<string>) -> {finally_runs: int, defer_runs: int, caught: list<string>} {
              let finally_runs = 0
              let defer_runs = 0
              let caught = []
              for label in input {
                try {
                  defer { defer_runs = defer_runs + 1 }
                  try {
                    if label == "direct" {
                      fail(label)
                    } else {
                      relay(label)
                    }
                  } finally {
                    finally_runs = finally_runs + 1
                  }
                } catch (error) {
                  caught = caught + [error]
                }
              }
              return {finally_runs: finally_runs, defer_runs: defer_runs, caught: caught}
            }
        "#,
        entry: "reduce",
        input_json: r#"["direct","relayed"]"#,
        expected_json: r#"{"finally_runs":2,"defer_runs":2,"caught":["direct","relayed"]}"#,
    },
    PureCase {
        // harn#9201: `?` returns an `Err` through pending cleanup. The
        // `finally` overrides the early return, so the result only reads
        // `cleanup` when that cleanup ran.
        id: "try-operator-runs-pending-finally",
        source: r#"
            fn check(value: int) -> Result<int, string> {
              if value < 0 { return Result.Err("negative") }
              return Result.Ok(value)
            }
            fn guarded(value: int) -> Result<int, string> {
              try {
                const checked = check(value)?
                return Result.Ok(checked + 1)
              } finally {
                if value < 0 { return Result.Err("cleanup") }
              }
              return Result.Err("fell through")
            }
            fn reduce(input: list<int>) -> list<list<any>> {
              let rendered = []
              for value in input {
                const result = guarded(value)
                rendered = rendered + [[result.variant, result.fields]]
              }
              return rendered
            }
        "#,
        entry: "reduce",
        input_json: "[-1,4]",
        expected_json: r#"[["Err",["cleanup"]],["Ok",[5]]]"#,
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
        id: "string-repetition-rejects-output-over-byte-limit",
        source: r"fn repeat(input: {text: string, count: int}) { return input.text * input.count }",
        entry: "repeat",
        input_json: r#"{"text":"ab","count":524289}"#,
        expected_code: "value_byte_limit",
    },
    RuntimeFailureCase {
        id: "string-repetition-rejects-overflowing-output-length",
        source: r"fn repeat(input: {text: string, count: int}) { return input.count * input.text }",
        entry: "repeat",
        input_json: r#"{"text":"abc","count":9223372036854775807}"#,
        expected_code: "value_byte_limit",
    },
    RuntimeFailureCase {
        id: "integer-modulo-by-integer-zero",
        source: r"fn modulo(input: {left: int, right: int}) { return input.left % input.right }",
        entry: "modulo",
        input_json: r#"{"left":7,"right":0}"#,
        expected_code: "division_by_zero",
    },
    RuntimeFailureCase {
        id: "float-modulo-by-integer-zero",
        source: r"fn modulo(input: {left: float, right: int}) { return input.left % input.right }",
        entry: "modulo",
        input_json: r#"{"left":7.5,"right":0}"#,
        expected_code: "division_by_zero",
    },
    RuntimeFailureCase {
        id: "integer-modulo-by-float-zero",
        source: r"fn modulo(input: {left: int, right: float}) { return input.left % input.right }",
        entry: "modulo",
        input_json: r#"{"left":7,"right":0.0}"#,
        expected_code: "division_by_zero",
    },
    RuntimeFailureCase {
        id: "float-modulo-by-negative-float-zero",
        source: r"fn modulo(input: {left: float, right: float}) { return input.left % input.right }",
        entry: "modulo",
        input_json: r#"{"left":7.5,"right":-0.0}"#,
        expected_code: "division_by_zero",
    },
    RuntimeFailureCase {
        id: "declared-error-survives-transparent-rethrows",
        source: r"
            fn fail(input: int) -> any throws int { throw input }
            fn reduce(input: int) -> any throws int {
                try { return retry 2 { try* fail(input) } }
                finally { try { throw 8 } catch (_ignored) {} }
            }
        ",
        entry: "reduce",
        input_json: "7",
        expected_code: "harn_declared_throw",
    },
    RuntimeFailureCase {
        id: "legacy-error-stays-runtime-under-declared-wrapper",
        source: r"
            fn fail(input: int) -> any { throw input }
            fn reduce(input: int) -> any throws int {
                try { return retry 2 { try* fail(input) } }
                finally { try { throw 8 } catch (_ignored) {} }
            }
        ",
        entry: "reduce",
        input_json: "7",
        expected_code: "harn_throw",
    },
    RuntimeFailureCase {
        id: "pick-rejects-non-record-source",
        source: r#"fn project(input: any) { return pick(input, ["name"]) }"#,
        entry: "project",
        input_json: "42",
        expected_code: "builtin_type",
    },
    RuntimeFailureCase {
        id: "pick-rejects-non-string-key",
        source: r"fn project(input: {name: string, keys: list<any>}) { return pick(input, input.keys) }",
        entry: "project",
        input_json: r#"{"name":"Ada","keys":[1]}"#,
        expected_code: "builtin_type",
    },
    RuntimeFailureCase {
        id: "pick-rejects-wrong-arity",
        source: r"fn project(args: list<any>) { return pick(...args) }",
        entry: "project",
        input_json: r#"[{"name":"Ada"}]"#,
        expected_code: "builtin_type",
    },
    RuntimeFailureCase {
        id: "int-param-rejects-float",
        source: r"
            fn takes_int(n: int) -> int { return n }
            // Deliberate unchecked host boundary: the failure belongs to the
            // portable runtime parameter contract.
            fn reduce(input: any) { return takes_int(input) }
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
        id: "pick-unknown-field",
        source: r#"fn project(input: {name: string}) { return pick(input, ["nmae"]) }"#,
        entry: "project",
        expected_code: "compile_frontend",
    },
    InvalidCase {
        id: "pick-dynamic-fields-are-not-required",
        source: r"fn project(input: {name: string}, keys: list<string>) -> {name: string} { return pick(input, keys) }",
        entry: "project",
        expected_code: "compile_frontend",
    },
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

#[derive(Clone, Copy)]
pub struct PackageCompilationCase {
    pub id: &'static str,
    pub source: &'static str,
    pub entry: &'static str,
    pub expected_ok: bool,
}

impl PackageCompilationCase {
    pub fn package(&self) -> harn_kernel::PortableSourcePackage {
        harn_kernel::PortableSourcePackage {
            root_source: self.source.to_string(),
            root_imports: Vec::new(),
            modules: Vec::new(),
        }
    }

    pub fn compile(&self) -> Result<harn_kernel::ProgramArtifact, Vec<harn_kernel::Diagnostic>> {
        let result = harn_kernel::compile_source_package(
            self.package(),
            self.entry,
            harn_kernel::EntryKind::Function,
        );
        assert_eq!(
            result.is_ok(),
            self.expected_ok,
            "{} compilation decision drifted",
            self.id
        );
        if let Err(diagnostics) = &result {
            assert!(
                !diagnostics.is_empty(),
                "{} has no failure diagnostic",
                self.id
            );
        }
        result
    }
}

pub fn package_compilation_cases() -> impl Iterator<Item = PackageCompilationCase> {
    INVALID_CASES
        .iter()
        .map(|case| PackageCompilationCase {
            id: case.id,
            source: case.source,
            entry: case.entry,
            expected_ok: false,
        })
        .chain(PACKAGE_COMPILATION_CASES.iter().copied())
}

pub const PACKAGE_COMPILATION_CASES: &[PackageCompilationCase] = &[
    PackageCompilationCase {
        id: "removed-provider-wrapper-alias",
        source: "fn reduce(input: string) { return with_rate_limit(input) }",
        entry: "reduce",
        expected_ok: false,
    },
    PackageCompilationCase {
        id: "retained-provider-call-alias",
        source: "fn reduce(input: string) { return llm_call(input) }",
        entry: "reduce",
        expected_ok: true,
    },
    PackageCompilationCase {
        id: "retained-provider-call-method",
        source: "fn reduce(harness: Harness, input: string) { return harness.llm.call(input) }",
        entry: "reduce",
        expected_ok: true,
    },
];
