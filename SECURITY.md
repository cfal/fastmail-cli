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
Basic login has no built-in attempt throttling. On reachable listeners, use strong
unique passwords and a rate-limiting reverse proxy for online-guessing resistance.

Protect both the Fastmail config and Basic auth file with private filesystem
permissions. Passwords in the auth file are plaintext. The server loads it at
startup; restart after editing it. Do not put Fastmail tokens in URLs or query
strings.
Loopback listeners enforce a localhost/loopback Host allowlist. Set
`--allowed-host` for custom hostnames or proxy deployments. Browser origins must
match the request Host; these checks are defense in depth, not authentication.

The CLI's `--server` mode sends all Fastmail requests through that server and
never falls back to local Fastmail credentials. Input attachments, downloaded
files, text extraction and user confirmations remain on the client.

For optional CLI Basic auth, use either a secret `FASTMAIL_SERVER` URL such as
`https://user:pass@server.example:8443`, or the separate `FASTMAIL_SERVER_USER`
and `FASTMAIL_SERVER_PASSWORD` settings. Mixed credential sources are rejected.
URL credentials are percent-decoded and removed before constructing request URLs;
the authorization header is marked sensitive, and CLI help hides server URL
and username environment values. Do not pass a credential-bearing URL as a
command-line argument or expose it in logs, shell history, or query strings.
Environment variables still require protection from other processes with access
to them.

## Saved Draft Operations

GraphQL/MCP `sendDraft` requires a one-shot account/operation/Email-ID/identity
approval and submits that immutable Email directly, without MIME reconstruction.
It refuses incomplete text previews, missing header metadata, repeated routing
headers (including Reply-To and Subject), and drafts in Trash, and rechecks these
at confirmation. Mailboxes are
resolved strictly by role, not display name; those IDs are also approval-bound.
Starting a submission invalidates all outstanding approvals for the same payload,
including on ambiguous failure. New explicit previews can still authorize a
repeat send; this is not a durable submission deduplication service.
JMAP has no atomic Email-state precondition on EmailSubmission creation: a
different client can change draft classification or submit it after that check.
Post-submit filing patches only Drafts/Sent membership and `$draft`/`$seen`;
it is not conditional on Email state. Filing errors retain positive submission
evidence and return a warning, not a reason to send again. Transport errors can
have ambiguous outcomes and are never retried automatically.

`deleteDraft` means a recoverable move to Trash, with no confirmation token,
like `moveEmail`. It requires a fresh draft snapshot and a matching account-wide
Email state at the write; even unrelated arrivals can reject the operation.
Neither operation permanently deletes or replaces message content.

## Resource Limits

HTTP JSON requests are limited to 2 MiB; attachment uploads and buffered upstream
responses to 64 MiB. Local `--html-file` input is also limited to 64 MiB by a
bounded read. Image decoding is limited to 16,384 pixels per dimension
and a 128 MiB allocation budget. Image conversion rejects budgets below 512 bytes
and stops after at most four resize passes, reusing reduced images. SSE frames are
limited to 1 MiB. GraphQL has a
depth limit of 15 and a complexity budget of 100,000; reduce page sizes for
attachment-heavy queries. Before parsing, query documents are limited to 64 KiB
and 32 levels of syntax nesting, including input values. Before recursive
validation, fragment expansion is limited to 10,000 selections across operations
and fragment definitions, with at most 32 levels of expanded nesting. Use variables
for large message bodies. These are not a sandbox or global memory quota.
Untrusted document parsing can still consume substantial CPU/memory. Prefer a
dedicated unprivileged account/container with OS resource limits for a shared
service. Extraction disables disk caching and OCR. The CLI's best-effort
extractor requests a 60-second cooperative timeout; this cannot forcibly
terminate every parser.

GraphQL attachment payloads share a per-operation scope: at most 128 MiB of
streamed blob input, 256 MiB of serialized attachment strings (including JSON
escaping), 1,024 unique blob IDs, and four concurrent downloads. The existing
64 MiB per-blob limit still applies. Aliases share a download and cached failures,
but each returned payload counts against the output budget. Partial failed or
cancelled downloads remain charged. Each subscription emission gets a fresh
scope; work already started retains its original scope.

Base64 encoding, image conversion, and document extraction share two process-wide
blocking-worker slots. Cancellation does not release a slot while its job is
still running. GraphQL extraction disables xberg's early-return timeout so that
the slot remains held until extraction finishes. It requests plain text, disables
recursive embedded-document extraction, caps archive input at 64 MiB and 1,024
entries, pages at 1,000, cells at 100,000, and applies explicit parser limits.
Returned extracted text is capped at 64 MiB of JSON-serialized text, or the
remaining payload allowance when smaller; resource/validation errors are visible.
These limits preserve the supported formats, but some xberg backends materialize
documents before checking their limits. They do not provide a hard bound on
parser allocations, CPU time, total process memory, or the complete HTTP response.
Raw bytes, transformed data and transport serialization may coexist. Custom
library schemas must use `graphql::request` to install this request accounting.

GraphQL/MCP compose inputs allow at most 128 attachments and 25 MiB of combined
decoded bytes, including existing blobs. Names and MIME types are bounded to
255 bytes and reject control characters; MIME types require concrete
`type/subtype` syntax without parameters or multipart types. Blob IDs use the
JMAP Id alphabet.
The existing 2 MiB HTTP JSON limit also applies to inline base64 and
`uploadAttachment`; this permits less than roughly 1.5 MiB of new raw bytes per
HTTP request. Query literals retain the separate 64 KiB document limit.
PREVIEW measures blobs by bounded streaming reads without retaining their bytes;
`Content-Length` can reject an oversized blob early but cannot approve it alone.
Ordered names/types and SHA-256 byte digests or immutable blob IDs are bound to
the existing account/operation-specific one-shot approval. No compose uploads or
mail creation happen before confirmation. The explicit `uploadAttachment`
mutation is a separate write and does not require a compose approval.

Saved-draft previews allow at most 128 listed body/attachment parts and 1 MiB of
formatted preview text, and request at most 1 MiB per text body value. Missing,
truncated, or encoding-problem text values prevent approval. Attachments and
non-text inline parts are represented by metadata, not downloaded or inspected.
The preview output cap is checked after formatting; upstream response bounds
remain the separate limit on fetched data and intermediate allocations.

Readable message bodies use local HTML-to-Markdown/text conversion, not document
ingestion or a browser. They never fetch linked resources, CID images, data-URL
images, or stylesheets, and never execute scripts. Image source URLs are omitted
from image output; alt text and placeholders remain. Original bodies are retained.
This is not HTML sanitization or a prompt-injection defense. Treat converted
content, link destinations, and raw messages as untrusted; downstream renderers
must disable raw HTML and automatic resource loading.

Each derived body processes at most 128 selected parts and 1 MiB of input, emits
at most 1 MiB of content, and limits HTML traversal to 64 levels. Missing parts,
conversion failures, encoding problems, and truncation are reported rather than
silently presented as complete content. These are not limits on the retained raw
message. GraphQL runs conversions off its async workers with at most two running
at once per process. Output limits apply after conversion; parser allocations
and CPU time are not hard-bounded by the output limit. Use OS resource limits
for hostile mail, as for other document parsing.

Caller-managed Email change batches are ID-only, account-wide reads. Aggregation
fails without a checkpoint above 1,000 pages, 100,000 returned ID entries, or
16 MiB of accumulated ID/state strings; upstream response limits still apply
separately. These are not global memory or wall-clock limits. No cursor is saved
or silently reset. Callers must validate the account/server, atomically persist
pending IDs with their checkpoint, and handle unavailable change history with
backfill. Existing in-memory watchers remain non-resumable across process restarts.

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

Reassessed on 2026-10-17: async-graphql 7.2.1 and 8.0.0-rc.5 both constrain
`lru` to `^0.16.2`, below patched 0.18.2. xberg 1.3.10 still enables biblatex
0.12 with its `office` feature, and biblatex still depends on `paste`. Neither
has a registry-only upgrade that clears its exception while preserving these
features. Track upstream constraint/macro migrations before the existing expiry;
do not widen the exceptions or upgrade to a prerelease merely to suppress them.
