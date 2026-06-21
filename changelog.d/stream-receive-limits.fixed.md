Fixed SSE and websocket receive limits so timeout, EOF, and open-control polls
no longer consume the configured event/message budget.
