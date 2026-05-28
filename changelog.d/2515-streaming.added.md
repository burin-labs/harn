- **A.12 streaming uploads for `harn-serve` (#2515).** Two new
  Rust-level primitives let adapter handlers process large inbound
  bodies without buffering the whole payload, closing the streaming
  gap the buffered `harn_vm::stdlib::multipart::multipart_parse` left
  open. `harn_serve::MultipartStream::start(multipart, config)` walks
  `axum::extract::Multipart` field-by-field and yields each
  `MultipartField { name, filename, content_type, bytes }` with a
  bounded inner bytes channel — fields stream straight into hashers,
  disk, or forwarded requests with a per-field byte cap. Companion
  `harn_serve::RequestBodyChannel::start(body, config)` exposes
  `Body::into_data_stream()` as a `mpsc` receiver for
  `Transfer-Encoding: chunked` consumers. A new
  `crates/harn-serve/tests/streaming_conformance.rs` proves both
  primitives walk a 50 MiB payload while the peak in-flight chunk
  stays bounded to the wire-shaped size (≤4× the source chunk for
  multipart, ≤2× for raw body), so the 50 MiB allocation cliff that
  `multipart_parse` would hit on the same upload is avoided. The
  `.harn` channel bridge for `http.multipart(req)` and
  `req.body_channel()` builtins lands together with the future `.harn`
  HTTP handler host (the same hosting pivot blocking the `WsSession`
  bridge per `#1870`).
