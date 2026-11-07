Retained WebSocket sessions keep an ordinary prompt running across disconnect
and replay its pending host request on reconnection. Disconnect still withdraws
that client's queued delivery prompts; explicit request cancellation remains
available to the owner.
