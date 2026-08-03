/* tslint:disable */
/* eslint-disable */

/**
 * The result of compiling source with the canonical Harn frontend.
 */
export class CompileOutcome {
    private constructor();
    free(): void;
    [Symbol.dispose](): void;
    /**
     * Return an independent copy suitable for transfer to a Web Worker.
     */
    artifactBytes(): Uint8Array;
    diagnosticsJson(): string;
    readonly digest: string;
    readonly ok: boolean;
}

/**
 * A stable projection of completed, suspended, or failed execution.
 */
export class ExecutionOutcome {
    private constructor();
    free(): void;
    [Symbol.dispose](): void;
    diagnosticJson(): string;
    requestJson(): string;
    snapshotBytes(): Uint8Array;
    valueJson(): string;
    readonly status: string;
}

/**
 * Return versioned build provenance for a portable browser benchmark receipt.
 */
export function benchmarkProvenanceJson(): string;

/**
 * Return the kernel-owned receipt version instead of duplicating it in
 * browser configuration.
 */
export function benchmarkSchemaVersion(): string;

/**
 * Hash a bounded portable terminal value with the same canonical JSON and
 * BLAKE3 contract used by the native benchmark receipt.
 */
export function benchmarkTerminalDigest(value_json: string): string;

/**
 * Compile a function or pipeline through the canonical Harn frontend.
 */
export function compile(source: string, entry: string, entry_kind: string): CompileOutcome;

/**
 * Validate and canonically serialize a browser-captured benchmark receipt
 * through the same closed Rust type used by the native CLI.
 */
export function normalizeBenchmarkReceiptJson(receipt_json: string): string;

/**
 * Resume a suspended execution with the matching typed capability result.
 */
export function resume(artifact: Uint8Array, snapshot: Uint8Array, capability_result_json: string, grants_json: string): ExecutionOutcome;

/**
 * Start a fresh portable execution.
 */
export function start(artifact: Uint8Array, input_json: string, grants_json: string): ExecutionOutcome;

/**
 * Aggregate host-recorded benchmark samples with the kernel's canonical
 * R-7 percentile and population-standard-deviation contract.
 *
 * The host owns clock access. This projection accepts only a bounded JSON
 * array so exposing benchmark aggregation does not grant the kernel a clock or
 * create a second JavaScript statistics implementation.
 */
export function summarizeBenchmarkSamples(samples_json: string): string;

export type InitInput = RequestInfo | URL | Response | BufferSource | WebAssembly.Module;

export interface InitOutput {
    readonly memory: WebAssembly.Memory;
    readonly __wbg_compileoutcome_free: (a: number, b: number) => void;
    readonly __wbg_executionoutcome_free: (a: number, b: number) => void;
    readonly benchmarkProvenanceJson: () => [number, number];
    readonly benchmarkSchemaVersion: () => [number, number];
    readonly benchmarkTerminalDigest: (a: number, b: number) => [number, number, number, number];
    readonly compile: (a: number, b: number, c: number, d: number, e: number, f: number) => number;
    readonly compileoutcome_artifactBytes: (a: number) => [number, number];
    readonly compileoutcome_diagnosticsJson: (a: number) => [number, number];
    readonly compileoutcome_digest: (a: number) => [number, number];
    readonly compileoutcome_ok: (a: number) => number;
    readonly executionoutcome_diagnosticJson: (a: number) => [number, number];
    readonly executionoutcome_requestJson: (a: number) => [number, number];
    readonly executionoutcome_snapshotBytes: (a: number) => [number, number];
    readonly executionoutcome_status: (a: number) => [number, number];
    readonly executionoutcome_valueJson: (a: number) => [number, number];
    readonly normalizeBenchmarkReceiptJson: (a: number, b: number) => [number, number, number, number];
    readonly resume: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number) => [number, number, number];
    readonly start: (a: number, b: number, c: number, d: number, e: number, f: number) => [number, number, number];
    readonly summarizeBenchmarkSamples: (a: number, b: number) => [number, number, number, number];
    readonly __wbindgen_externrefs: WebAssembly.Table;
    readonly __wbindgen_free: (a: number, b: number, c: number) => void;
    readonly __wbindgen_malloc: (a: number, b: number) => number;
    readonly __wbindgen_realloc: (a: number, b: number, c: number, d: number) => number;
    readonly __externref_table_dealloc: (a: number) => void;
    readonly __wbindgen_start: () => void;
}

export type SyncInitInput = BufferSource | WebAssembly.Module;

/**
 * Instantiates the given `module`, which can either be bytes or
 * a precompiled `WebAssembly.Module`.
 *
 * @param {{ module: SyncInitInput }} module - Passing `SyncInitInput` directly is deprecated.
 *
 * @returns {InitOutput}
 */
export function initSync(module: { module: SyncInitInput } | SyncInitInput): InitOutput;

/**
 * If `module_or_path` is {RequestInfo} or {URL}, makes a request and
 * for everything else, calls `WebAssembly.instantiate` directly.
 *
 * @param {{ module_or_path: InitInput | Promise<InitInput> }} module_or_path - Passing `InitInput` directly is deprecated.
 *
 * @returns {Promise<InitOutput>}
 */
export default function __wbg_init (module_or_path?: { module_or_path: InitInput | Promise<InitInput> } | InitInput | Promise<InitInput>): Promise<InitOutput>;
