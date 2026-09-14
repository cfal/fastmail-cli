# Compose

All examples below create mail on the server. Send, reply, and forward submit
immediately unless `--draft` is present; none has a CLI preview/confirmation step.
For a request to write text for review, return the text without running a compose
command unless the user also requested a saved draft or a send.

## Identity And Recipients

```bash
fastmail list identities
```

`--from` takes an identity's **email address**, not its ID. Sending requires a
matching identity (case-insensitive); without `--from`, the first returned
identity is used. Specify it when the intended alias matters. Saving a draft can
succeed without resolving an identity, so draft success does not prove the
sender is valid for submission.

Recipient flags accept comma-separated addresses, optionally `Name <email>`.
The parser splits on commas; avoid display names containing commas. Review the
actual targets, including CC/BCC, rather than composing from `.data[0]` of an
unreviewed search or contact lookup.

## Send, Reply, Forward

Run only the command corresponding to the authorized action:

```bash
fastmail send --to alice@example.com --subject "Project update" --body "The review is complete."
fastmail reply EMAIL_ID --body "Thanks for the update."
fastmail reply EMAIL_ID --body "Thanks, everyone." --all --cc reviewer@example.com
fastmail forward EMAIL_ID --to reviewer@example.com --body "For your review."
```

- Send requires `--to`, `--subject`, and `--body`.
- Reply requires an email ID and `--body`. It uses nonempty `Reply-To`, falling
  back to `From`. `--all` also includes original To/Cc recipients, excluding the
  selected sending identity from those added recipients. Other aliases are not
  automatically excluded. To/Cc lists are deduplicated; original Bcc is not copied.
- Reply sets `In-Reply-To` and `References` from the original and adds `Re:` unless
  already prefixed. Prefer it to manually constructing a threaded send.
- Forward requires an email ID and `--to`; `--body` is optional. It adds `Fwd:`
  unless already prefixed and quotes the original **plain-text** content with
  attribution. It does not copy original attachments or convert an HTML-only
  original to text. Inspect the source before promising a complete forward.
- Send's `--reply-to` is an RFC message ID for `In-Reply-To`, not a JMAP email ID
  or an address for a `Reply-To` header.

All three accept `--cc`, `--bcc`, `--from`, `--draft`, and repeatable
`-a` / `--attachment PATH`. They also accept **one** of `--html-body HTML` or
`--html-file PATH` alongside the plain-text body. Supplied HTML is a separate
alternative; forwarding does not append the original content to it.

```bash
fastmail send --to alice@example.com --subject "Report" --body "Report attached." \
  --html-file ./message.html --attachment ./report.pdf --attachment ./data.csv --draft
```

Use only intended local files as attachments. To forward original attachments,
download them explicitly and attach the selected files; see
[Attachments](attachments.md).

## Results And Failures

Check `.success`, `.data.email_id`, and `.data.status` (`draft` or `sent`). Reply
also returns `.data.in_reply_to`; forward returns `.data.forwarded_from`, both
containing the source email ID. A successful submission is not proof of delivery.

Messages are created in Drafts and move to Sent after successful submission.
A submission rejection can leave a draft behind; a transport failure can leave
the outcome unknown. Inspect Drafts/Sent before any retry to avoid duplicates.
Repeating a compose command creates a new message; it does not update or send an
existing saved draft by ID.
