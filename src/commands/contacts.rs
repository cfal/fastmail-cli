use crate::carddav::{CardDavClient, ContactEmail, ContactFields, ContactPhone};
use crate::config::Config;
use crate::models::Output;

fn make_carddav_client() -> anyhow::Result<CardDavClient> {
    if let Some(server) = crate::remote::HttpServer::current() {
        return Ok(CardDavClient::try_via_server(server)?);
    }
    let config = Config::load()?;
    let username = config.get_username()?;
    let app_password = config.get_app_password()?;
    Ok(CardDavClient::try_new(username, app_password)?)
}

/// List all contacts from all address books
pub async fn list_contacts() -> anyhow::Result<()> {
    let client = make_carddav_client()?;

    let addressbooks = client.list_addressbooks().await?;
    eprintln!("Found {} address book(s)", addressbooks.len());

    let mut all_contacts = Vec::new();
    for ab in &addressbooks {
        eprintln!("Fetching from: {}", ab.name);
        let contacts = client.list_contacts(&ab.href).await?;
        all_contacts.extend(contacts);
    }

    Output::success(all_contacts).print();
    Ok(())
}

/// Search contacts by name or email
pub async fn search_contacts(query: &str) -> anyhow::Result<()> {
    let client = make_carddav_client()?;
    let contacts = client.search_contacts(query).await?;

    Output::success(contacts).print();
    Ok(())
}

/// Create a new contact
pub async fn create_contact(
    name: &str,
    email: Option<&str>,
    phone: Option<&str>,
    organization: Option<&str>,
    title: Option<&str>,
    notes: Option<&str>,
) -> anyhow::Result<()> {
    let client = make_carddav_client()?;
    let emails = ContactEmail::parse_list(email);
    let phones = ContactPhone::parse_list(phone);

    let contact = client
        .create_contact(&ContactFields {
            name: Some(name),
            emails: Some(&emails),
            phones: Some(&phones),
            organization,
            title,
            notes,
        })
        .await?;

    Output::success(contact).print();
    Ok(())
}

/// Update an existing contact
pub async fn update_contact(
    contact_id: &str,
    name: Option<&str>,
    email: Option<&str>,
    phone: Option<&str>,
    organization: Option<&str>,
    title: Option<&str>,
    notes: Option<&str>,
) -> anyhow::Result<()> {
    let client = make_carddav_client()?;
    let emails = ContactEmail::parse_list(email);
    let phones = ContactPhone::parse_list(phone);

    let contact = client
        .update_contact(
            contact_id,
            &ContactFields {
                name,
                emails: (!emails.is_empty()).then_some(emails.as_slice()),
                phones: (!phones.is_empty()).then_some(phones.as_slice()),
                organization,
                title,
                notes,
            },
        )
        .await?;

    Output::success(contact).print();
    Ok(())
}

/// Delete a contact
pub async fn delete_contact(contact_id: &str) -> anyhow::Result<()> {
    let client = make_carddav_client()?;
    client.delete_contact(contact_id).await?;
    Output::<()>::success_msg(format!("Contact {contact_id} deleted.")).print();
    Ok(())
}
