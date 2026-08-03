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
