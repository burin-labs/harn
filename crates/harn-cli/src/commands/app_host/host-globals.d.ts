declare const __HARN_SANDBOX_ORIGIN__: string;
declare const __HARN_TITLE__: string;
declare const __HARN_VERSION__: string;

type JsonRpcId = string | number;

type JsonRpcMessage = {
  jsonrpc: "2.0";
  id?: JsonRpcId;
  method?: string;
  params?: Record<string, unknown>;
  result?: unknown;
  error?: { code: number; message: string };
};

type ViewInitialization =
  | { ok: true; protocolVersion: string }
  | { ok: false; code: number; message: string };

type ResourcePolicy = {
  connectDomains: string[];
  resourceDomains: string[];
  frameDomains: string[];
  baseUriDomains: string[];
};

declare const HarnAppHostProtocol: {
  appProtocolVersion: string;
  createViewConnection(): {
    initialize(params: Record<string, unknown> | undefined): ViewInitialization;
    markReady(): boolean;
    isReady(): boolean;
  };
  hasRequestId(message: unknown): message is JsonRpcMessage & { id: JsonRpcId };
  isNotification(message: unknown): boolean;
  isSandboxMessage(message: unknown): boolean;
  isServerRequestMethod(method: unknown): boolean;
  isViewNotificationMethod(method: unknown): boolean;
  proxyServerRequest(
    message: JsonRpcMessage,
    proxy: (message: JsonRpcMessage) => Promise<JsonRpcMessage>,
    reply: (message: JsonRpcMessage) => void,
  ): Promise<void>;
};

type PortableRequest = {
  id: string;
  capability: string;
  operation: string;
  arguments: unknown;
  expected: string;
};

type PortableResult =
  | { status: "ok"; request_id: string; value: unknown }
  | { status: "err"; request_id: string; code: string; message: string };

type PortableWorkerMessage =
  | {
      schema: "harn.portable_worker.v1";
      kind: "load";
      artifact: Uint8Array;
      state: unknown;
      grants: Record<string, unknown> & { capabilities: string[] };
    }
  | { schema: "harn.portable_worker.v1"; kind: "restore"; state: unknown }
  | { schema: "harn.portable_worker.v1"; kind: "event"; event: unknown }
  | {
      schema: "harn.portable_worker.v1";
      kind: "result";
      result: PortableResult;
    };

type PortableOutcome = {
  status: string;
  valueJson(): string;
  requestJson(): string;
  snapshotBytes(): Uint8Array;
  diagnosticJson(): string;
};

declare const HarnPortableRunner: {
  schema: "harn.portable_worker.v1";
  create(options: {
    start(artifact: Uint8Array, input: string, grants: string): PortableOutcome;
    resume(
      artifact: Uint8Array,
      snapshot: Uint8Array,
      result: string,
      grants: string,
    ): PortableOutcome;
    send(message: Record<string, unknown>): void;
  }): { receive(message: PortableWorkerMessage): void };
};
