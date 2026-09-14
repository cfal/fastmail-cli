//! CardDAV client for Fastmail contacts
//!
//! Uses raw HTTP with reqwest since CardDAV is just WebDAV with vCard.

use percent_encoding::{AsciiSet, CONTROLS, utf8_percent_encode};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tracing::{debug, instrument};

use crate::error::{Error, Result};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};

const CARDDAV_BASE: &str = "https://carddav.fastmail.com";

// Per RFC 3986, these chars need escaping when interpolating into a URL path
// segment. `/` is the segment delimiter and must be escaped to stay in-segment.
const PATH_SEGMENT: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'<')
    .add(b'>')
    .add(b'?')
    .add(b'`')
    .add(b'{')
    .add(b'}')
    .add(b'/')
    .add(b'%');

/// A contact parsed from vCard
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Contact {
    /// UID, or an opaque resource identifier when the card has no UID.
    pub id: String,
    /// Full name (FN property)
    pub name: String,
    /// Email addresses
    pub emails: Vec<ContactEmail>,
    /// Phone numbers
    pub phones: Vec<ContactPhone>,
    /// Organization/company
    pub organization: Option<String>,
    /// Job title
    pub title: Option<String>,
    /// Notes
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContactEmail {
    pub email: String,
    pub label: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContactPhone {
    pub number: String,
    pub label: Option<String>,
}

/// Address book info
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddressBook {
    pub href: String,
    pub name: String,
}

/// Fields for creating or updating a contact.
/// All fields are optional for updates (only provided fields are changed).
#[derive(Debug, Clone, Default, Serialize)]
pub struct ContactFields<'a> {
    pub name: Option<&'a str>,
    pub emails: Option<&'a [ContactEmail]>,
    pub phones: Option<&'a [ContactPhone]>,
    pub organization: Option<&'a str>,
    pub title: Option<&'a str>,
    pub notes: Option<&'a str>,
}

/// CardDAV client
pub struct CardDavClient {
    client: Client,
    username: String,
    app_password: String,
    server: Option<crate::remote::HttpServer>,
    base_url: String,
}

struct ContactResource {
    href: String,
    vcard: String,
    etag: String,
}

impl CardDavClient {
    /// Infallible compatibility constructor. Servers should use `try_new`.
    pub fn new(username: String, app_password: String) -> Self {
        Self::try_new(username, app_password).expect("Failed to build CardDAV client")
    }

    pub fn try_new(username: String, app_password: String) -> Result<Self> {
        Ok(Self {
            client: crate::util::http_client()?,
            username,
            app_password,
            server: None,
            base_url: CARDDAV_BASE.into(),
        })
    }

    pub fn try_via_server(server: crate::remote::HttpServer) -> Result<Self> {
        let mut client = Self::try_new(String::new(), String::new())?;
        client.server = Some(server);
        Ok(client)
    }

    pub fn via_server(server: crate::remote::HttpServer) -> Self {
        Self::try_via_server(server).expect("Failed to build CardDAV client")
    }

    fn resource_url(&self, href: &str) -> Result<reqwest::Url> {
        let base = reqwest::Url::parse(&self.base_url).unwrap();
        if !href.starts_with('/') && !href.starts_with(&format!("{}://", base.scheme())) {
            return Err(Error::Server("Invalid CardDAV resource URL".into()));
        }
        let url = base
            .join(href)
            .map_err(|_| Error::Server("Invalid CardDAV resource URL".into()))?;
        if url.origin() != base.origin()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
        {
            return Err(Error::Server(
                "CardDAV resource must stay on the server origin".into(),
            ));
        }
        Ok(url)
    }

    /// Discover address books for the user
    #[instrument(skip(self))]
    pub async fn list_addressbooks(&self) -> Result<Vec<AddressBook>> {
        if let Some(server) = &self.server {
            return server
                .contacts(serde_json::json!({"operation":"addressbooks"}))
                .await;
        }
        let encoded_user = utf8_percent_encode(&self.username, PATH_SEGMENT);
        let url = format!("{}/dav/addressbooks/user/{}/", self.base_url, encoded_user);

        let body = r#"<?xml version="1.0" encoding="utf-8"?>
<d:propfind xmlns:d="DAV:" xmlns:card="urn:ietf:params:xml:ns:carddav">
  <d:prop>
    <d:displayname/>
    <d:resourcetype/>
  </d:prop>
</d:propfind>"#;

        let response = self
            .client
            .request(reqwest::Method::from_bytes(b"PROPFIND").unwrap(), &url)
            .basic_auth(&self.username, Some(&self.app_password))
            .header("Content-Type", "application/xml")
            .header("Depth", "1")
            .body(body)
            .send()
            .await?;

        let status = response.status();
        let text = read_response(response).await?;

        debug!(status = %status, "PROPFIND response");

        if !status.is_success() && status.as_u16() != 207 {
            return Err(Error::Server(format!(
                "CardDAV PROPFIND failed: {} - {}",
                status, text
            )));
        }

        // Parse the multistatus XML response
        self.parse_addressbooks_response(&text)
    }

    fn parse_addressbooks_response(&self, xml: &str) -> Result<Vec<AddressBook>> {
        let doc = roxmltree::Document::parse(xml)
            .map_err(|e| Error::Server(format!("Failed to parse XML: {e}")))?;

        let dav_ns = "DAV:";
        let carddav_ns = "urn:ietf:params:xml:ns:carddav";
        let mut addressbooks = Vec::new();

        for response in doc
            .descendants()
            .filter(|n| n.has_tag_name((dav_ns, "response")))
        {
            let href = response
                .descendants()
                .find(|n| n.has_tag_name((dav_ns, "href")))
                .and_then(|n| n.text())
                .unwrap_or_default();

            // Check if this is an addressbook (has carddav:addressbook resourcetype)
            let is_addressbook = response
                .descendants()
                .any(|n| n.has_tag_name((carddav_ns, "addressbook")));

            if is_addressbook && !href.is_empty() {
                let displayname = response
                    .descendants()
                    .find(|n| n.has_tag_name((dav_ns, "displayname")))
                    .and_then(|n| n.text());

                let name = displayname.map(|s| s.to_string()).unwrap_or_else(|| {
                    href.split('/')
                        .rfind(|s| !s.is_empty())
                        .unwrap_or("Unknown")
                        .to_string()
                });

                // Skip the parent collection itself
                if !href.ends_with(&format!("{}/", self.username)) {
                    addressbooks.push(AddressBook {
                        href: href.to_string(),
                        name,
                    });
                }
            }
        }

        Ok(addressbooks)
    }

    /// List all contacts in an address book
    #[instrument(skip(self))]
    pub async fn list_contacts(&self, addressbook_href: &str) -> Result<Vec<Contact>> {
        if let Some(server) = &self.server {
            return server
                .contacts(serde_json::json!({"operation":"list", "href":addressbook_href}))
                .await;
        }
        let url = self.resource_url(addressbook_href)?;

        let body = r#"<?xml version="1.0" encoding="utf-8"?>
<card:addressbook-query xmlns:d="DAV:" xmlns:card="urn:ietf:params:xml:ns:carddav">
  <d:prop>
    <d:getetag/>
    <card:address-data/>
  </d:prop>
</card:addressbook-query>"#;

        let response = self
            .client
            .request(reqwest::Method::from_bytes(b"REPORT").unwrap(), url)
            .basic_auth(&self.username, Some(&self.app_password))
            .header("Content-Type", "application/xml")
            .header("Depth", "1")
            .body(body)
            .send()
            .await?;

        let status = response.status();
        let text = read_response(response).await?;

        debug!(status = %status, "REPORT response");

        if !status.is_success() && status.as_u16() != 207 {
            return Err(Error::Server(format!(
                "CardDAV REPORT failed: {} - {}",
                status, text
            )));
        }

        self.parse_contacts_response(&text)
    }

    fn parse_contacts_response(&self, xml: &str) -> Result<Vec<Contact>> {
        let doc = roxmltree::Document::parse(xml)
            .map_err(|e| Error::Server(format!("Failed to parse XML: {e}")))?;

        let dav_ns = "DAV:";
        let carddav_ns = "urn:ietf:params:xml:ns:carddav";
        let mut contacts = Vec::new();

        for response in doc
            .descendants()
            .filter(|n| n.has_tag_name((dav_ns, "response")))
        {
            if let Some(vcard_data) = dav_property(response, carddav_ns, "address-data") {
                let href = response
                    .descendants()
                    .find(|n| n.has_tag_name((dav_ns, "href")))
                    .and_then(|n| n.text())
                    .unwrap_or_default();
                if let Some(contact) = self.contact_at(vcard_data, href)? {
                    contacts.push(contact);
                }
            }
        }

        contacts.sort_by_key(|c| c.name.to_lowercase());
        Ok(contacts)
    }

    fn contact_at(&self, vcard: &str, href: &str) -> Result<Option<Contact>> {
        let Some(mut contact) = parse_vcard(vcard) else {
            return Ok(None);
        };
        if contact.id.is_empty() {
            let Ok(url) = self.resource_url(href) else {
                debug!("Skipping UID-less contact with an invalid resource URL");
                return Ok(None);
            };
            contact.id = format!("href:{}", URL_SAFE_NO_PAD.encode(url.as_str()));
        }
        Ok(Some(contact))
    }

    /// Search contacts by name or email
    pub async fn search_contacts(&self, query: &str) -> Result<Vec<Contact>> {
        // Get all contacts from all addressbooks and filter
        let addressbooks = self.list_addressbooks().await?;
        let mut all_contacts = Vec::new();

        for ab in addressbooks {
            let contacts = self.list_contacts(&ab.href).await?;
            all_contacts.extend(contacts);
        }

        let query_lower = query.to_lowercase();
        let filtered: Vec<Contact> = all_contacts
            .into_iter()
            .filter(|c| {
                c.name.to_lowercase().contains(&query_lower)
                    || c.emails
                        .iter()
                        .any(|e| e.email.to_lowercase().contains(&query_lower))
                    || c.organization
                        .as_ref()
                        .is_some_and(|o| o.to_lowercase().contains(&query_lower))
            })
            .collect();

        Ok(filtered)
    }

    /// Get the first (default) address book href
    async fn default_addressbook(&self) -> Result<String> {
        let addressbooks = self.list_addressbooks().await?;
        addressbooks
            .into_iter()
            .next()
            .map(|ab| ab.href)
            .ok_or(Error::Server("No address books found".to_string()))
    }

    async fn find_contact_href(&self, contact_id: &str) -> Result<Option<ContactResource>> {
        let addressbooks = self.list_addressbooks().await?;

        for ab in addressbooks {
            let url = self.resource_url(&ab.href)?;

            let body = r#"<?xml version="1.0" encoding="utf-8"?>
<card:addressbook-query xmlns:d="DAV:" xmlns:card="urn:ietf:params:xml:ns:carddav">
  <d:prop>
    <d:getetag/>
    <card:address-data/>
  </d:prop>
</card:addressbook-query>"#;

            let response = self
                .client
                .request(reqwest::Method::from_bytes(b"REPORT").unwrap(), url)
                .basic_auth(&self.username, Some(&self.app_password))
                .header("Content-Type", "application/xml")
                .header("Depth", "1")
                .body(body)
                .send()
                .await?;

            let status = response.status();
            let text = read_response(response).await?;

            if !status.is_success() && status.as_u16() != 207 {
                continue;
            }

            let doc = roxmltree::Document::parse(&text)
                .map_err(|e| Error::Server(format!("Failed to parse XML: {e}")))?;

            let dav_ns = "DAV:";
            let carddav_ns = "urn:ietf:params:xml:ns:carddav";

            for response in doc
                .descendants()
                .filter(|n| n.has_tag_name((dav_ns, "response")))
            {
                let href = response
                    .descendants()
                    .find(|n| n.has_tag_name((dav_ns, "href")))
                    .and_then(|n| n.text())
                    .unwrap_or_default();

                if let Some(vcard_data) = dav_property(response, carddav_ns, "address-data")
                    && self
                        .contact_at(vcard_data, href)?
                        .is_some_and(|c| c.id == contact_id)
                {
                    let etag = dav_property(response, dav_ns, "getetag")
                        .map(str::trim)
                        .filter(|value| value.starts_with('"') && value.ends_with('"'))
                        .ok_or_else(|| {
                            Error::Server(
                                "Contact has no strong ETag; refusing an unconditional mutation"
                                    .into(),
                            )
                        })?;
                    return Ok(Some(ContactResource {
                        href: href.into(),
                        vcard: vcard_data.into(),
                        etag: etag.into(),
                    }));
                }
            }
        }

        Ok(None)
    }

    /// Create a new contact. Returns the created Contact.
    /// `fields.name` is required for creation.
    #[instrument(skip(self, fields))]
    pub async fn create_contact(&self, fields: &ContactFields<'_>) -> Result<Contact> {
        if let Some(server) = &self.server {
            return server
                .contacts(serde_json::json!({"operation":"create", "fields":fields}))
                .await;
        }
        let name = fields.name.ok_or(Error::Server(
            "Name is required to create a contact".to_string(),
        ))?;
        let emails = fields.emails.unwrap_or(&[]);
        let phones = fields.phones.unwrap_or(&[]);

        let ab_href = self.default_addressbook().await?;
        let uid = generate_uid();
        let vcard = build_vcard(
            &uid,
            name,
            emails,
            phones,
            fields.organization,
            fields.title,
            fields.notes,
        );

        let url = self.resource_url(&format!("{ab_href}{uid}.vcf"))?;
        debug!(url = %url, "Creating contact");

        let response = self
            .client
            .put(url)
            .basic_auth(&self.username, Some(&self.app_password))
            .header("Content-Type", "text/vcard; charset=utf-8")
            .header("If-None-Match", "*")
            .body(vcard)
            .send()
            .await?;

        let status = response.status();
        let text = read_response(response).await?;

        if !status.is_success() && status.as_u16() != 201 && status.as_u16() != 204 {
            return Err(Error::Server(format!(
                "CardDAV PUT failed: {} - {}",
                status, text
            )));
        }

        Ok(Contact {
            id: uid,
            name: name.to_string(),
            emails: emails.to_vec(),
            phones: phones.to_vec(),
            organization: fields.organization.map(String::from),
            title: fields.title.map(String::from),
            notes: fields.notes.map(String::from),
        })
    }

    /// Update an existing contact. Merges provided fields with existing data.
    /// Returns the updated Contact.
    #[instrument(skip(self, fields))]
    pub async fn update_contact(
        &self,
        contact_id: &str,
        fields: &ContactFields<'_>,
    ) -> Result<Contact> {
        if let Some(server) = &self.server {
            return server
                .contacts(
                    serde_json::json!({"operation":"update", "id":contact_id, "fields":fields}),
                )
                .await;
        }
        let resource = self
            .find_contact_href(contact_id)
            .await?
            .ok_or_else(|| Error::Server(format!("Contact not found: {contact_id}")))?;

        let vcard = update_vcard(&resource.vcard, fields)?;
        let updated = self
            .contact_at(&vcard, &resource.href)?
            .ok_or_else(|| Error::Server("Updated contact must have a name".into()))?;
        let url = self.resource_url(&resource.href)?;
        debug!(url = %url, "Updating contact");

        let response = self
            .client
            .put(url)
            .basic_auth(&self.username, Some(&self.app_password))
            .header("Content-Type", "text/vcard; charset=utf-8")
            .header("If-Match", &resource.etag)
            .body(vcard)
            .send()
            .await?;

        let status = response.status();
        let text = read_response(response).await?;

        if status.as_u16() == 412 {
            return Err(Error::Server(
                "Contact changed concurrently; fetch it again before updating".into(),
            ));
        }
        if !status.is_success() && status.as_u16() != 204 {
            return Err(Error::Server(format!(
                "CardDAV PUT failed: {} - {}",
                status, text
            )));
        }

        Ok(updated)
    }

    /// Delete a contact by ID.
    #[instrument(skip(self))]
    pub async fn delete_contact(&self, contact_id: &str) -> Result<()> {
        if let Some(server) = &self.server {
            return server
                .contacts(serde_json::json!({"operation":"delete", "id":contact_id}))
                .await;
        }
        let resource = self
            .find_contact_href(contact_id)
            .await?
            .ok_or_else(|| Error::Server(format!("Contact not found: {contact_id}")))?;

        let url = self.resource_url(&resource.href)?;
        debug!(url = %url, "Deleting contact");

        let response = self
            .client
            .delete(url)
            .basic_auth(&self.username, Some(&self.app_password))
            .header("If-Match", &resource.etag)
            .send()
            .await?;

        let status = response.status();
        let text = read_response(response).await?;

        if status.as_u16() == 412 {
            return Err(Error::Server(
                "Contact changed concurrently; fetch it again before deleting".into(),
            ));
        }
        if !status.is_success() && status.as_u16() != 204 {
            return Err(Error::Server(format!(
                "CardDAV DELETE failed: {} - {}",
                status, text
            )));
        }

        Ok(())
    }
}

fn dav_property<'a, 'input>(
    response: roxmltree::Node<'a, 'input>,
    namespace: &str,
    name: &str,
) -> Option<&'a str> {
    for propstat in response
        .children()
        .filter(|n| n.has_tag_name(("DAV:", "propstat")))
    {
        let status = propstat
            .children()
            .find(|n| n.has_tag_name(("DAV:", "status")))
            .and_then(|n| n.text());
        if status.and_then(|s| s.split_ascii_whitespace().nth(1)) != Some("200") {
            continue;
        }
        if let Some(value) = propstat
            .children()
            .filter(|n| n.has_tag_name(("DAV:", "prop")))
            .flat_map(|n| n.children())
            .filter(|n| n.has_tag_name((namespace, name)))
            .filter_map(|n| n.text())
            .find(|value| !value.trim().is_empty())
        {
            return Some(value);
        }
    }
    None
}

async fn read_response(response: reqwest::Response) -> Result<String> {
    let bytes =
        crate::util::read_bounded_response(response, crate::util::MAX_ATTACHMENT_BYTES).await?;
    String::from_utf8(bytes).map_err(|_| Error::Server("Invalid UTF-8 in CardDAV response".into()))
}

/// Unfold vCard lines per RFC 6350 §3.2: continuation lines start with a space or tab.
fn unfold_vcard(raw: &str) -> String {
    let mut result = String::with_capacity(raw.len());
    for line in raw.lines() {
        if line.starts_with(' ') || line.starts_with('\t') {
            // Continuation line — append without the leading whitespace
            result.push_str(&line[1..]);
        } else {
            if !result.is_empty() {
                result.push('\n');
            }
            result.push_str(line);
        }
    }
    result
}

/// Decode quoted-printable encoded value (basic implementation for vCard)
fn decode_qp(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut decoded_bytes = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'=' && i + 2 < bytes.len() {
            if bytes[i + 1] == b'\r' || bytes[i + 1] == b'\n' {
                // Soft line break — skip
                i += 2;
                if i < bytes.len() && bytes[i] == b'\n' {
                    i += 1;
                }
            } else if let (Some(hi), Some(lo)) = (
                (bytes[i + 1] as char).to_digit(16),
                (bytes[i + 2] as char).to_digit(16),
            ) {
                decoded_bytes.push((hi * 16 + lo) as u8);
                i += 3;
            } else {
                decoded_bytes.push(b'=');
                i += 1;
            }
        } else {
            decoded_bytes.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(decoded_bytes)
        .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
}

fn escape_vcard_text(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .replace('\n', "\\n")
        .replace(';', "\\;")
        .replace(',', "\\,")
}

fn unescape_vcard_text(value: &str) -> String {
    let mut result = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            result.push(c);
            continue;
        }
        match chars.next() {
            Some('n' | 'N') => result.push('\n'),
            Some(c @ ('\\' | ';' | ',')) => result.push(c),
            Some(c) => {
                result.push('\\');
                result.push(c);
            }
            None => result.push('\\'),
        }
    }
    result
}

fn vcard_label(label: Option<&str>) -> String {
    match label.filter(|label| {
        !label.is_empty()
            && label
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == ',')
    }) {
        Some(label) => format!(";TYPE={label}"),
        None => String::new(),
    }
}

/// Parse a vCard string into a Contact
fn parse_vcard(vcard_str: &str) -> Option<Contact> {
    let unfolded = unfold_vcard(vcard_str);
    let mut id = String::new();
    let mut name = String::new();
    let mut emails = Vec::new();
    let mut phones = Vec::new();
    let mut organization = None;
    let mut title = None;
    let mut notes = None;

    for line in unfolded.lines() {
        let line = line.trim();

        // Extract property value, handling optional parameters and QP encoding
        let extract_value = |line: &str| -> String {
            let value = line.split_once(':').map(|(_, v)| v).unwrap_or("");
            if line.to_uppercase().contains("ENCODING=QUOTED-PRINTABLE") {
                decode_qp(value)
            } else {
                unescape_vcard_text(value)
            }
        };

        let property = property_name(line);
        if property.eq_ignore_ascii_case("UID") {
            id = extract_value(line);
        } else if property.eq_ignore_ascii_case("FN") {
            name = extract_value(line);
        } else if property.eq_ignore_ascii_case("EMAIL") {
            // EMAIL;TYPE=work:bob@example.com or EMAIL:bob@example.com
            let label = if line.contains("TYPE=") {
                line.split("TYPE=")
                    .nth(1)
                    .and_then(|s| s.split(':').next())
                    .map(|s| s.to_string())
            } else {
                None
            };
            let email = extract_value(line);
            if !email.is_empty() {
                emails.push(ContactEmail { email, label });
            }
        } else if property.eq_ignore_ascii_case("TEL") {
            let label = if line.contains("TYPE=") {
                line.split("TYPE=")
                    .nth(1)
                    .and_then(|s| s.split(':').next())
                    .or_else(|| line.split("TYPE=").nth(1).and_then(|s| s.split(';').next()))
                    .map(|s| s.to_string())
            } else {
                None
            };
            let number = extract_value(line);
            if !number.is_empty() {
                phones.push(ContactPhone { number, label });
            }
        } else if property.eq_ignore_ascii_case("ORG") {
            organization = Some(extract_value(line));
        } else if property.eq_ignore_ascii_case("TITLE") {
            title = Some(extract_value(line));
        } else if property.eq_ignore_ascii_case("NOTE") {
            notes = Some(extract_value(line));
        }
    }

    // Need at least a name
    if name.is_empty() {
        return None;
    }

    Some(Contact {
        id,
        name,
        emails,
        phones,
        organization,
        title,
        notes,
    })
}

fn property_name(line: &str) -> &str {
    line.split([';', ':'])
        .next()
        .unwrap_or_default()
        .rsplit('.')
        .next()
        .unwrap_or_default()
}

fn update_vcard(existing: &str, fields: &ContactFields<'_>) -> Result<String> {
    let changed = |name: &str| match name.to_ascii_uppercase().as_str() {
        "FN" | "N" => fields.name.is_some(),
        "EMAIL" => fields.emails.is_some(),
        "TEL" => fields.phones.is_some(),
        "ORG" => fields.organization.is_some(),
        "TITLE" => fields.title.is_some(),
        "NOTE" => fields.notes.is_some(),
        _ => false,
    };
    let replacements = build_vcard(
        "",
        fields.name.unwrap_or(""),
        fields.emails.unwrap_or(&[]),
        fields.phones.unwrap_or(&[]),
        fields.organization,
        fields.title,
        fields.notes,
    );
    // Keep untouched content lines, including their parameters, groups and folds.
    let mut lines: Vec<String> = Vec::new();
    for line in existing.split_inclusive('\n') {
        if line.starts_with([' ', '\t']) && !lines.is_empty() {
            lines.last_mut().unwrap().push_str(line);
        } else {
            lines.push(line.to_owned());
        }
    }
    let mut result = String::new();
    let mut ended = false;
    if !lines
        .first()
        .is_some_and(|line| line.trim_end().eq_ignore_ascii_case("BEGIN:VCARD"))
        || !lines
            .last()
            .is_some_and(|line| line.trim_end().eq_ignore_ascii_case("END:VCARD"))
        || lines
            .iter()
            .filter(|line| property_name(line).eq_ignore_ascii_case("BEGIN"))
            .count()
            != 1
        || lines
            .iter()
            .filter(|line| property_name(line).eq_ignore_ascii_case("END"))
            .count()
            != 1
    {
        return Err(Error::Server(
            "Expected one complete vCard for update".into(),
        ));
    }
    for line in lines {
        if line.trim_end().eq_ignore_ascii_case("END:VCARD") {
            for replacement in replacements
                .lines()
                .filter(|line| changed(property_name(line)))
            {
                result.push_str(replacement);
                result.push_str("\r\n");
            }
            ended = true;
        }
        if !changed(property_name(&line)) {
            result.push_str(&line);
        }
    }
    if !ended {
        return Err(Error::Server("Contact has no END:VCARD marker".into()));
    }
    Ok(result)
}

/// Generate a UUID-like UID for new contacts
fn generate_uid() -> String {
    use std::hash::{Hash, Hasher};
    use std::time::{SystemTime, UNIX_EPOCH};
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    // Mix timestamp nanos with a counter for uniqueness
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let count = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    now.as_nanos().hash(&mut hasher);
    count.hash(&mut hasher);
    let hi = hasher.finish();
    // Second hash with different seed for lower bits
    now.as_nanos()
        .wrapping_mul(6364136223846793005)
        .hash(&mut hasher);
    let lo = hasher.finish();
    format!(
        "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
        (hi >> 32) as u32,
        (hi >> 16) as u16,
        hi as u16,
        (lo >> 48) as u16,
        lo & 0xFFFF_FFFF_FFFF
    )
}

/// Build a vCard 3.0 string from contact fields
fn build_vcard(
    uid: &str,
    name: &str,
    emails: &[ContactEmail],
    phones: &[ContactPhone],
    organization: Option<&str>,
    title: Option<&str>,
    notes: Option<&str>,
) -> String {
    let mut lines = vec![
        "BEGIN:VCARD".to_string(),
        "VERSION:3.0".to_string(),
        format!("UID:{}", escape_vcard_text(uid)),
        format!("FN:{}", escape_vcard_text(name)),
    ];

    // N property — split FN into family/given (best-effort)
    let parts: Vec<&str> = name.splitn(2, ' ').collect();
    if parts.len() == 2 {
        lines.push(format!(
            "N:{};{};;;",
            escape_vcard_text(parts[1]),
            escape_vcard_text(parts[0])
        ));
    } else {
        lines.push(format!("N:{};;;;", escape_vcard_text(name)));
    }

    for email in emails {
        lines.push(format!(
            "EMAIL{}:{}",
            vcard_label(email.label.as_deref()),
            escape_vcard_text(&email.email)
        ));
    }

    for phone in phones {
        lines.push(format!(
            "TEL{}:{}",
            vcard_label(phone.label.as_deref()),
            escape_vcard_text(&phone.number)
        ));
    }

    if let Some(org) = organization {
        lines.push(format!("ORG:{}", escape_vcard_text(org)));
    }

    if let Some(t) = title {
        lines.push(format!("TITLE:{}", escape_vcard_text(t)));
    }

    if let Some(n) = notes {
        lines.push(format!("NOTE:{}", escape_vcard_text(n)));
    }

    lines.push("END:VCARD".to_string());
    lines.join("\r\n") + "\r\n"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resource_urls_cannot_redirect_credentials() {
        let client = CardDavClient::try_new("test".into(), "secret".into()).unwrap();
        for href in [
            "@evil.example/dav/",
            "//evil.example/dav/",
            "https://evil.example/",
            "/\\evil.example/",
            "https://user@carddav.fastmail.com/",
        ] {
            assert!(client.resource_url(href).is_err(), "{href}");
        }
        for href in [
            "/dav/contacts/",
            "https://carddav.fastmail.com/dav/contacts/",
        ] {
            assert_eq!(
                client.resource_url(href).unwrap().as_str(),
                "https://carddav.fastmail.com/dav/contacts/"
            );
        }
    }

    #[test]
    fn vcard_fields_cannot_inject_properties() {
        let name = "Name\r\nEMAIL:injected@example.com";
        let notes = "first\nsecond; value, \\n";
        let vcard = build_vcard(
            "id",
            name,
            &[ContactEmail {
                email: "real@example.com".into(),
                label: Some("work\r\nEMAIL:injected@example.com".into()),
            }],
            &[],
            Some("Company; division"),
            None,
            Some(notes),
        );
        assert_eq!(vcard.lines().filter(|l| l.starts_with("EMAIL")).count(), 1);
        let contact = parse_vcard(&vcard).unwrap();
        assert_eq!(contact.name, name.replace("\r\n", "\n"));
        assert_eq!(contact.notes.as_deref(), Some(notes));
        assert_eq!(contact.organization.as_deref(), Some("Company; division"));
        assert_eq!(contact.emails[0].email, "real@example.com");
    }

    #[test]
    fn test_unfold_vcard_lines() {
        // RFC 6350 §3.2: leading space/tab is the fold indicator and is consumed
        let input = "FN:John\n  Doe\nEMAIL:john@example.com";
        let result = unfold_vcard(input);
        assert_eq!(result, "FN:John Doe\nEMAIL:john@example.com");
    }

    #[test]
    fn test_unfold_tab_continuation() {
        let input = "FN:John\n\tDoe";
        let result = unfold_vcard(input);
        assert_eq!(result, "FN:JohnDoe");
    }

    #[test]
    fn test_decode_qp_basic() {
        assert_eq!(decode_qp("hello=20world"), "hello world");
        assert_eq!(decode_qp("caf=C3=A9"), "café");
    }

    #[test]
    fn test_decode_qp_soft_linebreak() {
        assert_eq!(decode_qp("hello=\nworld"), "helloworld");
    }

    #[test]
    fn test_parse_vcard_basic() {
        let vcard = "BEGIN:VCARD\nVERSION:3.0\nUID:abc123\nFN:Alice Smith\nEMAIL:alice@example.com\nEND:VCARD";
        let contact = parse_vcard(vcard).unwrap();
        assert_eq!(contact.id, "abc123");
        assert_eq!(contact.name, "Alice Smith");
        assert_eq!(contact.emails.len(), 1);
        assert_eq!(contact.emails[0].email, "alice@example.com");
    }

    #[test]
    fn test_parse_vcard_with_line_folding() {
        // Fold happens mid-value: "Very Long Name Here" folded after "Na"
        // Continuation line starts with space (fold indicator consumed)
        let vcard = "BEGIN:VCARD\nFN:Very Long Na\n me Here\nEMAIL:test@example.com\nEND:VCARD";
        let contact = parse_vcard(vcard).unwrap();
        assert_eq!(contact.name, "Very Long Name Here");
    }

    #[test]
    fn test_parse_vcard_with_params() {
        let vcard = "BEGIN:VCARD\nFN:Bob\nEMAIL;TYPE=work:bob@work.com\nTEL;TYPE=cell:+1234567890\nORG:Acme Inc\nTITLE:Engineer\nEND:VCARD";
        let contact = parse_vcard(vcard).unwrap();
        assert_eq!(contact.emails[0].email, "bob@work.com");
        assert_eq!(contact.emails[0].label, Some("work".to_string()));
        assert_eq!(contact.phones[0].number, "+1234567890");
        assert_eq!(contact.organization, Some("Acme Inc".to_string()));
        assert_eq!(contact.title, Some("Engineer".to_string()));
    }

    #[test]
    fn uidless_contacts_use_stable_distinct_resource_ids() {
        let vcard = "BEGIN:VCARD\nFN:No UID\nEND:VCARD";
        let client = CardDavClient::try_new("user".into(), "password".into()).unwrap();
        let a = client.contact_at(vcard, "/books/a.vcf").unwrap().unwrap();
        let b = client.contact_at(vcard, "/books/b.vcf").unwrap().unwrap();
        assert!(a.id.starts_with("href:"));
        assert_ne!(a.id, b.id);
        assert_eq!(
            a.id,
            client
                .contact_at(vcard, "https://carddav.fastmail.com/books/a.vcf")
                .unwrap()
                .unwrap()
                .id
        );
    }

    #[test]
    fn contact_updates_preserve_unrequested_content_lines() {
        let original = concat!(
            "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:contact\r\nFN:Original\r\nN:Family;Given;;;\r\n",
            "ADR;TYPE=home:;;Street;City;;;\r\nBDAY:2000-01-01\r\n",
            "PHOTO;ENCODING=b:YWJj\r\n ZGVm\r\nURL:https://example.test/\r\n",
            "item1.EMAIL;TYPE=home:person@example.test\r\nitem1.X-ABLabel:Custom\r\n",
            "X-CUSTOM:keep\r\nTITLE:Old\r\nEND:VCARD\r\n",
        );
        let updated = update_vcard(
            original,
            &ContactFields {
                title: Some("New"),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(updated, original.replace("TITLE:Old", "TITLE:New"));
        let renamed = update_vcard(
            original,
            &ContactFields {
                name: Some("New Name"),
                emails: Some(&[]),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(!renamed.contains("item1.EMAIL"));
        assert!(renamed.contains("FN:New Name\r\nN:Name;New;;;\r\n"));
        assert!(renamed.contains("item1.X-ABLabel:Custom\r\n"));
        assert!(renamed.contains("PHOTO;ENCODING=b:YWJj\r\n ZGVm\r\n"));
    }

    #[test]
    fn contact_updates_reject_incomplete_or_multiple_cards() {
        let fields = ContactFields {
            title: Some("Updated"),
            ..Default::default()
        };
        for original in [
            "BEGIN:OTHER\nFN:Name\nEND:VCARD\n",
            "BEGIN:VCARD\nFN:Name\n",
            "BEGIN:VCARD\nFN:Name\nEND:VCARD\nBEGIN:VCARD\nFN:Other\nEND:VCARD\n",
        ] {
            assert!(update_vcard(original, &fields).is_err());
        }
    }

    async fn contact_server(vcard: &str, etag: &str) -> (CardDavClient, wiremock::MockServer) {
        use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method};
        let server = MockServer::start().await;
        let mut client = CardDavClient::try_new("user".into(), "password".into()).unwrap();
        client.base_url = server.uri();
        Mock::given(method("PROPFIND")).respond_with(ResponseTemplate::new(207).set_body_string(
            r#"<d:multistatus xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:carddav"><d:response><d:href>/books/</d:href><d:propstat><d:prop><d:resourcetype><c:addressbook/></d:resourcetype></d:prop></d:propstat></d:response></d:multistatus>"#
        )).mount(&server).await;
        Mock::given(method("REPORT")).respond_with(ResponseTemplate::new(207).set_body_string(format!(
            r#"<d:multistatus xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:carddav"><d:response><d:href>/books/contact.vcf</d:href><d:propstat><d:prop><d:getetag>{etag}</d:getetag><c:address-data><![CDATA[{vcard}]]></c:address-data></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#
        ))).mount(&server).await;
        (client, server)
    }

    #[tokio::test]
    async fn contact_mutations_use_successful_properties_and_trim_etags() {
        use wiremock::{
            Mock, ResponseTemplate,
            matchers::{header, method},
        };
        let original = "BEGIN:VCARD\nUID:contact\nFN:Name\nEND:VCARD\n";
        let (client, server) = contact_server(original, "\"old\"").await;
        Mock::given(method("REPORT")).respond_with(
            ResponseTemplate::new(207).set_body_string(format!(
                r#"<d:multistatus xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:carddav"><d:response><d:href>/books/contact.vcf</d:href><d:propstat><d:prop><d:getetag/><c:address-data/></d:prop><d:status>HTTP/1.1 404 Not Found</d:status></d:propstat><d:propstat><d:prop><d:getetag>
                "version-1"
                </d:getetag><c:address-data><![CDATA[{original}]]></c:address-data></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#
            ))
        ).with_priority(1).mount(&server).await;
        for verb in ["PUT", "DELETE"] {
            Mock::given(method(verb))
                .and(header("If-Match", "\"version-1\""))
                .respond_with(ResponseTemplate::new(204))
                .expect(1)
                .mount(&server)
                .await;
        }
        client
            .update_contact(
                "contact",
                &ContactFields {
                    title: Some("New"),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        client.delete_contact("contact").await.unwrap();
    }

    #[test]
    fn bad_uidless_resources_do_not_abort_contact_listing() {
        let client = CardDavClient::try_new("user".into(), "password".into()).unwrap();
        let responses = ["contact.vcf", "", "/books/good.vcf"].into_iter().map(|href| format!(
            r#"<d:response><d:href>{href}</d:href><d:propstat><d:prop><c:address-data>BEGIN:VCARD
FN:Name
END:VCARD
</c:address-data></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>"#
        )).collect::<String>();
        let contacts = client.parse_contacts_response(&format!(
            r#"<d:multistatus xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:carddav">{responses}</d:multistatus>"#
        )).unwrap();
        assert_eq!(contacts.len(), 1);
        assert_eq!(
            contacts[0].id,
            client
                .contact_at("FN:Name", "/books/good.vcf")
                .unwrap()
                .unwrap()
                .id
        );
    }

    #[tokio::test]
    async fn uidless_resource_mutations_use_etags_and_preserve_data() {
        use wiremock::{
            Mock, ResponseTemplate,
            matchers::{header, method, path},
        };
        let original = "BEGIN:VCARD\nVERSION:3.0\nFN:Same Name\nBDAY:2000-01-01\nEND:VCARD\n";
        let (client, server) = contact_server(original, "\"version-1\"").await;
        for verb in ["PUT", "DELETE"] {
            Mock::given(method(verb))
                .and(path("/books/contact.vcf"))
                .and(header("If-Match", "\"version-1\""))
                .respond_with(ResponseTemplate::new(204))
                .expect(1)
                .mount(&server)
                .await;
        }
        let contact = client.list_contacts("/books/").await.unwrap().remove(0);
        let updated = client
            .update_contact(
                &contact.id,
                &ContactFields {
                    title: Some("Updated"),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(updated.id, contact.id);
        client.delete_contact(&contact.id).await.unwrap();
        let requests = server.received_requests().await.unwrap();
        let put = requests.iter().find(|r| r.method == "PUT").unwrap();
        let body = std::str::from_utf8(&put.body).unwrap();
        assert!(body.contains("BDAY:2000-01-01"));
        assert!(body.contains("TITLE:Updated"));
        assert!(!body.contains("UID:"));
    }

    #[tokio::test]
    async fn contact_mutations_report_conflicts_and_refuse_missing_etags() {
        use wiremock::{Mock, ResponseTemplate, matchers::method};
        let original = "BEGIN:VCARD\nUID:contact\nFN:Name\nEND:VCARD\n";
        let (client, server) = contact_server(original, "\"version-1\"").await;
        for verb in ["PUT", "DELETE"] {
            Mock::given(method(verb))
                .respond_with(ResponseTemplate::new(412))
                .expect(1)
                .mount(&server)
                .await;
        }
        let fields = ContactFields {
            title: Some("Updated"),
            ..Default::default()
        };
        assert!(
            client
                .update_contact("contact", &fields)
                .await
                .unwrap_err()
                .to_string()
                .contains("concurrently")
        );
        assert!(
            client
                .delete_contact("contact")
                .await
                .unwrap_err()
                .to_string()
                .contains("concurrently")
        );
        let (client, server) = contact_server(original, "").await;
        assert!(
            client
                .update_contact("contact", &fields)
                .await
                .unwrap_err()
                .to_string()
                .contains("ETag")
        );
        assert!(
            client
                .delete_contact("contact")
                .await
                .unwrap_err()
                .to_string()
                .contains("ETag")
        );
        assert!(
            !server
                .received_requests()
                .await
                .unwrap()
                .iter()
                .any(|r| r.method == "PUT" || r.method == "DELETE")
        );
    }

    #[tokio::test]
    async fn weak_etags_never_allow_unconditional_mutations() {
        let (client, server) =
            contact_server("BEGIN:VCARD\nUID:contact\nFN:Name\nEND:VCARD\n", "W/\"v1\"").await;
        assert!(
            client
                .delete_contact("contact")
                .await
                .unwrap_err()
                .to_string()
                .contains("strong ETag")
        );
        assert!(
            !server
                .received_requests()
                .await
                .unwrap()
                .iter()
                .any(|r| r.method == "DELETE")
        );
    }

    #[tokio::test]
    async fn shared_transport_keeps_carddav_credentials_request_scoped() {
        use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method};
        let server = MockServer::start().await;
        Mock::given(method("PROPFIND"))
            .respond_with(
                ResponseTemplate::new(207).set_body_string(r#"<d:multistatus xmlns:d="DAV:"/>"#),
            )
            .mount(&server)
            .await;
        for username in ["alice", "bob"] {
            let mut client = CardDavClient::try_new(username.into(), "password".into()).unwrap();
            client.base_url = server.uri();
            client.list_addressbooks().await.unwrap();
        }
        let requests = server.received_requests().await.unwrap();
        assert_eq!(
            requests[0].headers["authorization"],
            "Basic YWxpY2U6cGFzc3dvcmQ="
        );
        assert_eq!(
            requests[1].headers["authorization"],
            "Basic Ym9iOnBhc3N3b3Jk"
        );
    }

    #[test]
    fn test_parse_vcard_returns_none_without_name() {
        let vcard = "BEGIN:VCARD\nUID:abc\nEMAIL:test@example.com\nEND:VCARD";
        assert!(parse_vcard(vcard).is_none());
    }

    #[test]
    fn test_build_vcard_basic() {
        let vcard = build_vcard(
            "test-uid-123",
            "Jane Doe",
            &[ContactEmail {
                email: "jane@example.com".to_string(),
                label: None,
            }],
            &[],
            Some("Acme Corp"),
            None,
            None,
        );
        assert!(vcard.contains("BEGIN:VCARD"));
        assert!(vcard.contains("VERSION:3.0"));
        assert!(vcard.contains("UID:test-uid-123"));
        assert!(vcard.contains("FN:Jane Doe"));
        assert!(vcard.contains("N:Doe;Jane;;;"));
        assert!(vcard.contains("EMAIL:jane@example.com"));
        assert!(vcard.contains("ORG:Acme Corp"));
        assert!(vcard.contains("END:VCARD"));
    }

    #[test]
    fn test_build_vcard_with_labels() {
        let vcard = build_vcard(
            "uid-456",
            "Bob",
            &[ContactEmail {
                email: "bob@work.com".to_string(),
                label: Some("work".to_string()),
            }],
            &[ContactPhone {
                number: "+1234567890".to_string(),
                label: Some("cell".to_string()),
            }],
            None,
            Some("Engineer"),
            Some("A note"),
        );
        assert!(vcard.contains("EMAIL;TYPE=work:bob@work.com"));
        assert!(vcard.contains("TEL;TYPE=cell:+1234567890"));
        assert!(vcard.contains("N:Bob;;;;"));
        assert!(vcard.contains("TITLE:Engineer"));
        assert!(vcard.contains("NOTE:A note"));
    }

    #[test]
    fn test_build_vcard_roundtrips() {
        let vcard = build_vcard(
            "roundtrip-uid",
            "Alice Smith",
            &[
                ContactEmail {
                    email: "alice@home.com".to_string(),
                    label: Some("home".to_string()),
                },
                ContactEmail {
                    email: "alice@work.com".to_string(),
                    label: Some("work".to_string()),
                },
            ],
            &[ContactPhone {
                number: "+9876543210".to_string(),
                label: None,
            }],
            Some("Widgets Inc"),
            Some("CEO"),
            Some("Important person"),
        );

        // parse_vcard expects \n line endings, build_vcard uses \r\n
        let unix_vcard = vcard.replace("\r\n", "\n");
        let contact = parse_vcard(&unix_vcard).expect("Should parse built vcard");
        assert_eq!(contact.id, "roundtrip-uid");
        assert_eq!(contact.name, "Alice Smith");
        assert_eq!(contact.emails.len(), 2);
        assert_eq!(contact.emails[0].email, "alice@home.com");
        assert_eq!(contact.emails[0].label, Some("home".to_string()));
        assert_eq!(contact.phones.len(), 1);
        assert_eq!(contact.phones[0].number, "+9876543210");
        assert_eq!(contact.organization, Some("Widgets Inc".to_string()));
        assert_eq!(contact.title, Some("CEO".to_string()));
        assert_eq!(contact.notes, Some("Important person".to_string()));
    }

    #[test]
    fn test_generate_uid_unique() {
        let uid1 = generate_uid();
        let uid2 = generate_uid();
        assert_ne!(uid1, uid2);
        // Should look UUID-ish
        assert_eq!(uid1.matches('-').count(), 4);
    }
}
