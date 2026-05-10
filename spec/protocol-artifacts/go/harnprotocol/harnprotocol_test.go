// Round-trip test for the Harn protocol bindings.
//
// Loads the published JSON fixture, decodes each envelope into the
// corresponding generated struct, re-encodes via encoding/json, and asserts
// structural parity (key-by-key) with the original. This catches drift between
// the Rust adapter wire vocabulary and the Go bindings before downstream
// consumers see it.
package harnprotocol

import (
	"bytes"
	"encoding/json"
	"os"
	"path/filepath"
	"reflect"
	"sort"
	"testing"
)

func loadFixture(t *testing.T) map[string]json.RawMessage {
	t.Helper()
	path := filepath.Join("..", "..", "fixtures", "round_trip.json")
	bytes, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("read fixture: %v", err)
	}
	var fixture map[string]json.RawMessage
	if err := json.Unmarshal(bytes, &fixture); err != nil {
		t.Fatalf("decode fixture: %v", err)
	}
	return fixture
}

// canonicalize returns a stable, key-sorted JSON form so two encodings of the
// same structure compare equal regardless of key ordering or numeric
// formatting differences in the unmarshal/marshal cycle.
func canonicalize(t *testing.T, raw []byte) []byte {
	t.Helper()
	var value any
	decoder := json.NewDecoder(bytes.NewReader(raw))
	decoder.UseNumber()
	if err := decoder.Decode(&value); err != nil {
		t.Fatalf("decode for canonicalize: %v", err)
	}
	return canonicalizeValue(t, value)
}

func canonicalizeValue(t *testing.T, value any) []byte {
	t.Helper()
	out, err := canonicalEncode(value)
	if err != nil {
		t.Fatalf("canonicalize: %v", err)
	}
	return out
}

func canonicalEncode(value any) ([]byte, error) {
	switch v := value.(type) {
	case map[string]any:
		keys := make([]string, 0, len(v))
		for key := range v {
			keys = append(keys, key)
		}
		sort.Strings(keys)
		var buf bytes.Buffer
		buf.WriteByte('{')
		for index, key := range keys {
			if index > 0 {
				buf.WriteByte(',')
			}
			encoded, err := json.Marshal(key)
			if err != nil {
				return nil, err
			}
			buf.Write(encoded)
			buf.WriteByte(':')
			child, err := canonicalEncode(v[key])
			if err != nil {
				return nil, err
			}
			buf.Write(child)
		}
		buf.WriteByte('}')
		return buf.Bytes(), nil
	case []any:
		var buf bytes.Buffer
		buf.WriteByte('[')
		for index, item := range v {
			if index > 0 {
				buf.WriteByte(',')
			}
			child, err := canonicalEncode(item)
			if err != nil {
				return nil, err
			}
			buf.Write(child)
		}
		buf.WriteByte(']')
		return buf.Bytes(), nil
	default:
		return json.Marshal(v)
	}
}

func assertRoundTrip(t *testing.T, label string, original, replayed []byte) {
	t.Helper()
	left := canonicalize(t, original)
	right := canonicalize(t, replayed)
	if !reflect.DeepEqual(left, right) {
		t.Fatalf(
			"%s round-trip mismatch:\n  original: %s\n  replayed: %s",
			label, string(left), string(right),
		)
	}
}

func TestRoundTripFixture(t *testing.T) {
	fixture := loadFixture(t)

	if got := mustString(t, fixture, "artifactVersion"); got != ArtifactVersion {
		t.Fatalf(
			"artifact version drift: bindings=%q fixture=%q",
			ArtifactVersion, got,
		)
	}
	if got := mustString(t, fixture, "harnAgentEventMethod"); got != HarnAgentEventMethod {
		t.Fatalf(
			"agent-event method drift: bindings=%q fixture=%q",
			HarnAgentEventMethod, got,
		)
	}

	var envelopes map[string]json.RawMessage
	if err := json.Unmarshal(fixture["envelopes"], &envelopes); err != nil {
		t.Fatalf("decode envelopes: %v", err)
	}

	t.Run("ACPRequest", func(t *testing.T) {
		var request ACPRequest
		if err := json.Unmarshal(envelopes["request"], &request); err != nil {
			t.Fatalf("decode request: %v", err)
		}
		out, err := json.Marshal(request)
		if err != nil {
			t.Fatalf("encode request: %v", err)
		}
		assertRoundTrip(t, "ACPRequest", envelopes["request"], out)
	})

	t.Run("ACPResponse", func(t *testing.T) {
		var response ACPResponse
		if err := json.Unmarshal(envelopes["response"], &response); err != nil {
			t.Fatalf("decode response: %v", err)
		}
		out, err := json.Marshal(response)
		if err != nil {
			t.Fatalf("encode response: %v", err)
		}
		assertRoundTrip(t, "ACPResponse", envelopes["response"], out)
	})

	t.Run("ACPResponseError", func(t *testing.T) {
		var response ACPResponse
		if err := json.Unmarshal(envelopes["errorResponse"], &response); err != nil {
			t.Fatalf("decode error response: %v", err)
		}
		if response.Error == nil {
			t.Fatal("expected error sub-envelope to decode")
		}
		out, err := json.Marshal(response)
		if err != nil {
			t.Fatalf("encode error response: %v", err)
		}
		assertRoundTrip(t, "ACPResponse(error)", envelopes["errorResponse"], out)
	})

	t.Run("SessionUpdateNotification", func(t *testing.T) {
		var notification ACPSessionUpdateNotification
		if err := json.Unmarshal(envelopes["sessionUpdateNotification"], &notification); err != nil {
			t.Fatalf("decode session update: %v", err)
		}
		if notification.Params.Update.SessionUpdate != "tool_call" {
			t.Fatalf("expected sessionUpdate=tool_call, got %q", notification.Params.Update.SessionUpdate)
		}
		out, err := json.Marshal(notification)
		if err != nil {
			t.Fatalf("encode session update: %v", err)
		}
		assertRoundTrip(t, "ACPSessionUpdateNotification", envelopes["sessionUpdateNotification"], out)
	})

	t.Run("AgentEventNotification", func(t *testing.T) {
		var notification HarnAgentEventNotification
		if err := json.Unmarshal(envelopes["agentEventNotification"], &notification); err != nil {
			t.Fatalf("decode agent event: %v", err)
		}
		if notification.Method != HarnAgentEventMethod {
			t.Fatalf("expected method=%s, got %q", HarnAgentEventMethod, notification.Method)
		}
		out, err := json.Marshal(notification)
		if err != nil {
			t.Fatalf("encode agent event: %v", err)
		}
		assertRoundTrip(t, "HarnAgentEventNotification", envelopes["agentEventNotification"], out)
	})

	t.Run("A2ATask", func(t *testing.T) {
		raw, ok := fixture["a2aTask"]
		if !ok {
			t.Fatal("fixture missing a2aTask")
		}
		var task A2ATask
		if err := json.Unmarshal(raw, &task); err != nil {
			t.Fatalf("decode a2a task: %v", err)
		}
		out, err := json.Marshal(task)
		if err != nil {
			t.Fatalf("encode a2a task: %v", err)
		}
		assertRoundTrip(t, "A2ATask", raw, out)
	})

	t.Run("MCPTool", func(t *testing.T) {
		raw, ok := fixture["mcpTool"]
		if !ok {
			t.Fatal("fixture missing mcpTool")
		}
		var tool MCPTool
		if err := json.Unmarshal(raw, &tool); err != nil {
			t.Fatalf("decode mcp tool: %v", err)
		}
		out, err := json.Marshal(tool)
		if err != nil {
			t.Fatalf("encode mcp tool: %v", err)
		}
		assertRoundTrip(t, "MCPTool", raw, out)
	})
}

func mustString(t *testing.T, fixture map[string]json.RawMessage, key string) string {
	t.Helper()
	raw, ok := fixture[key]
	if !ok {
		t.Fatalf("fixture missing key %q", key)
	}
	var value string
	if err := json.Unmarshal(raw, &value); err != nil {
		t.Fatalf("decode %s: %v", key, err)
	}
	return value
}

func TestEnvelopeKindHelpers(t *testing.T) {
	cases := []struct {
		name           string
		raw            string
		isRequest      bool
		isResponse     bool
		isNotification bool
	}{
		{"request", `{"jsonrpc":"2.0","id":1,"method":"foo"}`, true, false, false},
		{"response", `{"jsonrpc":"2.0","id":1,"result":{}}`, false, true, false},
		{"notification", `{"jsonrpc":"2.0","method":"foo"}`, false, false, true},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			var envelope map[string]json.RawMessage
			if err := json.Unmarshal([]byte(tc.raw), &envelope); err != nil {
				t.Fatalf("decode: %v", err)
			}
			if got := IsRequest(envelope); got != tc.isRequest {
				t.Errorf("IsRequest=%v want %v", got, tc.isRequest)
			}
			if got := IsResponse(envelope); got != tc.isResponse {
				t.Errorf("IsResponse=%v want %v", got, tc.isResponse)
			}
			if got := IsNotification(envelope); got != tc.isNotification {
				t.Errorf("IsNotification=%v want %v", got, tc.isNotification)
			}
		})
	}
}

func TestJSONRPCIDEncoding(t *testing.T) {
	cases := []struct {
		name string
		id   JSONRPCID
		want string
	}{
		{"int", NewJSONRPCIDInt(42), `42`},
		{"string", NewJSONRPCIDString("abc"), `"abc"`},
		{"null", NullJSONRPCID(), `null`},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			out, err := json.Marshal(tc.id)
			if err != nil {
				t.Fatalf("marshal: %v", err)
			}
			if got := string(out); got != tc.want {
				t.Errorf("marshal=%s want %s", got, tc.want)
			}
			var roundtrip JSONRPCID
			if err := json.Unmarshal([]byte(tc.want), &roundtrip); err != nil {
				t.Fatalf("unmarshal: %v", err)
			}
			out2, err := json.Marshal(roundtrip)
			if err != nil {
				t.Fatalf("re-marshal: %v", err)
			}
			if string(out2) != tc.want {
				t.Errorf("round-trip=%s want %s", string(out2), tc.want)
			}
		})
	}
}

// Regression guard: ensure typed string aliases are populated and stay
// non-empty. A drop to zero would mean the generator silently emitted an empty
// vocabulary for a downstream binding consumer.
func TestVocabulariesArePopulated(t *testing.T) {
	checks := []struct {
		name   string
		values []string
	}{
		{"ACPAgentMethods", ACPAgentMethods},
		{"ACPClientMethods", ACPClientMethods},
		{"ACPSessionUpdates", ACPSessionUpdates},
		{"ACPToolKinds", ACPToolKinds},
		{"ACPToolCallStatuses", ACPToolCallStatuses},
		{"HarnToolCallErrorCategories", HarnToolCallErrorCategories},
		{"HarnSideEffectLevels", HarnSideEffectLevels},
		{"A2ATaskStates", A2ATaskStates},
		{"A2ATaskEventTypes", A2ATaskEventTypes},
		{"MCPMethods", MCPMethods},
	}
	for _, check := range checks {
		if len(check.values) == 0 {
			t.Errorf("%s is empty", check.name)
		}
	}
}
