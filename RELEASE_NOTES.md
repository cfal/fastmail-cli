# v4.0.0

## HTTP Client And Authentication

- `fastmail --server URL` routes mail, mailboxes, identities, masked email,
  contacts, attachments and watch requests through the server. No Fastmail
  credentials are needed on the client, and there is no direct fallback.
- HTTP Basic authentication is optional. Start the server with
  `--auth-file users.toml`, containing a `[users]` table of username/password
  strings. All users access the same server-configured Fastmail account.
- Clients can set `FASTMAIL_SERVER`, `FASTMAIL_SERVER_USER` and
  `FASTMAIL_SERVER_PASSWORD`. Use HTTPS for remote Basic authentication.
- **Breaking:** `X-Fastmail-Token`, `X-Fastmail-Username` and
  `X-Fastmail-App-Password` overrides are removed. Configure credentials on the
  server instead. Local direct CLI mode and stdio MCP remain available.
- Authentication remains optional even on non-loopback listeners. Without it,
  every reachable caller can use the server's mailbox credentials. Configure
  listener addresses, firewalls and trusted proxies accordingly.

## Security Fixes

- Replace bundled PDFium with the published xberg Rust-native PDF backend.
  No vendoring, shared temporary library loading or unpinned PDFium download.
- Use request-scoped CardDAV credentials consistently, escape vCard values,
  validate resource origins and refuse credential-bearing redirects.
- Bind compose approval tokens to the operation, account, complete recipient
  list, resolved sender, text, HTML and relevant original-message content.
- Bound attachment/response sizes, image decoding, SSE frames and GraphQL
  complexity. Malformed addresses and long UTF-8 filenames no longer panic.
- Serve GraphiQL scripts, styles, fonts and workers locally under a restrictive
  content security policy. Do not persist mailbox data in browser storage.
- Update dependencies and add automated scans. Two time-limited advisory
  dispositions remain documented in `SECURITY.md`: an unused lru cache path
  and the unmaintained paste macro dependency.

## Distribution

Four platform archives include licenses; `SHA256SUMS` covers the archives.
Container bases and the Rust toolchain are pinned; containers run unprivileged.
Publishing now requires an explicit CI dispatch with `publish=true`. A draft
release is published only after all checks, builds and asset uploads succeed.
