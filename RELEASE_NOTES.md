# v4.0.4

## Readable Email Bodies

- Full CLI reads (`get`, `thread`, and `watch --full`) now include `readableBody`
  alongside unchanged JMAP body values. Agents can read HTML-only messages
  without building their own HTML extractor.
- Choose `--body-format auto|markdown|text|raw`. Automatic mode prefers genuine
  plain text and converts actual HTML parts to Markdown. Markdown mode prefers
  the HTML alternative; plain-text parts use literal blocks to preserve layout.
- Select `readableBody(format: AUTO|MARKDOWN|TEXT)` on GraphQL/MCP emails,
  including connections, threads, and subscription arrivals. It shares the lazy
  detail fetch, and queued conversions share body inputs rather than deep-cloning
  them per field. Existing public Rust loader APIs remain available.

## Fidelity And Safety

- Use `html-to-markdown-rs` locally, preserving selected JMAP part order without
  combining alternative representations or trimming quoted conversations.
- Return source-part metadata, truncation and encoding flags, and fidelity
  warnings. Conversion is best-effort, limited to 128 parts, 1 MiB of selected
  input, 1 MiB of returned content, and 64 HTML traversal levels. These limits do
  not bound upstream body fetching or every intermediate parser allocation.
- Replace images and embedded media with escaped alt text or omission notices.
  No remote, CID, or data-URL resource is loaded, and no scripts execute. Text
  mode places SVG/MathML omission notices at the end of the body part.
- Keep original bodies available for inspection. Derived content remains
  untrusted; conversion is neither a browser rendering nor a security sanitizer.
- Write diagnostic logs to stderr so stdout remains valid JSON/NDJSON.

Four platform archives include licenses; `SHA256SUMS` covers all four archives.
The release also publishes Linux amd64/arm64 container manifests.
