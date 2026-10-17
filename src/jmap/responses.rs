use super::method_error;
use crate::error::{Error, Result};
use serde::de::DeserializeOwned;
use serde_json::Value;

pub(super) fn invalid(method: &str, description: &str) -> Error {
    let description = if method.ends_with("/set") {
        format!("{description}. Write outcome unknown; do not retry automatically")
    } else {
        description.to_owned()
    };
    Error::Jmap {
        method: method.into(),
        error_type: "parse".into(),
        description,
    }
}

fn tuple(value: &Value) -> Option<(&str, &Value, &str)> {
    let array = value.as_array()?;
    if array.len() != 3 || !array[1].is_object() {
        return None;
    }
    Some((array[0].as_str()?, &array[1], array[2].as_str()?))
}

pub(super) fn method_response<'a>(
    responses: &'a [Value],
    method: &str,
    call_id: &str,
    account: &str,
) -> Result<&'a Value> {
    let mut selected = None;
    for response in responses {
        let (name, data, id) =
            tuple(response).ok_or_else(|| invalid(method, "Malformed method response"))?;
        if id == call_id
            && (name == method || name == "error")
            && selected.replace((name, data)).is_some()
        {
            return Err(invalid(method, "Conflicting method responses"));
        }
    }
    match selected {
        Some(("error", data)) => Err(method_error(method, data, "No description")),
        Some((_, data)) if data["accountId"] == account => Ok(data),
        Some(_) => Err(invalid(method, "Account mismatch")),
        None => Err(invalid(method, "Missing method response or wrong call ID")),
    }
}

pub(super) fn parse<T: DeserializeOwned>(
    responses: &[Value],
    method: &str,
    call_id: &str,
    account: &str,
) -> Result<T> {
    serde_json::from_value(method_response(responses, method, call_id, account)?.clone())
        .map_err(|error| invalid(method, &error.to_string()))
}

pub(super) fn get<T: DeserializeOwned>(
    responses: &[Value],
    method: &str,
    call_id: &str,
    account: &str,
    ids: Option<&[String]>,
) -> Result<T> {
    let data = method_response(responses, method, call_id, account)?;
    if !data["state"].is_string() {
        return Err(invalid(method, "Missing or malformed state"));
    }
    let list = data["list"]
        .as_array()
        .ok_or_else(|| invalid(method, "Missing list"))?;
    let not_found = data["notFound"]
        .as_array()
        .ok_or_else(|| invalid(method, "Missing notFound"))?;
    let mut seen = std::collections::HashSet::new();
    for value in list
        .iter()
        .map(|value| &value["id"])
        .chain(not_found.iter())
    {
        let id = value
            .as_str()
            .filter(|id| !id.is_empty())
            .ok_or_else(|| invalid(method, "Invalid result ID"))?;
        if !seen.insert(id) {
            return Err(invalid(method, "Duplicate or conflicting result IDs"));
        }
    }
    if let Some(ids) = ids {
        let expected: std::collections::HashSet<_> = ids.iter().map(String::as_str).collect();
        if seen != expected {
            return Err(invalid(method, "Incomplete or unexpected result IDs"));
        }
    } else if !not_found.is_empty() {
        return Err(invalid(method, "Unexpected notFound IDs"));
    }
    serde_json::from_value(data.clone()).map_err(|error| invalid(method, &error.to_string()))
}

fn map_entry<'a>(
    data: &'a Value,
    field: &str,
    key: &str,
    method: &str,
) -> Result<Option<&'a Value>> {
    match data.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Object(map)) => Ok(map.get(key)),
        Some(_) => Err(invalid(method, &format!("Malformed {field} map"))),
    }
}

fn positive<'a>(
    data: &'a Value,
    method: &str,
    key: &str,
    yes: &str,
    no: &str,
) -> Result<&'a Value> {
    match (
        map_entry(data, yes, key, method)?,
        map_entry(data, no, key, method)?,
    ) {
        (Some(_), Some(_)) => Err(invalid(method, "Conflicting success and failure evidence")),
        (None, Some(error)) => Err(method_error(method, error, "Write failed")),
        (Some(value), None) => Ok(value),
        (None, None) => Err(invalid(method, "No positive write evidence returned")),
    }
}

pub(super) fn require_created_id(data: &Value, method: &str, key: &str) -> Result<String> {
    positive(data, method, key, "created", "notCreated")?["id"]
        .as_str()
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| invalid(method, "No successful creation ID returned"))
}

pub(super) fn require_updated(data: &Value, method: &str, id: &str) -> Result<()> {
    let value = positive(data, method, id, "updated", "notUpdated")?;
    if value.is_null() || value.is_object() {
        Ok(())
    } else {
        Err(invalid(method, "Malformed updated entry"))
    }
}

pub(super) fn submission_response(
    responses: &[Value],
    call_id: &str,
    account: &str,
    email_id: &str,
) -> Result<(String, Option<String>)> {
    let method = "EmailSubmission/set";
    let primary: Vec<_> = responses
        .iter()
        .filter(|value| value[0] == method && value[2] == call_id)
        .cloned()
        .collect();
    if primary.is_empty() {
        method_response(responses, method, call_id, account)?;
        return Err(invalid(method, "Missing submission response"));
    }
    // A same-call-ID error may describe implicit filing, but only after positive
    // submission evidence. Never turn accepted delivery into a retryable failure.
    let data = method_response(&primary, method, call_id, account)?;
    let id = require_created_id(data, method, "submission")?;
    let filing: Vec<_> = responses
        .iter()
        .filter(|value| !(value[0] == method && value[2] == call_id))
        .cloned()
        .collect();
    let warning = method_response(&filing, "Email/set", call_id, account)
        .and_then(|data| require_updated(data, "Email/set", email_id))
        .err()
        .map(|error| format!(
            "Submitted, but filing in Sent is unconfirmed: {error}. Do not resend; inspect the email's mailboxes."
        ));
    Ok((id, warning))
}
