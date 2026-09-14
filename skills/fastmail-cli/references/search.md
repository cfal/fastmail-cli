# Search

`fastmail search` combines supplied filters with AND and returns email summaries.
Without a mailbox filter it searches across mailboxes; the default limit is 50.
Results are requested newest-first by `receivedAt`. A limited result is not an
exhaustive account search, and the CLI exposes no cursor/offset flag.

## Filters

| Flag | Meaning |
| --- | --- |
| `--text`, `-t` | Full-text search across from, to, cc, bcc, subject, and body |
| `--from`, `--to`, `--cc`, `--bcc` | Match the respective address field |
| `--subject`, `--body` | Search the respective text field |
| `--mailbox`, `-m` | Restrict to a mailbox name or role |
| `--after` | Received on or after the date/time |
| `--before` | Received before the date/time |
| `--unread` | Lacks the `$seen` keyword |
| `--flagged` | Has the `$flagged` keyword |
| `--has-attachment` | Has attachments |
| `--min-size`, `--max-size` | Email size in integer bytes, not `1M`-style values |
| `--limit`, `-l` | Maximum results, default 50 |

Dates accept ISO 8601 timestamps. A bare `YYYY-MM-DD` becomes midnight UTC, not
local midnight. Use explicit timestamps for time-sensitive boundaries.

```bash
fastmail search --from alice@example.com --unread --limit 20
fastmail search --subject "deployment" --mailbox "Work"
fastmail search --after 2026-01-01 --before 2026-02-01
fastmail search --has-attachment --min-size 1048576 --limit 20
```

Choose specific filters when the user identifies a sender, subject, or date range.
The text arguments are JMAP search strings, not a promise of regex or an arbitrary
mail-provider query language. A match is not proof of sender authenticity.

## Inspect Matches

Review IDs, recipients, subjects, and dates before choosing a message. Search
returns summaries, not full body content. Fetch only the relevant message or
conversation:

```bash
fastmail get EMAIL_ID
fastmail thread EMAIL_ID
```

If the result reaches the limit, narrow the filters or deliberately raise the
limit before claiming completeness. No matches is a valid result, not a reason
to fabricate an ID or silently broaden a sensitive operation.
