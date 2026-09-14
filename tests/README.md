# Tests

Run the local test suite from the repository root:

```sh
CARGO_BUILD_JOBS=1 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test --locked
CARGO_BUILD_JOBS=1 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo clippy --all-targets --locked -- -D warnings
CARGO_BUILD_JOBS=1 CARGO_PROFILE_DEV_DEBUG=0 RUSTDOCFLAGS='-D warnings' cargo doc --locked --no-deps
cargo fmt --check
npm --prefix web ci
npm --prefix web test
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s .github/scripts -p 'test_*.py'
```

## Organization

- `src/carddav/tests.rs`: DAV parsing, property-preserving writes, conditional mutations, URL and credential isolation.
- `src/jmap/tests.rs`: request shapes, capability and object limits, composition, exact/domain sender selection, recipients, uploads, and HTTP error classification.
- `src/jmap/events.rs` and `src/jmap/watch.rs`: streaming parser boundaries, cursor retention, resync, reconnects, filtering, and fatal errors.
- `src/mcp/tests.rs`: complete HTTP router contracts, MCP tools, transport routing, and server-owned credentials.
- `src/mcp/http_security.rs`: Basic auth, browser policy, and exact-bound request preservation.
- `src/remote.rs`: server URL validation, Basic credential decoding/redaction, and proxy error classification.
- `src/mcp/graphql/tests/`: schema, connections, resolution and batching, attachments, mutations, sessions, and subscriptions. Shared JMAP fixtures remain in `src/mcp/graphql/tests.rs`.
- Small pure helpers retain tests beside their implementations, including GraphQL input limits, filters, SDL slicing, image bounds, and model serialization.
- `tests/config.rs`: real configuration getters and persistence in child processes with isolated homes and environment variables.
- `tests/http_cli.rs`: executable-level output, exit status, remote transport, domain senders and names across compose operations, downloads, and destructive-command confirmation.
- `src/main.rs`: Clap argument-definition consistency and literal help text.
- `tests/extraction.rs`: generated document fixtures for supported extraction formats.
- `web/fetcher.test.js`: GraphiQL HTTP/SSE routing, cancellation, per-request headers, and failure propagation.
- `.github/scripts/test_release_version.py`: release ordering, paginated GitHub results, workflow outputs, and fail-closed lookup behavior. GitHub calls are mocked.
- `.github/scripts/test_lru_guard.py`: the dependency-policy source guard, including forbidden references, search errors, and missing Git.

## Conventions

Assert observable contracts: exact error variants/messages, response fields, bytes on disk, request shapes/counts, and absence of unwanted network or file operations. Similar assertions against different protocols or failure branches are not duplicate coverage.

Use real parsers and schemas with local fixtures. Keep fixture helpers domain-specific; a shared helper must not hide the request shape a test is checking. Table-test equivalent setup while keeping independent failure modes separately named.

Do not mutate the Rust test runner's environment. Re-execute an exact test in an isolated child process instead. The HTTP cache regression also uses a child process because other tests can legitimately evict its single cache slot. Keep cross-runtime servers and waits bounded; do not hide races with retries or serializing the entire suite.

These tests need no live Fastmail credentials. They do not establish live CardDAV interoperability, upstream service availability, exhaustive parser correctness, or browser rendering. Browser changes additionally require a deterministic bundle rebuild and real-browser QA; mock and unit tests are not substitutes for those checks.
