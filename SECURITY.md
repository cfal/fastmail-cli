# Security

## Deployment

The HTTP server uses the Fastmail token and optional CardDAV credentials on the
server. Every permitted HTTP caller has access to that same account, including
destructive operations. HTTP Basic users are login identities, not separate
Fastmail accounts or permission scopes.

Authentication is optional. Without `--auth-file`, anyone who can reach the
server can use its credentials. Loopback binding restricts network exposure but
does not protect against other local processes or users. Use a trusted network,
firewall or authenticated reverse proxy as appropriate. Use HTTPS for remote
Basic authentication; Basic credentials are only encoded, not encrypted.

Protect both the Fastmail config and Basic auth file with private filesystem
permissions. Passwords in the auth file are plaintext. The server loads it at
startup; restart after editing it. Do not put secrets in URLs or query strings.
Loopback listeners enforce a localhost/loopback Host allowlist. Set
`--allowed-host` for custom hostnames or proxy deployments. Browser origins must
match the request Host; these checks are defense in depth, not authentication.

The CLI's `--server` mode sends all Fastmail requests through that server and
never falls back to local Fastmail credentials. Input attachments, downloaded
files, text extraction and user confirmations remain on the client.

## Resource Limits

HTTP JSON requests are limited to 2 MiB; attachment uploads and buffered upstream
responses to 64 MiB. Image decoding is limited to 16,384 pixels per dimension
and a 128 MiB allocation budget. SSE frames are limited to 1 MiB. GraphQL has a
depth limit of 15 and a complexity budget of 100,000; reduce page sizes for
attachment-heavy queries. These are not a sandbox or global memory quota.
Untrusted document parsing can still consume substantial CPU/memory. Prefer a
dedicated unprivileged account/container with OS resource limits for a shared
service. Extraction disables disk caching and OCR and requests a 60-second
cooperative timeout; this cannot forcibly terminate every parser.

## Dependency Dispositions

The September 2026 review found no evidence of deliberately malicious code in
the reviewed first-party source or targeted dependency/build inspection. This
is not proof that all dependency, action, toolchain or historical source is
free of malicious code.

PDF extraction now uses the published `xberg` 1.1.5 Rust-native PDF backend.
There is no vendored crate, PDFium runtime extraction, or unpinned PDFium build
download. GraphiQL assets are built from an npm integrity lockfile and embedded
locally, including workers; no runtime CDN scripts are loaded. Mailbox data and
HTTP headers are not persisted to browser storage.

Two scanner exceptions expire on 2026-12-13:

- **RUSTSEC-2026-0253 (`lru` 0.16.4):** the latest stable `async-graphql` 7.2.1
  constrains this dependency below the patched 0.18.2 version. The affected
  `LruCache::pop` path requires a panicking key destructor and recovery from
  unwinding. This application does not instantiate `LruCache` at all: record
  loaders use `HashMapCache`, and blobs use `NoCache`. The affected path is not
  reachable in this application. Reassess any cache changes. Avoiding an unused
  path does not fix the upstream crate for other consumers.
- **RUSTSEC-2024-0436 (`paste` 1.0.15):** unmaintained procedural macro dependency
  of `biblatex` through `xberg`. No exploitable defect or patched release is
  reported by this advisory. Its maintenance risk remains; track an upstream
  replacement rather than patching or vendoring unrelated parser code.

`osv-scanner.toml` records these specific, time-limited dispositions. All other
advisories fail CI. npm dependencies are audited without exceptions. Dependency
presence scans and these reachability assessments do not replace a complete
source audit or runtime isolation.
