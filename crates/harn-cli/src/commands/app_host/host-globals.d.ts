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

declare const HarnAppHostProtocol: {
  proxyServerRequest(
    message: JsonRpcMessage,
    proxy: (message: JsonRpcMessage) => Promise<JsonRpcMessage>,
    reply: (message: JsonRpcMessage) => void,
  ): Promise<void>;
};
