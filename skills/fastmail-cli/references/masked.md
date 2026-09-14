# Masked Addresses

These commands use Fastmail's JMAP masked-email capability and the email API
token, not CardDAV credentials. If the account/token lacks the capability, report
that limitation rather than switching credentials or promising the operation.

## Inspect

```bash
fastmail masked list
```

The result is `.data[]` with `id`, `email`, and optional metadata such as `state`,
`forDomain`, `description`, `createdAt`, and `lastMessageAt`. States can include
`pending`, `enabled`, `disabled`, and `deleted`. Use the returned **ID**, not the
address string, for state changes.

## Create When Requested

```bash
fastmail masked create --domain "https://example.com" \
  --description "Example site signup" --prefix "example_shop"
```

All create flags are optional. The command requests an enabled address and
returns the created record in `.data`; use its returned email rather than
constructing one from the prefix.

`--domain` becomes `forDomain` metadata describing the site, not the domain of the
generated mailbox. `--description` is a label. `--prefix` is passed to Fastmail as
`emailPrefix`; CLI help describes a maximum of 64 characters using lowercase
letters, digits, and underscores. The server validates it, so handle rejection
rather than assuming arbitrary strings will work.

## Authorized State Changes

```bash
fastmail masked disable MASKED_ID
fastmail masked enable MASKED_ID
fastmail masked delete MASKED_ID -y
```

These request the `disabled`, `enabled`, or `deleted` state and return a status
message. Delete refuses without `-y`, without an interactive prompt. Do not
auto-disable an address merely because a search found unwanted mail. Verify the
target and requested action, and inspect the resulting state when needed.

The CLI does not define server delivery, bounce, retention, or recoverability
policies. Do not promise that deleting an address erases past mail, or that a
deleted address can be re-enabled.
