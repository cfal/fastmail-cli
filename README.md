# fastmail-cli

CLI for Fastmail's JMAP API. Read, search, send, manage and *watch* email from your terminal or AI assistant.

See [SECURITY.md](SECURITY.md) for deployment boundaries, resource limits and
dependency advisory dispositions.

## Features

| Feature               | Description                                                            |
| --------------------- | ---------------------------------------------------------------------- |
| **Email**             | List, search, read, send, reply, forward, threads, identity selection, HTML bodies, file attachments |
| **Mailboxes**         | List folders, move emails, mark spam/read                              |
| **Real-time**         | `fastmail watch` streams arriving mail as NDJSON over JMAP push — pipe it into a shell loop. Also a GraphQL subscription |
| **Contacts**          | Search, create, update, delete contacts via CardDAV                    |
| **Attachments**       | Download files, extract text, resize images                            |
| **Text Extraction**   | Documents via [xberg](https://github.com/xberg-io/xberg), with Rust-native PDF extraction |
| **HTTP Client**       | `--server URL` runs mail and contact commands using server-owned credentials |
| **Image Resizing**    | `--max-size` to resize images on download                              |
| **Masked Email**      | Create, list, enable/disable aliases                                   |
| **MCP Server**        | Claude integration via Model Context Protocol                          |
| **Shell Completions** | Bash, Zsh, Fish, PowerShell                                            |
| **JSON Output**       | All commands output JSON for scripting                                 |

## Compared to Fastmail's official MCP

Fastmail [ships an official MCP server](https://www.fastmail.com/blog/an-mcp-server-for-fastmail/)
(hosted at `api.fastmail.com/mcp`, OAuth with read/write/send scopes). It's
zero-setup and covers more of Fastmail's suite — calendar, notes, and org
directory that `fastmail-cli` doesn't touch. `fastmail-cli` is the self-hosted
alternative: a CLI *and* an MCP server you run yourself, open source, with
masked email, attachment text extraction, spam-filter training, and a real-time
stream of incoming mail the official server doesn't offer — plus full custody of
the data path.

|                                                            | `fastmail-cli`                     | Fastmail official MCP            |
| ---------------------------------------------------------- | ---------------------------------- | -------------------------------- |
| Interface                                                  | CLI **and** MCP (stdio / HTTP)     | MCP only                         |
| Setup                                                      | install + run the binary           | add URL + OAuth, nothing to run  |
| Auth                                                       | API token (+ app password for CardDAV) | OAuth: `read` / `write` / `send` |
| Hosting / data path                                        | your machine or your own server    | Fastmail-hosted                  |
| Source                                                     | open source (MIT), extensible      | proprietary                      |
| Email: read / search / threads                             | ✅ (rich search filters)           | ✅                               |
| Send / reply / forward (preview → confirm/draft)           | ✅                                 | ✅                               |
| Move / archive / mark read                                 | ✅                                 | ✅                               |
| Real-time stream of incoming mail                          | ✅ (CLI + GraphQL subscription)    | —                                |
| Mark as spam (+ trains the filter)                         | ✅                                 | —                                |
| Masked Email (create / enable / disable / delete)          | ✅                                 | —                                |
| Attachments: document text extraction + image resize       | ✅                                 | —                                |
| Contacts (create / update / delete / search)               | ✅ (CardDAV)                       | ✅                               |
| Org directory search                                       | —                                  | ✅                               |
| Calendar                                                   | —                                  | ✅                               |
| Notes                                                      | —                                  | ✅                               |
| Identities / signatures                                    | ✅                                 | ✅                               |

Use Fastmail's for zero maintenance and the wider suite (calendar, notes);
use `fastmail-cli` for a scriptable CLI, mail that streams as it arrives, the
masked-email / attachment-extraction / spam-training tooling, or to self-host and
keep the data path yours. Streaming is the one difference that isn't a matter of
scope: MCP tools are request/response, so a hosted MCP server cannot hand you a
subscription that never returns, whichever tools it grows.
(`fastmail-cli`'s column is verified against its GraphQL schema; Fastmail's is
its current hosted tool set and may grow.)

## Quick Start

### Installation

#### From GitHub Releases (recommended for mise)

```bash
# Add to mise config
mise use -g "github:cfal/fastmail-cli"
```

#### From Source

```bash
cargo install --git https://github.com/cfal/fastmail-cli --locked
```

### HTTP Client

Direct Fastmail access remains the default. To keep Fastmail credentials on a
server instead, start HTTP mode there and pass its URL to the CLI:

```bash
# Server, with Fastmail credentials configured there
fastmail mcp --http 127.0.0.1:8080

# Client, with no Fastmail token or CardDAV password
fastmail --server http://127.0.0.1:8080 list emails
fastmail --server http://127.0.0.1:8080 contacts list
fastmail --server http://127.0.0.1:8080 watch
```

`FASTMAIL_SERVER` can supply the URL. When the server uses optional Basic auth,
either include the login in that URL or set `FASTMAIL_SERVER_USER` (or
`--server-user`) and `FASTMAIL_SERVER_PASSWORD` separately. For example, using
placeholder credentials:

```bash
export FASTMAIL_SERVER='https://user:pass@server.example:8443'
fastmail list emails
```

Percent-encode reserved characters in the username and password, such as `%40`
for `@`, `%23` for `#`, `%2F` for `/`, and `%3F` for `?`; write a literal `%` as
`%25`. `+` remains a literal plus. Both values must be nonempty, and the username
cannot contain a colon. Server URLs must not contain raw control characters,
including trailing CR/LF from a secret file. Do not combine URL credentials
with the separate login settings; mixed sources are rejected. Unset unused
credential variables; empty values still count as a second source.

The CLI extracts URL credentials into the Basic authorization header, removes
them from request URLs, and hides server URL and username environment values in
help output. Treat the whole credential-bearing URL as a secret: load it from a
secret manager or private environment configuration, not a command-line argument
or shell history. Use HTTPS for remote authenticated access.

Mail, mailbox, identity, masked-email, contact, attachment and watch requests
all go through `/cli/v1/*`. There is no direct Fastmail fallback. Attachment
paths, download destinations, extraction and confirmation prompts remain local.
`auth`, `mcp` and shell completion generation are local administration commands;
`auth` and `mcp` reject `--server` to prevent configuring the wrong machine.

### Authentication

1. Generate an API token at [Fastmail Settings > Privacy & Security > Integrations > API tokens](https://app.fastmail.com/settings/security/tokens)
2. Auth with the CLI — the token is read from stdin so it stays out of shell history, `ps`, and the process environment:

```bash
# interactive — paste the token at the prompt
fastmail auth

# non-interactive — pipe from a password manager, file, or env var
echo "$FASTMAIL_TOKEN" | fastmail auth
```

The positional form `fastmail auth YOUR_TOKEN` still works for backward compatibility, but the stdin form is preferred.

Token is stored in `~/.config/fastmail-cli/config.toml` with `0600` permissions (directory `0700`). The file is written atomically via rename, and the path is refused if it's a symlink.

### Configuration

Credentials can be set via environment variables or config file. Env vars take precedence.

**Environment variables:**

```bash
export FASTMAIL_API_TOKEN="fmu1-..."      # Required for JMAP (email)
export FASTMAIL_USERNAME="you@fastmail.com"  # Required for CardDAV (contacts)
export FASTMAIL_APP_PASSWORD="xxxx..."    # Required for CardDAV (contacts)
```

**Config file** (`~/.config/fastmail-cli/config.toml`):

```toml
[core]
api_token = "fmu1-..."

[contacts]
username = "you@fastmail.com"
app_password = "xxxx..."
```

The `auth` command only sets `[core].api_token`. For contacts, add `[contacts]` section manually or use env vars.

## Usage

All output is JSON for easy scripting with `jq`.

### List Mailboxes

```bash
fastmail list mailboxes
```

### List Emails

```bash
# Default: INBOX, 50 emails
fastmail list emails

# Specific mailbox and limit
fastmail list emails --mailbox Sent --limit 10
```

### Get Email Details

```bash
fastmail get EMAIL_ID
```

Full CLI reads (`get`, `thread`, and `watch --full`) add `readableBody` alongside
the unchanged JMAP fields. Its `content` is ready to read, with an actual `format`
of `text` or `markdown`, ordered `sourceParts` (`partId` and `type`) considered
up to the reading limits, `isTruncated`, `isEncodingProblem`, and `warnings`.
When `isTruncated` is true, later parts may be absent from `sourceParts`.

```bash
# Default: prefer genuine plain text; convert HTML-only parts to Markdown
fastmail get EMAIL_ID | jq '.data.readableBody'

# Prefer the richer HTML alternative, converted to Markdown
fastmail thread EMAIL_ID --body-format markdown

# Plain-text rendering, or only the original JMAP fields without conversion
fastmail get EMAIL_ID --body-format text
fastmail get EMAIL_ID --body-format raw
```

`--body-format auto|markdown|text|raw` never changes `bodyValues`, `textBody`, or
`htmlBody`. JMAP's `textBody` is a text-*preferred* part sequence, not a guarantee
of plain text. Conversion checks each part's MIME type and keeps the selected
sequence in order, without concatenating alternative representations. No
`readableBody` is added when there are no body parts; list/search summaries stay
unchanged. Reading does not mark mail read.

HTML conversion uses [html-to-markdown-rs](https://github.com/xberg-io/html-to-markdown)
locally. Markdown preserves links, lists, tables, and quotation structure without
article extraction or quote trimming; plain-text rendering retains the content
but simplifies formatting. Plain-text parts in a Markdown view use literal blocks
to preserve line breaks and indentation. Images become alt-text/placeholders;
scripts, styles, comments, and obviously hidden elements are omitted. No remote,
CID, or data-URL resource is loaded, and no scripts execute. Image-only content
still needs separate inspection. Layout and visibility are best-effort, not a
browser rendering or a security sanitizer; all content remains untrusted.
Text mode reports omitted SVG/MathML with a notice at the end of the body part.

The derived view processes at most 128 parts and 1 MiB of selected input, emits
at most 1 MiB of content, and uses a 64-level HTML traversal limit. Oversized HTML
parts are skipped rather than parsed partially. Check the flags and warnings
before treating a view as complete; raw values remain available for missing,
unsupported, or failed conversions. Limits do not change the upstream raw body
fetch. `--body-format` on `watch` requires `--full`.

### Search

Search uses JMAP filter flags (all filters are ANDed together):

```bash
# Full-text search
fastmail search --text "meeting notes"

# Filter by header fields
fastmail search --from "alice@example.com"
fastmail search --to "bob" --subject "project"

# Filter by mailbox
fastmail search --mailbox Sent --limit 10

# Attachments and size
fastmail search --has-attachment
fastmail search --min-size 1000000  # > 1MB

# Date range (ISO 8601)
fastmail search --after 2024-01-01 --before 2024-12-31

# Status filters
fastmail search --unread
fastmail search --flagged

# Combine filters
fastmail search --from "boss" --has-attachment --after 2024-06-01 --limit 20
```

Available flags: `--text`, `--from`, `--to`, `--cc`, `--bcc`, `--subject`, `--body`, `--mailbox`, `--has-attachment`, `--min-size`, `--max-size`, `--before`, `--after`, `--unread`, `--flagged`

### Watch for New Mail

Block and emit one JSON object per line as mail arrives, so a shell loop can act
on it:

```bash
# Everything that arrives, anywhere in the account
fastmail watch

# Just the inbox
fastmail watch --mailbox inbox

# Pipe into a loop
fastmail watch --mailbox inbox | while read -r line; do
  echo "$line" | jq -r '.data.subject'
done

# Include bodies and attachment metadata, not just summaries
fastmail watch --full

# Fall back to polling every 60s where a long-lived connection won't survive
fastmail watch --poll 60
```

Output is the same `{"success":true,"data":{...}}` envelope as every other
command, one compact line per email, flushed as it is written — so `jq` filters
and `read` loops both work unbuffered.

It uses JMAP's push channel (`eventSourceUrl`), but treats a notification purely
as a signal to look again: the state cursor lives in the CLI, and each wake-up
runs `Email/changes` against it. Dropped connections are reconciled on reconnect
and `--poll` takes the identical path, so a missed notification costs latency
rather than mail. Only *new* messages are reported — flag and folder changes to
existing mail are not arrivals.

Reconnects, and the rare case where the server has discarded change history and
the cursor has to resync, are reported on stderr; stdout stays pure NDJSON.

Watch keeps its cursor only in memory and starts at the current state after a
process restart. Use the commands below when downtime must be reconciled from
a caller-owned checkpoint. Existing watch and GraphQL subscription behavior is
unchanged; neither accepts a saved Email state.

### Caller-Managed Email Checkpoints

```bash
# Read the current account-wide Email state, without retrieving any mail
fastmail email-state

# Immediately catch up from the last state your application committed
fastmail changes --since-state "$state"
```

Both commands support `--server` and require only read access. `email-state`
returns `{"success":true,"data":{"accountId":"account","state":"s0"}}`.
`changes` follows every `hasMoreChanges` page before printing one JSON envelope:

```json
{
  "success": true,
  "data": {
    "accountId": "account",
    "oldState": "s0",
    "newState": "s2",
    "created": ["email-1", "email-2"],
    "updated": ["email-3"],
    "destroyed": ["email-4"]
  }
}
```

These commands are stateless and ID-only: no body fetch, polling, local cursor
file, or implicit acknowledgment. Treat states as opaque strings scoped to the
account and server, not timestamps or sortable values. There is no mailbox
filter. The arrays concatenate every page in server order, not chronological
order; duplicates and IDs in multiple arrays are retained. In particular, a
created ID is not removed if a later page reports it destroyed. Deduplicate
work by account and email ID. Empty batches are successful too, and their state
may still advance.

For a SQLite-backed consumer:

1. Read the last committed state and call `changes`. Require exit status zero
   and `success: true`; verify `accountId` and `oldState` against your checkpoint.
2. In one transaction, enqueue **all** IDs you need and save `newState`. Use a
   unique account/email key for pending work. Use one checkpoint writer, or
   compare the stored state with `oldState` inside the transaction to reject
   competing advances.
3. Fetch/process the durable pending IDs separately. Leave failed work pending;
   saving a discovery checkpoint is not acknowledgment of successful processing.
   An ID may already be deleted when fetched; handle that explicitly.

A crash before that transaction commits leaves the old state intact. Restarting
from it rediscovers the still-available changes, possibly with additional changes
since the previous attempt. If you fetch before enqueueing instead, do not save
`newState` until every required fetch succeeds. A page/transport/parse failure
returns exit status 1 and no successful batch or partial checkpoint. Aggregation
also fails, rather than truncates, above 1,000 pages, 100,000 ID entries (including
duplicates), or 16 MiB of ID/state strings across pages.
For `limitExceeded`, capture a fresh state with `email-state` and use the backfill
procedure below instead of repeatedly retrying the oversized window.

If JMAP returns `cannotCalculateChanges`, `changes` exits 1 with structured data:

```json
{
  "success": false,
  "error": "Email change history is unavailable; backfill before using currentState",
  "data": {
    "type": "resync-required",
    "accountId": "account",
    "staleState": "s0",
    "staleStateError": "JMAP error: Email/changes failed - cannotCalculateChanges: State expired",
    "currentState": "replacement-state"
  }
}
```

`staleState` is the original caller-supplied state, even if a later page failed.
`staleStateError` preserves the JMAP error and any server-provided explanation.
No partial IDs are emitted and the command does not reset or continue. If the
replacement-state lookup also fails, `currentState` is `null` and
`currentStateError` explains why; obtain a state with `email-state` before recovery.

For initial synchronization or resync, capture a replacement state **before**
starting a fully paginated backfill over your chosen time window, with overlap.
Only after the backfill's IDs are durably queued should you commit that captured
state; then run `changes` from it to catch arrivals during backfill. Never sample
a fresh state after backfill and commit that instead: it can skip concurrent mail.

JMAP changes are not an audit log. History retention is server-dependent, and
messages both created and destroyed between checkpoints can be omitted entirely
by the server. A time-window backfill cannot reconstruct deleted mail or guarantee
coverage of imports with older received dates. Choose recovery coverage for your
application; these commands do not promise exactly-once delivery or full history.

### List Identities

View available sender identities (useful for `--from`):

```bash
fastmail list identities
```

`--from` accepts an email address or `Name <address>`. Exact identities take
precedence over domain identities such as `*@yourdomain.com`, which authorize
any concrete address at that domain (not its subdomains). Supply the concrete
address, never the literal wildcard. Without `--from`, the first non-wildcard
identity is used; wildcard-only accounts require an explicit address.

The optional name overrides the display name for that message only. It does not
create or modify an identity. This also applies to replies, forwards, drafts,
and the GraphQL/MCP compose mutations' `from` argument.

### Send Email

```bash
fastmail send \
  --to "alice@example.com, bob@example.com" \
  --subject "Hello" \
  --body "Message body here"

# With CC/BCC
fastmail send \
  --to "alice@example.com" \
  --cc "bob@example.com" \
  --bcc "secret@example.com" \
  --subject "Hello" \
  --body "Message"

# Send from a specific identity/alias
fastmail send \
  --to "alice@example.com" \
  --from "alias@yourdomain.com" \
  --subject "Hello" \
  --body "Message"

# Send through a domain identity with a per-message display name
fastmail send \
  --to "alice@example.com" \
  --from "My Team <new-address@yourdomain.com>" \
  --subject "Hello" \
  --body "Message"

# HTML email body (inline or from file)
fastmail send \
  --to "alice@example.com" \
  --subject "Newsletter" \
  --body "Plain text fallback" \
  --html-body "<h1>Hello</h1><p>Rich content here</p>"

fastmail send \
  --to "alice@example.com" \
  --subject "Report" \
  --body "See attached" \
  --html-file ./email.html

# File attachments (repeatable)
fastmail send \
  --to "alice@example.com" \
  --subject "Documents" \
  --body "Please review" \
  -a report.pdf -a data.xlsx
```

### Move Email

```bash
fastmail move EMAIL_ID --to Archive
fastmail move EMAIL_ID --to Trash
```

### Mark as Spam

```bash
# Requires confirmation
fastmail spam EMAIL_ID

# Skip confirmation
fastmail spam EMAIL_ID -y
```

### Mark as Read/Unread

```bash
# Mark as read
fastmail mark-read EMAIL_ID

# Mark as unread
fastmail mark-read EMAIL_ID --unread
```

### Download Attachments

```bash
# Download to current directory
fastmail download EMAIL_ID

# Download to specific directory
fastmail download EMAIL_ID --output ~/Downloads

# Extract text content as JSON (PDF, DOCX, DOC, TXT)
fastmail download EMAIL_ID --format json

# Resize images to max 500KB
fastmail download EMAIL_ID --max-size 500K
```

Images that cannot be resized within the limit are skipped while other attachments
continue. Partial results have `success: false`, written paths in `data.files`, and
filenames/errors in `data.skipped`; oversized originals are never written as a fallback.

Text extraction uses [xberg](https://github.com/xberg-io/xberg), including these document formats:

- **Documents**: PDF, DOC, DOCX, ODT, RTF
- **Spreadsheets**: XLS, XLSX, ODS, CSV, TSV
- **Presentations**: PPT, PPTX
- **eBooks**: EPUB, FB2
- **Markup**: HTML, XML, Markdown, RST, Org
- **Data**: JSON, YAML, TOML
- **Email**: EML, MSG
- **Archives**: ZIP, TAR, GZ, 7z
- **Academic**: BibTeX, LaTeX, Typst, Jupyter notebooks

### Reply to Email

```bash
# Reply to sender only
fastmail reply EMAIL_ID --body "Thanks for your message"

# Reply all
fastmail reply EMAIL_ID --body "Thanks everyone" --all

# Reply with additional CC/BCC
fastmail reply EMAIL_ID --body "Response" --cc "boss@example.com"

# Reply from a specific identity
fastmail reply EMAIL_ID --body "Thanks" --from "alias@yourdomain.com"
```

### Forward Email

```bash
fastmail forward EMAIL_ID \
  --to "colleague@example.com" \
  --body "FYI - see below"

# Forward from a specific identity
fastmail forward EMAIL_ID \
  --to "colleague@example.com" \
  --from "alias@yourdomain.com" \
  --body "FYI"
```

### Shell Completions

```bash
# Bash
fastmail completions bash >> ~/.bashrc

# Zsh
fastmail completions zsh >> ~/.zshrc

# Fish
fastmail completions fish > ~/.config/fish/completions/fastmail.fish
```

### Contacts

CRUD operations for Fastmail contacts via CardDAV. Requires an app password (API tokens don't work for CardDAV).

```bash
# Set credentials
export FASTMAIL_USERNAME="you@fastmail.com"
export FASTMAIL_APP_PASSWORD="your-app-password"

# List all contacts
fastmail contacts list

# Search by name, email, or organization
fastmail contacts search "alice"

# Create a new contact
fastmail contacts create --name "Jane Doe" --email "jane@example.com" --organization "Acme Corp"

# Update an existing contact (only provided fields are changed)
fastmail contacts update CONTACT_ID --organization "New Corp" --title "CEO"

# Delete a contact (requires -y confirmation)
fastmail contacts delete CONTACT_ID -y
```

Generate an app password at [Fastmail Settings > Privacy & Security > Integrations > App passwords](https://app.fastmail.com/settings/security/devicekeys).

### Masked Email

Create disposable email addresses for signups. Requires Fastmail's masked email feature.

```bash
# List all masked emails
fastmail masked list

# Create a new masked email
fastmail masked create --domain "https://example.com" --description "Example Site"

# Create with custom prefix
fastmail masked create --prefix "shopping" --description "Shopping sites"

# Enable/disable a masked email
fastmail masked enable MASKED_EMAIL_ID
fastmail masked disable MASKED_EMAIL_ID

# Delete (requires confirmation)
fastmail masked delete MASKED_EMAIL_ID -y
```

## Output Format

Mail and contact command results normally use this JSON envelope:

```json
{
  "success": true,
  "data": { ... },
  "message": "optional status message",
  "error": "error message if success=false"
}
```

### Parsing with jq

```bash
# Get unread count for INBOX
fastmail list mailboxes | jq '.data[] | select(.role == "inbox") | .unreadEmails'

# List email subjects
fastmail list emails | jq '.data.emails[].subject'

# Read without hand-parsing HTML; retain fidelity warnings
fastmail get EMAIL_ID | jq '.data.readableBody'

# Inspect original email body parts and their values
fastmail get EMAIL_ID | jq '.data | {textBody, htmlBody, bodyValues}'
```

## Agent Skill

The portable [fastmail-cli skill](skills/fastmail-cli/SKILL.md) teaches agents how
to use the CLI, interpret results, and distinguish read-only operations from
writes. Its focused references cover search, conversations, composition,
attachments, contacts, and masked addresses.

Install the entire `skills/fastmail-cli` directory in your agent's supported skill
location, keeping `SKILL.md` and `references/` together. For
[Claude Code](https://claude.ai/claude-code), run from this repository's root:

```bash
mkdir -p ~/.claude/skills
cp -R skills/fastmail-cli ~/.claude/skills/
```

Then invoke `/fastmail-cli`. The topic references are loaded as needed, not
separate slash commands. Other agents use their own skill installation and
invocation mechanisms. The skill does not install the `fastmail` executable or
configure credentials.

## MCP Server (Claude Integration)

Run as an MCP server for use with Claude Desktop or other MCP clients:

```bash
fastmail mcp
```

Configure in Claude Desktop's `claude_desktop_config.json`:

```json
{
  "mcpServers": {
    "fastmail": {
      "command": "mise",
      "args": ["x", "--", "fastmail", "mcp"],
      "env": {
        "FASTMAIL_API_TOKEN": "your-token-here",
        "FASTMAIL_USERNAME": "you@fastmail.com",
        "FASTMAIL_APP_PASSWORD": "your-app-password"
      }
    }
  }
}
```

Username and app password are optional - only needed for contact search (CardDAV requires app password, API tokens don't work).

### HTTP transport

Three independent surfaces, each opt-in, sharing one port (default
`127.0.0.1:8080`, or pass an address to `--http`):

| Flag         | Serves                                                      |
| ------------ | ----------------------------------------------------------- |
| `--http`     | MCP streamable-HTTP at `/mcp`                               |
| `--graphql`  | plain GraphQL-over-HTTP at `/graphql`, subscriptions at `/graphql/stream` |
| `--graphiql` | the GraphiQL IDE at `/`, and its `/graphql`                 |
| `--browser`  | opens the IDE once the port is bound, implying `--graphiql` |

```bash
fastmail mcp                                   # stdio MCP, no listener
fastmail mcp --browser                         # HTTP IDE, opened for you
fastmail mcp --http                            # /mcp and CLI transport
fastmail mcp --http 0.0.0.0:8080 --graphql     # both, explicit address
```

Asking for any HTTP surface binds the listener and mounts `/mcp` and `/cli/v1/*`.
`--browser` implies `--graphiql`, since the IDE is what it opens. GraphiQL assets
and editor workers are embedded locally; queries and headers are not persisted
to browser storage.

`/graphql` is plain GraphQL-over-HTTP, which is what a browser speaks; `/mcp` is
MCP JSON-RPC, which it doesn't. That is why GraphiQL needs its own route rather
than pointing at the MCP one. Both share the schema, the client cache and the
credential resolution below, so the IDE sees exactly what a model sees.

`/graphql/stream` carries subscriptions over Server-Sent Events using the
distinct-connection `graphql-sse` protocol (`next` and `complete` events).
GraphiQL routes subscriptions here automatically. POST the operation and read
events off the response:

```bash
curl -N http://127.0.0.1:8080/graphql/stream \
  -H 'Content-Type: application/json' \
  -d '{"query":"subscription { emails(mailbox: \"inbox\") { id subject from { email } } }"}'
```

The stream is not resumable: `Last-Event-ID` is not supported, and a new POST
starts watching from the current Fastmail state. Query for mail received during
a subscriber disconnect before starting a new subscription. Upstream Fastmail
reconnects are reconciled while the subscriber request stays open.

Use `full: true` to fetch body and attachment fields with each arrival. Lazy
fields also work; subscription record caches are disabled so records do not
accumulate for the connection lifetime. `pollSeconds` is the same fallback as
the CLI's `--poll`; both require an interval of at least one second. MCP has no
equivalent: tools are request/response, and a subscription never returns.

**The HTTP server uses its own configured Fastmail credentials.** It no longer
accepts `X-Fastmail-Token`, `X-Fastmail-Username`, or `X-Fastmail-App-Password`
overrides. Configure `FASTMAIL_API_TOKEN` (or `fastmail auth`) on the server,
plus `FASTMAIL_USERNAME` and `FASTMAIL_APP_PASSWORD` for contacts.

HTTP authentication is optional, including on non-loopback listeners. Without
it, anyone who can reach the server can use the configured account. Choose the
listener address, firewall and reverse-proxy policy accordingly.

To enable Basic authentication, pass a TOML password file:

```toml
[users]
alice = "replace-with-a-long-unique-password"
bob = "replace-with-a-different-password"
```

```bash
chmod 600 users.toml
fastmail mcp --http 0.0.0.0:8080 --graphql --auth-file users.toml
curl --user alice http://127.0.0.1:8080/graphql \
  -H 'Content-Type: application/json' -d '{"query":"{ session { status } }"}'
```

These usernames identify HTTP clients, not Fastmail accounts: all authorized
users access the same server-owned mailbox and contacts. The file is read at
startup; restart the server after changing it. Empty or invalid files fail
closed. Passwords are stored in plaintext in the file, so keep it private and
out of version control. Basic auth is not encryption: use HTTPS through a
reverse proxy for remote access.

Host and same-origin checks cover every HTTP endpoint, with or without auth.
Loopback listeners accept loopback hosts by default. Non-loopback listeners
accept arbitrary hosts unless restricted with repeated `--allowed-host HOST`
options (hostnames without ports). Set your public hostname when using a proxy,
and preserve the original `Host` header. Cross-origin browser access is rejected.

The token is resolved on first query rather than at startup, so an expired one
shows up as an error in the response pane rather than a server that won't boot —
run `fastmail auth` to refresh it, or ask `session` first. **Introspection needs
no token**: it is answered from the schema without touching Fastmail, so
GraphiQL's docs, autocomplete and explorer work before you have working
credentials. Queries that select any real field still authenticate as normal.

#### Checking a connection

`session` answers whether a token still works, and needs no mail to exist to do
it:

```graphql
{ session { status username primaryAccountId capabilities carddavConfigured detail } }
```

One `GET /jmap/session` — the handshake Fastmail only completes for a request it
has authenticated. `status` is `CONNECTED`, `INVALID_CREDENTIALS`, or
`UNREACHABLE`, and a bad connection is reported there rather than raised as a
GraphQL error: it is the answer to this question, not a failure to answer it.
The split is the one a UI acts on — tell the user to re-authenticate, or tell
them to wait. A 401 is the only thing that reads as a credential verdict;
everything else (5xx, rate limiting, timeouts, DNS) is `UNREACHABLE`, because
telling someone their token is dead over an outage would be a lie. `detail`
carries the reason in prose for a tooltip; branch on `status`, which won't be
reworded.

It re-runs the handshake rather than reading the cached client — clients are
cached per token for the life of the process, so a cached answer would keep
reporting success long after a revocation, which is the case this exists to
catch. When connected it also reports the accounts the token reaches and the
capability URNs it was granted, which is what says whether masked email and
sending are available at all.

`carddavConfigured` is the one thing there that `capabilities` cannot answer.
Capabilities are what the JMAP server advertises, and contacts go over CardDAV —
a separate protocol, with separate credentials, invisible to the handshake. So
without it the only way to find out that `contacts` is unavailable is to run it
and fail, halfway through a plan that assumed it. It reports whether a username
and app password are both present, not whether they are correct, and it answers
independently of `status`: credentials are local configuration, so a dead API
token doesn't make contact reachability unanswerable.

The MCP server exposes **2 tools** via a GraphQL interface:

- **`graphql`** — executes any GraphQL query or mutation. Its description carries a slimmed schema for everyday mail: the queries, the `EmailFilter` tree, the common `Email` fields, the connection shape and the PREVIEW→CONFIRM send flow
- **`schema_sdl`** — the full SDL, with an optional `types` list (e.g. `["MutationRoot", "Attachment"]`) returning only those definitions

The split is about round trips. The SDL is ~27KB, most of it the doc comments
that make it worth reading, and it was previously the only way to learn
anything — so reading mail cost a 27KB fetch first, and cost it again whenever
the connection dropped. The common case is now answered where the model is
already looking, and `schema_sdl` is for what the sketch explicitly says it
doesn't cover: attachment payloads, masked email, contacts, identities,
`moveEmail`, `markAsRead`, `markAsSpam`, and the remaining filter and sort
options. `types` keeps that second hop small too. Named types come back whole
but their references don't, so name those as well; an unrecognised name is
reported alongside the list of names that do exist, rather than silently
dropped.

Both the worked examples and the inlined sketch are checked by the test suite —
the examples are executed against the real schema, and every field name in the
sketch must exist in it. A cheat sheet that outlives a rename is worse than no
cheat sheet.

This replaces the previous 18 individual tools with a composable interface. The LLM fetches the schema once, then constructs exactly the queries it needs — fetching multiple resources in a single round-trip, requesting only the fields it wants, and using typed arguments for filtering and pagination.

### Nested resolution

The object graph is fully navigable, so the LLM can get everything it needs in one hit:

```
Mailbox ──emails──▶ Email ──attachments──▶ Attachment ──base64/image/text──▶
   ▲ │                │ │
   │ └─parent/children┘ ├──thread──▶ Thread ──emails──▶ Email …
   └────mailboxes───────┘
```

Every collection is a Relay connection — `mailboxes`, `identities`,
`maskedEmails`, `contacts`, `attachments`, `Mailbox.children`, `Thread.emails` —
so each takes `first` / `last` / `after` / `before` and exposes `totalCount`,
`pageInfo`, `edges` and `nodes`. Cursors are IDs. Use `nodes` for the items;
`edges` only when you want per-item cursors. Default page 25, max 100 — check
`pageInfo.hasNextPage` rather than assuming you got the lot.

Only `emails` pages server-side, via JMAP's anchors. The rest arrive whole from
one call, so their paging is slicing: `totalCount` is free (a length, not
`calculateTotal`) and a cursor holds only as long as its item is still in the
list — if it goes, you get a "restart pagination" error rather than a quietly
different page. `Thread.emails` returns the same `EmailConnection` as the query
does, with `queryState` null because a conversation is not a query.

Value lists stay plain arrays: `from`, `to`, `cc`, `bcc`, `replyTo`, `sender`,
`keywords`, `mailboxIds`, `headers`, `Contact.emails`. They belong to the parent,
arrive with it, and paging them would be ceremony.

Attachment payloads are three separate fields, each doing the least work that
answers it. Metadata (`name`, `size`, `contentType`, `cid`, …) arrives with the
email and downloads nothing at all:

- `base64` — the raw bytes, base64-encoded. Download only, any type.
- `image(maxBytes:)` — resized then encoded, so a model isn't handed a 10MB
  photo. Null for non-images.
- `text` — extracted document text. **The expensive one**: it parses the whole
  file, is priced far above the other two, and nothing else on `Attachment`
  triggers it. Prefer a small page when selecting it.

```graphql
# Bodies and attachment text for a whole folder — one query, 3 API calls.
# Note the smaller page: `text` parses every document, so a big page means a
# lot of work. Nothing stops you asking for more; the cost is just yours.
{
  emails(filter: { inMailbox: "INBOX" }, first: 10) {
    nodes {
      subject
      from { name email }
      readableBody { format content isTruncated isEncodingProblem warnings }
      attachments {
        nodes { name contentType size cid text }
      }
    }
  }
}

# Walk the graph: folder tree → emails → conversation → the folders they live in
{
  mailbox(name: "INBOX") {
    name
    children { nodes { name unreadEmails } }
    emails(first: 5) {
      nodes {
        subject
        thread { total emails { nodes { subject readableBody { format content warnings } } } }
        mailboxes { name role }
      }
    }
  }
}
```

`Email.readableBody(format: AUTO)` is lazy and shares the same batched detail
fetch as raw bodies and attachment metadata. `MARKDOWN` prefers the HTML
alternative; `TEXT` renders plain text. `sourceParts { partId contentType }`
identifies parts considered up to the reading limits. The field is available on individual messages,
connections, threads, and subscription arrivals, including through MCP's
`graphql` tool. `textBody` and `htmlBody` remain raw joined JMAP values, not
converted reading views.

### Composable filters

`EmailFilter` mirrors JMAP's filter tree (RFC 8620 §5.5) rather than flattening
it into scalar arguments. Fields on one filter object are AND-ed; `and` / `or` /
`not` nest arbitrarily. The same input type is accepted everywhere emails
appear, including `Mailbox.emails`, where it's AND-ed with the mailbox.

```graphql
# Unread, from either sender, not in Archive, biggest first
{
  emails(
    filter: {
      unread: true
      or:  [{ from: "alice@example.com" }, { from: "bob@example.com" }]
      not: [{ inMailbox: "Archive" }]
    }
    sort: [{ property: SIZE, ascending: false }]
    first: 20
  ) {
    totalCount
    nodes { subject size }
  }
}
```

Mailbox names and roles are resolved to IDs at every depth of the tree in a
single `Mailbox/get`. `collapseThreads: true` returns one email per
conversation, for a threaded view.

The filter mirrors JMAP's `FilterCondition` (RFC 8621 §4.4.1) field for field:
alongside the usual participant/date/size conditions there are
`inMailboxOtherThan`, `hasKeyword` / `notKeyword`, the thread-wide
`allInThreadHaveKeyword` / `someInThreadHaveKeyword` / `noneInThreadHaveKeyword`,
and raw `header` matching (`["List-Id"]` for presence, `["List-Id", "rust-lang"]`
for a value).

Sorting likewise covers the full comparator set — `receivedAt`, `sentAt`, `size`,
`subject`, `from`, `to`, plus the keyword-based `hasKeyword`,
`allInThreadHaveKeyword` and `someInThreadHaveKeyword`, with `collation`. The
keyword comparators require a `keyword` argument and the others reject one;
both are caught before any API call.

### Pagination

Email lists are Relay connections with `edges`/`nodes`, `pageInfo`, `totalCount`,
`position`, and `queryState`.

**Cursors are email IDs**, mapped onto JMAP's `anchor` / `anchorOffset`. This
falls out of what the API already offers, and it's what makes pagination stable:
a positional cursor silently shifts every time mail arrives, so page 2 would
re-show or skip messages. An anchor names a specific message, so the page after
it is the same page whatever else changed. It stays legible for a model
composing the follow-up query too — the cursor is just an ID it has already seen.

```graphql
{
  emails(first: 25, after: "Mabc123") {
    totalCount
    pageInfo { hasNextPage endCursor }
    edges { cursor node { subject } }
  }
}
```

`last` without `before` becomes a negative `position`, which JMAP counts from
the end — "the last N" is one call and never needs a total. `last` with `before`
anchors backwards from the cursor.

The trade for stability is that a cursor can go stale: if its message is deleted
or stops matching the filter, JMAP returns `anchorNotFound`. That surfaces as an
error saying exactly that, and that the fix is to restart pagination. Compare
`queryState` between pages to detect that the result set moved underneath you.

`totalCount` maps to JMAP's `calculateTotal`, which costs the server real work,
so it's only requested when the field is actually selected. The same look-ahead
means a query selecting **only** `totalCount` performs one `Email/query` and
fetches no emails at all:

```graphql
# "How many unread from this sender?" — one call, zero emails transferred
{ emails(filter: { unread: true, from: "alerts@example.com" }) { totalCount } }
```

### Lazy fields and batching

Nothing below a list is fetched eagerly, and every lazy field goes through a
[DataLoader](https://github.com/graphql/dataloader): the resolvers for a list's
elements run concurrently, and their fetches collapse into **one batched API
call** rather than one per element.

| Selection on `emails(first: 25) { nodes { … } }` | JMAP calls |
| ----------------------------------------------- | ---------- |
| `{ totalCount }` alone | 1 (`Email/query`, no emails fetched) |
| `{ subject from { email } }` | 2 (`Email/query` + `Email/get`) |
| `{ subject textBody }` | 3 (+1 batched `Email/get` for all 25 bodies) |
| `{ subject textBody attachments { nodes { name } } }` | 3 (same batch covers both) |
| `{ … attachments { nodes { name cid size } } }` | 3 (metadata downloads nothing) |
| `{ … attachments { nodes { base64 } } }` | 3 + the blob downloads, issued concurrently |
| `{ … mailboxes { name } }` + `mailbox(…)` + a name filter | +1 `Mailbox/get` total, however many ask |

The naive shape — one detail call per email — would be 26. Loader caches are
per request, so referencing the same email or mailbox twice in one query costs
one fetch; nothing is retained between requests where it could go stale.

Because the graph contains cycles (`Email.thread.emails`, `Email.mailboxes.emails`),
nesting is capped at depth 15. The complexity budget is 100,000. Resolvers
declare a cost (document parsing costs more than downloading; nested lists
scale with page size). Reduce page sizes or split attachment-heavy requests
when a query exceeds the budget.

All operations are available as GraphQL queries and mutations: mailboxes, emails, search, threads, identities (with signatures), attachments (with text extraction and image resizing), contacts, masked email management, and send/reply/forward with the preview/confirm safety pattern.

`markAsSpam` also requires a preview. Request `confirmationToken` in the PREVIEW
result and pass it to CONFIRM after approval. Tokens expire after 15 minutes and
cannot be reused, transferred to another account, or applied to a different email.

One subscription, `emails`, streams arrivals over the same machinery as `fastmail watch`.

Token can be set via `FASTMAIL_API_TOKEN` env var or config file.

## Debug Logging

Enable debug output with `RUST_LOG`:

```bash
RUST_LOG=debug fastmail list mailboxes
```

## JMAP API

By default the CLI talks directly to Fastmail's JMAP server; `--server` routes
the same protocol operations through your HTTP server. Capabilities follow the
Fastmail token's permissions: read-only tokens support listing/reading, while
sending and masked email require the corresponding capabilities.

For more on JMAP: [jmap.io](https://jmap.io/)

## Releases

Pushes to `main` run checks and platform builds but do not publish. After a
version bump and successful CI, explicitly publish the current main revision:

```bash
gh workflow run ci.yml --ref main -f publish=true
```

The workflow refuses older versions and conflicting tags. It publishes the
GitHub release only after checks, all four platform builds, container uploads
and release asset uploads succeed. Archives include licenses; `SHA256SUMS`
covers all four archives.

## License

MIT
