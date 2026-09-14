# v4.0.2

## HTTP Client Authentication

- Supply optional HTTP Basic credentials in one `FASTMAIL_SERVER` URL, using
  percent-encoded username and password values. The separate `--server-user` /
  `FASTMAIL_SERVER_USER` and `FASTMAIL_SERVER_PASSWORD` settings remain supported.
- Strip URL credentials before constructing requests, keep authorization headers
  sensitive and request-scoped, and hide server URL and username environment
  values in CLI help.
- Reject empty or malformed URL logins, raw URL control characters, and mixed
  credential sources. Unset unused credential variables; empty values still
  count as supplied. Encode reserved characters, including literal `%` as `%25`.
- Expand tests for encoding, URL normalization, redaction, conflicting settings,
  authenticated mail/contact commands, and redirect rejection.

Authentication remains optional. HTTP callers still use the server-owned Fastmail
account, with no direct fallback to local credentials. Use HTTPS for remote Basic
authentication and protect credential-bearing URLs as secrets.

## Builds And Agent Support

- Enable fat LTO and optimization level 3 for release builds, retaining symbol
  stripping.
- Move and clean up the portable skill at `skills/fastmail-cli`.
- Add repository working instructions in `AGENTS.md` and a `CLAUDE.md` symlink.

Four platform archives include licenses; `SHA256SUMS` covers all four archives.
The release also publishes Linux amd64/arm64 container manifests.
