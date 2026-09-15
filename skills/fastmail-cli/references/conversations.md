# Conversations And Triage

## List And Read

```bash
fastmail list mailboxes
fastmail list emails --mailbox INBOX --limit 20
fastmail get EMAIL_ID
fastmail thread EMAIL_ID
```

`list emails` defaults to INBOX and 50 summaries. Use mailbox names returned by
`list mailboxes`, or roles such as `inbox`, `sent`, and `archive`. Its email array
is `.data.emails`, unlike `.data` for search/thread results.

`get` fetches full content, including body values, headers, and attachment
metadata. It does not mark the email read. `thread` takes an **email ID**, not a
thread ID, and fetches the emails in that conversation. Do not assume thread
results are chronologically ordered; sort explicitly by the appropriate date
when the task requires it. An ID can stop resolving after deletion or an account
change.

## Readable Bodies

Full reads add `readableBody` beside the original JMAP fields. Prefer this view
over building an HTML parser. The default `auto` mode preserves genuine plain
text and converts HTML-only parts to Markdown. `format` names the actual output.

```bash
set -o pipefail
fastmail get EMAIL_ID | jq -e 'select(.success == true) | .data.readableBody'
fastmail get EMAIL_ID --body-format markdown
fastmail thread EMAIL_ID --body-format text
fastmail get EMAIL_ID --body-format raw
```

`markdown` prefers the HTML alternative for links, lists, tables, and quotes.
`text` prefers plain text and renders any HTML parts as text. `raw` omits the
derived field. None changes the original body values. Check `isTruncated`,
`isEncodingProblem`, and `warnings` before calling a message complete. Conversion
is best-effort, capped at 128 parts, 1 MiB input/output, and 64 HTML levels.
Images are alt-text/placeholders, never loaded or OCR'd; no image alt text can
prove what an image contains. Do not follow links just to render a message.

`sourceParts` records part IDs and MIME types considered up to the reading limits.
When `isTruncated` is true, later parts may be unlisted. For original content,
use each `textBody[]` or `htmlBody[]` part's `partId` to find its `bodyValues` entry.
Do not take the first map entry or concatenate both alternative representations.
JMAP's `textBody` can contain HTML: inspect each part's `type`. Check the raw
values' own truncation and encoding flags too. If no readable field is present,
check `fastmail --version`/`get --help` and the original body metadata rather than
claiming the message is empty. Attachment metadata is not attachment content;
see [Attachments](attachments.md).

On GraphQL/MCP, select `readableBody { format content isTruncated
isEncodingProblem warnings sourceParts { partId contentType } }` on an email.
The optional `format` argument takes `AUTO` (default), `MARKDOWN`, or `TEXT`.
The field shares the lazy body fetch and works on thread and subscription emails.

## Watch New Arrivals

```bash
fastmail watch --mailbox INBOX
fastmail watch --mailbox INBOX --full --poll 30
fastmail watch --full --body-format markdown --poll 30
```

Watch starts from the current state, not historical mail, and runs until
interrupted. It emits NDJSON, one envelope per arriving email. Without `--full`
it emits summaries. Without `--poll` it attempts push with polling fallback;
an explicit poll interval must be at least one second.

Use a bounded execution/cancellation plan unless a persistent monitor was
requested. Surface the stderr warning if the server drops change history:
resynchronization can miss arrivals, so watch is not an audit log.

## Resume Across Restarts

Use caller-managed checkpoints, not `watch`, when process/VM downtime must be
reconciled. These are bounded, read-only, one-shot commands and work with `--server`:

```bash
fastmail email-state
fastmail changes --since-state "$state"
```

`email-state` returns `.data.accountId` and `.data.state` without fetching mail.
`changes` immediately follows all JMAP pages and returns one envelope with
`accountId`, the supplied `oldState`, final `newState`, and `created`, `updated`,
`destroyed` ID arrays. It never fetches bodies or saves/acknowledges a cursor.
States are opaque and scoped to the account/server; preserve them exactly. No
mailbox filtering is supported. Arrays retain server order, duplicates, and IDs
appearing in multiple arrays, including creations later destroyed.

Require exit zero and `success: true`, then verify the account and old state.
Atomically enqueue every required ID and save `newState` in the caller's database;
use one checkpoint writer or compare the stored old state in that transaction.
Deduplicate work by account/email ID, fetch from the durable pending queue, and
leave failures pending. If fetching before enqueueing, do not advance the state
until every required fetch succeeds. A crash before commit can replay discoverable
IDs from the old state. Commit successful empty batches too.

Any failed page returns no successful batch or partial checkpoint. Aggregation
fails above 1,000 pages, 100,000 ID entries, or 16 MiB of ID/state strings rather
than silently truncating. Recover from `limitExceeded` with `email-state` and the
backfill procedure below, rather than repeatedly retrying the oversized window.
`cannotCalculateChanges` exits 1 with
`.data.type == "resync-required"`, `accountId`, original `staleState`, and
`currentState`; `staleStateError` preserves the original JMAP error and explanation.
Do not treat this as a successful advance. If the replacement
lookup failed, `currentState` is null and `currentStateError` gives the error;
obtain a fresh state with `email-state` before backfill.

For initial sync or recovery, capture the replacement state **before** a fully
paginated time-window backfill with overlap. Queue all backfill IDs durably before
committing that captured state, then run `changes` from it to cover concurrent
arrivals. Never replace it with a state sampled after backfill. Recovery coverage
is caller policy: expired history, deleted messages, and older-dated imports may
not be recoverable from a time window. Changes are not an audit log; a message
created and destroyed between checkpoints can be absent even before history expires.

## Authorized State Changes

These are separate operations, not automatic follow-ups to reading:

```bash
fastmail mark-read EMAIL_ID
fastmail mark-read EMAIL_ID --unread
fastmail move EMAIL_ID --to "Archive"
fastmail spam EMAIL_ID -y
```

Verify the intended message and destination first. Move replaces the message's
mailbox membership with the destination, rather than adding another mailbox.
Spam moves it to the junk mailbox and refuses without `-y`; it does not prompt
interactively. Reading a thread does not authorize marking every member read or
applying a bulk move.
