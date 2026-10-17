# v4.0.6

## Attachments And Drafts

- Send, reply, and forward with GraphQL/MCP attachments supplied as inline base64
  or existing blob IDs. Upload reusable blobs with `uploadAttachment`. Attachment
  previews are bounded and confirmations bind the ordered payload contents.
- Send saved drafts with `sendDraft`, preserving MIME content and pinning the
  submission identity. `deleteDraft` moves a draft to Trash with a fresh state
  guard rather than permanently deleting it.
- Infer image MIME types for outgoing CLI attachments instead of sending them as
  `application/octet-stream`.

## Protocol And Mutation Safety

- Validate JMAP method, call ID, account, and complete get results; require
  positive evidence for writes. Confirmed submission remains successful when
  automatic Sent filing fails: `sendDraft` returns `filingWarning`, while ordinary
  sends log a warning visible with `RUST_LOG=warn`. Uncertain writes are never
  retried automatically.
- Resolve internal Drafts, Sent, Junk, and Trash destinations by unique role and
  pin reply identities. User-supplied mailbox names retain their precedence.
  Reject ambiguous or incomplete CardDAV mutation discovery while preserving
  best-effort listing and untouched vCard properties.
- Use independent private temporary files for concurrent configuration saves.
  Reject invalid attachment download formats, oversized local HTML input, and
  scalar/array GraphQL variables; null variables remain equivalent to omission.

## GraphQL Performance And Limits

- Preserve cursor intervals in both pagination directions, give each attachment
  occurrence its own cursor, and retain match-all operands in boolean filters.
- Share attachment downloads within each request or subscription emission.
  Enforce cumulative 128 MiB input and 256 MiB serialized-payload budgets, retain
  the 64 MiB per-blob limit, and bound download and processing concurrency.
  Extraction backend internals still have documented memory and CPU limitations.
- Page thread member summaries before loading bodies, fetch bodies only when
  selected, and release the account lock during independent reads. Existing
  public Rust APIs and eager thread loading remain compatible.
- Bound arrival-change paging and report subscription history gaps explicitly.
  Oversized change windows stop the watcher without advancing its checkpoint.
  Subscriptions require exactly one expanded root-field occurrence, including
  aliases, fragments, and repeats.

Source builds require Rust 1.98.1 or newer.

Four platform archives include licenses; `SHA256SUMS` covers all four archives.
The release also publishes Linux amd64/arm64 container manifests.
