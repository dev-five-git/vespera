# testkit

A tiny in-process HTTP client for the examples' integration tests. `TestServer`
wraps an `axum::Router` (via the `vespera::axum` re-export) and drives it with
`tower::ServiceExt::oneshot` — no socket, no port, and no test-server crate
beyond what axum already depends on. It covers exactly the request/response
surface the examples' tests use (JSON, raw and multipart bodies, headers,
status and body assertions); add it as a `[dev-dependencies]` path dependency
of an example. It is unpublished (`publish = false`) and not a general-purpose
testing library.
