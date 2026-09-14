# v4.0.3

## Domain-Wide Sending

- Send from any concrete address authorized by a JMAP domain identity such as
  `*@yourdomain.com`. Exact identities take precedence; domain matching excludes
  subdomains and keeps the wildcard identity's submission ID.
- Use `--from 'My Team <new-address@yourdomain.com>'` for a per-message display
  name without creating or modifying a saved identity. The same behavior applies
  to replies, forwards, drafts, and GraphQL/MCP compose mutations.
- Keep the identity's saved name when `--from` is a bare address. Without
  `--from`, use the first non-wildcard identity; wildcard-only accounts require
  an explicit address to send.

## Sender Safety And Coverage

- Reject malformed, multiple, and literal or quoted wildcard senders locally.
  An explicit sender must resolve successfully before creating mail, including
  drafts. Drafts without `--from` can still be saved when identity lookup fails.
- Include sender names in GraphQL previews and confirmation bindings, preserve
  the reviewed default when upstream identities are reordered, and exclude the
  concrete sender from reply-all recipients.
- Expand coverage for identity selection, per-message names across all compose
  paths, confirmation binding, and rejected sends without account mutations.
- Live smoke testing verified domain-wide delivery and the display name in the
  received message's raw `From` header.

Four platform archives include licenses; `SHA256SUMS` covers all four archives.
The release also publishes Linux amd64/arm64 container manifests.
