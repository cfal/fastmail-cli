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

## Body Values

Use each `textBody[].partId` to look up its entry in `bodyValues`. Do not take the
first map entry: it may be HTML or only one part of a multipart body.

```bash
set -o pipefail
fastmail get EMAIL_ID | jq -er '
  select(.success == true) | .data as $email
  | [($email.textBody // [])[]
     | select(.partId != null)
     | $email.bodyValues[.partId].value // empty]
  | join("\n")
'
```

If no text body exists, inspect `htmlBody` and its corresponding values rather
than claiming the message is empty. Check `isTruncated` and `isEncodingProblem`
on body values before treating them as complete, faithful text. Attachment
metadata is not attachment content; see [Attachments](attachments.md).

## Watch New Arrivals

```bash
fastmail watch --mailbox INBOX
fastmail watch --mailbox INBOX --full --poll 30
```

Watch starts from the current state, not historical mail, and runs until
interrupted. It emits NDJSON, one envelope per arriving email. Without `--full`
it emits summaries. Without `--poll` it attempts push with polling fallback;
an explicit poll interval must be at least one second.

Use a bounded execution/cancellation plan unless a persistent monitor was
requested. Surface the stderr warning if the server drops change history:
resynchronization can miss arrivals, so watch is not an audit log.

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
