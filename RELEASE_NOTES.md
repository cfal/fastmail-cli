# v4.0.5

## Caller-Managed Email Checkpoints

- `fastmail email-state` reads the account's current opaque Email state without
  fetching mail. `fastmail changes --since-state STATE` immediately follows all
  change pages and returns one complete ID-only batch, including `accountId`,
  original `oldState`, final `newState`, and `created`/`updated`/`destroyed` arrays.
  Both commands support direct access and `--server`.
- Keep durable processing under caller control: atomically enqueue IDs and save
  the candidate checkpoint, then fetch/process pending messages separately.
  Restart from the old state after an uncommitted result to replay discoverable
  changes. The CLI never persists or acknowledges a checkpoint on your behalf.
- Preserve every page's IDs, including duplicates and creations later destroyed,
  without assuming chronological order or collapsing them into net changes.
  Successful empty batches can advance the state too.

## Recovery And Limits

- Return no partial successful batch or checkpoint on pagination, transport,
  validation, or limit failures. Aggregation is capped at 1,000 pages, 100,000 ID
  entries, and 16 MiB of ID/state strings, rather than silently truncated.
- On `cannotCalculateChanges`, exit 1 with structured `resync-required` data,
  original `staleState`, the server error in `staleStateError`, and `currentState`.
  If the replacement-state lookup fails, return `currentState: null` and
  `currentStateError`. Never silently reset or continue.
- Document SQLite-style atomic consumption and recovery: capture a replacement
  state before backfill, durably queue the backfill IDs, then resume changes from
  that captured state. Use the same backfill procedure for oversized windows.
- Existing `watch`, GraphQL subscriptions, and legacy Rust APIs remain unchanged.
  Change history is server-dependent, not an audit log: messages created and
  destroyed between checkpoints may be omitted by JMAP entirely.

## Security

- Update `rustls` to 0.23.45, fixing RUSTSEC-2026-0285: TLS 1.3 handshake
  messages sent across encryption-level boundaries are now rejected.

Four platform archives include licenses; `SHA256SUMS` covers all four archives.
The release also publishes Linux amd64/arm64 container manifests.
