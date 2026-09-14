# Contacts

Contacts use CardDAV, not the email API token. Direct access requires a Fastmail
username and a separate app password with contact access. Use configured values
or authorized secret sources; do not request or substitute the account password.

## Configuration

Store `[contacts].username` and `[contacts].app_password` in the existing private
`~/.config/fastmail-cli/config.toml`, preserving other sections, or export
`FASTMAIL_USERNAME` and `FASTMAIL_APP_PASSWORD`. Environment values override the
file. For an authorized app-password file, without persisting it in config:

```bash
export FASTMAIL_USERNAME="you@fastmail.com"
export FASTMAIL_APP_PASSWORD="$(tr -d '\r\n' < /secure/fastmail-app-password)"
```

Keep shell tracing off and do not print these values. In `--server` mode these
credentials belong on the server; client-side values are not forwarded.

## Lookup

```bash
fastmail contacts search "Alice Example"
fastmail contacts search "example.com"
fastmail contacts list
```

Both return `.data[]` contact records with `id`, `name`, `emails`, `phones`,
`organization`, `title`, and `notes`. Email entries have `email` and `label`;
phone entries have `number` and `label`. Optional fields can be null and lists
can be empty. A contact ID can be a UID or an opaque resource identifier; use it
unchanged.

Search is case-insensitive substring matching on name, email, or organization,
not phone or notes. It fetches address books and filters locally, so targeted
search reduces returned data but is not a server-side query optimization.
Review multiple matches and addresses before choosing a recipient or edit target.

## Authorized Edits

```bash
fastmail contacts create --name "Alice Example" --email alice@example.com
fastmail contacts update CONTACT_ID --title "Engineer"
fastmail contacts update CONTACT_ID --email "alice@example.com,alice@work.example"
fastmail contacts delete CONTACT_ID -y
```

Create requires `--name` and uses the first discovered address book. Create and
update accept `--name`, `--email`, `--phone`, `--organization`, `--title`, and
`--notes`; only supplied fields are changed on update.

Email and phone options take comma-separated strings and **replace** their
respective lists rather than append. Whitespace is trimmed, but empty comma
components are retained as empty entries; do not use empty strings as a
clear-list operation. Omit the flag to preserve the list. The CLI does not set
per-entry labels.

Unchanged and unsupported vCard properties are preserved. Replacing grouped
email/phone properties can leave companion label properties behind. Updates and
deletes require strong ETags; on a conflict, re-fetch and review the contact
before retrying rather than bypassing concurrency protection.

Create/update return the contact in `.data`. Delete returns a status message and
requires `-y`; without it, the command exits without deleting or prompting.
