---
name: fastmail/contacts
description: fastmail contacts — CardDAV setup, search and editing
---

# fastmail-cli — Contacts

Contacts use CardDAV and require separate credentials from the JMAP API token.

## Configuration

```toml
# ~/.config/fastmail-cli/config.toml
[contacts]
username = "you@fastmail.com"
app_password = "your-app-password"
```

Or via env:
```bash
FASTMAIL_USERNAME="you@fastmail.com"
FASTMAIL_APP_PASSWORD="your-app-password"
```

Generate an app password at: Fastmail Settings → Privacy & Security → App Passwords

## Commands

```bash
# List all contacts
fastmail contacts list

# Search by name, email, or organization
fastmail contacts search "Alice"
fastmail contacts search "acme.com"
fastmail contacts search "ACME Corp"

fastmail contacts create --name "Alice Example" --email alice@example.com
fastmail contacts update CONTACT_ID --title "Engineer"
fastmail contacts delete CONTACT_ID -y
```

## Typical Patterns

```bash
# Find email address before composing
fastmail contacts search "Bob Smith" | jq -r '.data[0].emails[0].email'

# Verify who someone is before replying
fastmail contacts search "bob@unknown.com"

# Find all contacts at a company
fastmail contacts search "bigcorp.com"
```

## Notes

- `contacts list` returns all contacts — can be large. Prefer `contacts search` for targeted lookups.
- Contact data includes name, emails, phone numbers, organization, and notes where available.
- Create/update accept `--name`, `--email`, `--phone`, `--organization`, `--title`
  and `--notes`. Only create requires a name. Email and phone flags replace the
  corresponding lists; omitted fields and unsupported vCard properties are preserved.
  Group companion properties (such as custom labels) are preserved too, so a
  replaced grouped email/phone can leave a label without its former member.
- Updates/deletes use ETags. On a concurrent-edit conflict, fetch and review the
  contact again before retrying. `delete -y` is required; without `-y` the command
  exits without deleting, rather than prompting interactively.
- In `--server` mode, CardDAV credentials belong on the server, not the CLI client.
