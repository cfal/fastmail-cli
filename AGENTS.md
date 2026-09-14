# Working In This Repository

This is a Rust 2024 crate providing the `fastmail` CLI and a public library for
JMAP, CardDAV, GraphQL, and MCP. The GraphiQL browser assets are built separately
and checked in so ordinary Rust builds do not require Node.js.

Start with [README.md](README.md) for usage, [SECURITY.md](SECURITY.md) for security
contracts, and [tests/README.md](tests/README.md) for test organization. Toolchain,
runner, and Node versions are pinned in [.github/workflows/ci.yml](.github/workflows/ci.yml)
and [Dockerfile](Dockerfile); use those rather than guessing current versions.

## Working Rules

- Inspect the current branch and worktree before editing. Preserve unrelated
  changes; do not reset, force-push, or rewrite published history.
- Keep changes scoped. Prefer existing helpers, structured parsers, explicit
  control flow, and descriptive names over new abstractions or dense expressions.
- Preserve public Rust APIs, CLI flags/help, JSON fields, error variants/messages,
  and exit behavior unless changing that contract is part of the request.
- Use `apply_patch` for manual edits and `rg` for local searches. Add comments
  only when they explain a non-obvious constraint or decision.
- Build with one job in resource-constrained environments. Put scratch checkouts
  and large artifacts under `$HOME`, not `/tmp`. Use `rm`, not `gio`.
- Do not commit design documents unless explicitly asked. Pushing, tagging, and
  publishing require explicit authorization; preparing a change is not permission
  to release it.
- Do not use tools named exactly `Agent` or `Task`, or built-in, MCP, or generic
  CLI delegation. Autonomous delegation uses only Garcon-Amp through its skill
  and generated launchers. If unavailable, work directly. Another mechanism
  requires the user to name it explicitly in the current request.

## Code Map

- `src/main.rs`: Clap definitions, command dispatch, remote-client scope, exit status.
- `src/commands/`: CLI operations and their output envelopes.
- `src/jmap/`: session discovery, requests, composition, SSE parsing, arrival reconciliation.
- `src/carddav/`: address-book discovery, DAV/vCard parsing, conditional contact writes.
- `src/config.rs`: configuration and credential persistence.
- `src/remote.rs`: the CLI's `--server` transport; no local-credential fallback.
- `src/models/`, `src/error.rs`, `src/util.rs`: wire models, errors, HTTP clients,
  bounded reads, filenames, image handling, and extraction.
- `src/mcp/`: HTTP surfaces, security middleware, MCP tools, and SDL slicing.
- `src/mcp/graphql/`: schema, resolvers, connections, filters, loaders, confirmations,
  and subscriptions. MCP's `graphql` tool executes this same schema.
- `web/`, `templates/graphiql.html`: browser source and the embedded IDE template.
- `.github/scripts/`, `.github/workflows/ci.yml`: release policy and verification gates.

## Validation

Run focused tests while iterating, then the relevant full checks before handing
off code changes. No live credentials are needed for the automated suite.

```sh
export CARGO_BUILD_JOBS=1
export CARGO_PROFILE_DEV_DEBUG=0
export CARGO_PROFILE_TEST_DEBUG=0
cargo fmt --check
cargo test --locked
cargo clippy --all-targets --locked -- -D warnings
RUSTDOCFLAGS='-D warnings' cargo doc --locked --no-deps
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s .github/scripts -p 'test_*.py'
bash .github/scripts/check-lru-cache.sh
git diff --check
```

Use `cargo test --locked <test-name-filter>` for a focused Rust run. Run
`actionlint` when changing workflows. CI tools must be explicitly available:
hosted runners do not necessarily include local conveniences such as ripgrep.
Report which checks actually ran; do not reuse results from a different source
revision without checking whether the affected code changed.

## Behavioral Contracts

- CLI output uses `models::Output`. Keep watch output newline-delimited and
  flushed; do not mix progress text into machine-readable output. Partial image
  downloads intentionally report `success: false` while retaining successful
  files and returning exit status zero.
- Clients and their clones belong to one Tokio runtime. Preserve the bounded,
  runtime-aware transport cache in `util::http_client`; a process-global shared
  connection pool can strand requests when another runtime shuts down. Do not
  retry possibly-delivered writes to mask transport failures.
- HTTP Basic login is separate from Fastmail credentials. HTTP callers share the
  server-configured account, not separate tenants. Authentication remains optional;
  do not silently change that policy or restore client-supplied Fastmail headers.
  Authorization belongs on requests, not shared-client default headers.
- Preserve URL origin checks, redirect restrictions, auth/Host/Origin ordering,
  and bounded streaming reads. Keep the GraphQL guard before recursive upstream
  validation. Do not replace limits with `Content-Length` checks or unbounded reads.
- Preserve expiring, one-shot confirmation tokens and their account/operation/
  payload binding. Keep previews consistent with the actual operation.
- CardDAV updates preserve unknown/grouped vCard properties. Updates and deletes
  require strong ETags and conditional requests. API tokens do not substitute for
  CardDAV app passwords.
- Arrival reconciliation must not advance past failed detail fetches. Browser
  subscriptions are non-resumable and must not silently reconnect across a gap.
- Image extraction, raster resizing, and MIME inference intentionally support
  different formats. Do not merge their predicates. Resizing must never fall back
  to writing an oversized original.
- Config lives at `$HOME/.config/fastmail-cli/config.toml`; `XDG_CONFIG_HOME` does
  not select its location. Preserve private permissions and symlink protections.

## Tests And Fixtures

- Keep protocol tests in `src/jmap/tests.rs` and `src/carddav/tests.rs`, HTTP/MCP
  router tests in `src/mcp/tests.rs`, and GraphQL tests in their domain modules
  under `src/mcp/graphql/tests/`. Small pure helpers can retain inline tests.
- Use real parsers, schemas, local mock servers, and domain-specific fixtures.
  Assert exact results, request shapes/counts, and the absence of unwanted side
  effects. Similar assertions for different protocol branches are not duplicates.
- Configuration/environment tests run in isolated child processes. Do not mutate
  the parallel Rust test runner's environment. Keep waits and local servers bounded.
- The HTTP cache reuse test also needs process isolation. Its bodyless response
  and single-worker scheduling are deliberate: consuming a response body does
  not prove Hyper has returned a connection to the pool. Do not hide races with
  sleeps, retries, or whole-suite serialization.
- For bug fixes, demonstrate the regression fails before the fix when feasible.
  Credentialed tests, crash/OOM probes, and load tests are not routine validation.

## Browser Assets

Edit `web/` source or the Askama template, not generated `web/dist` files. For
browser changes, run from the repository root:

```sh
npm --prefix web ci --ignore-scripts
npm --prefix web test
npm --prefix web audit
npm --prefix web run build
(cd web/dist && sha256sum -c SHA256SUMS)
```

Include the regenerated `web/dist` assets, Rust asset index, hashes, and license
notices with the source change. A repeated build must produce no further diff.
Node tests do not replace real-browser checks for rendering, workers, HTTP/SSE
routing, and cancellation. Route using the submitted query, not stale editor
analysis; keep credentials request-specific and out of browser storage.

For VM previews, bind an unused port on `0.0.0.0`; the IDE is served by
`fastmail mcp --http 0.0.0.0:8080 --graphiql`, not a separate Node dev server.
HTTP mode reads local credentials. Use isolated configuration and fixtures unless
live access is authorized; do not expose a live account without approved access
controls. Stop temporary servers when validation is complete.

## Credentials And Releases

- Available secrets are not authorization to use them. For an authorized live
  smoke test, remove trailing token newlines, avoid command-line arguments and
  logs, isolate local configuration, and prefer bounded read-only operations.
  Do not persist credentials or print private mail just to demonstrate success.
- Dependency exceptions are specific and time-limited in `osv-scanner.toml` and
  explained in `SECURITY.md`. Do not broaden or extend them to make CI pass.
  Reassess the `LruCache` policy before changing caches or source layout.
- When a version bump is requested, keep `Cargo.toml`, the root package entry in
  `Cargo.lock`, `CHANGELOG.md`, and `RELEASE_NOTES.md` consistent. Check published
  releases, not only Git tags, when selecting the next version.
- Confirm the remote target before pushing; this fork is `cfal/fastmail-cli`.
  Publication uses the explicit `publish=true` dispatch of `ci.yml` on `main`,
  described in README. Never bypass its checks, stale-revision guard, or tag guard.
- A release is complete only after all four platform archives, their checksums,
  and Linux amd64/arm64 container manifests are published and verified. Preserve
  pinned build inputs and the Docker build/runtime glibc compatibility constraint.
