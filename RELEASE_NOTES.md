# v4.0.1

Security and correctness fixes following two independent comprehensive source
reviews. No deliberately malicious code was identified in the reviewed scope.

## Security

- Bound GraphQL query size and syntax nesting before parsing, including input
  values, and cap fragment expansion before recursive validation. Apply the
  2 MiB JSON limit to streamed MCP request bodies.
- Require an expiring, one-shot `confirmationToken` from spam PREVIEW before
  CONFIRM. Tokens are bound to the account, email and operation.
- Stop subscription record caches from accumulating mail indefinitely. Enforce
  actual encoded image limits with bounded resize work and reject invalid CLI size values.
- Authentication remains optional, including on non-loopback listeners. Basic
  users still share the server-configured account. Reachable authenticated
  listeners should use HTTPS, strong passwords and a rate-limiting reverse proxy.

## Correctness

- Preserve unknown/grouped vCard properties, use ETags for contact mutations,
  and support stable IDs for UID-less contacts.
- Retry failed arrival reconciliation without losing the cursor. Recover from
  invalid SSE frames, preserve split UTF-8 and reject zero polling intervals.
- Respect JMAP fetch limits and refresh mailbox lookups. Keep rejected sends in
  Drafts and patch read flags without replacing unrelated keywords.
- Fix cold session health, pageInfo-only cursors, remote error messages/classification
  and attachment filename collisions. Report skipped images while continuing other
  downloads. Reuse HTTP transports without sharing credentials.
- GraphiQL subscriptions now use `/graphql/stream` with `graphql-sse`
  `next`/`complete` events. Downstream reconnects are not resumable; query for
  mail received during a disconnect. The IDE surfaces disconnects rather than
  silently starting a new subscription.
- Refresh agent references and restore the missing v4.0.0 changelog entry.

No vendoring or new authentication requirement. Existing time-limited `lru` and
`paste` dependency dispositions remain documented in `SECURITY.md`.

Four platform archives include licenses; `SHA256SUMS` covers the archives.
The CI release also publishes Linux amd64/arm64 container manifests.
