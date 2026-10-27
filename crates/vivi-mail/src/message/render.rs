//! Rendering a raw `.eml` for terminal display and JSON output.

use chrono::DateTime;

use vivi_error::VivariumError;

use super::normalize_message_id;

/// Render a raw `.eml` as readable terminal output.
///
/// # Errors
/// Returns an error if the message cannot be parsed.
pub fn render_message(data: &[u8]) -> Result<String, VivariumError> {
    let parsed = mail_parser::MessageParser::default()
        .parse(data)
        .ok_or_else(|| VivariumError::Parse("failed to parse message".into()))?;

    let from = parsed
        .from()
        .and_then(|a| a.first())
        .map_or_else(|| "unknown".to_string(), display_address);
    let to = parsed
        .to()
        .and_then(|a| a.first())
        .map_or_else(|| "unknown".to_string(), display_address);

    let subject = parsed.subject().unwrap_or("(no subject)");
    let msg_date = parsed
        .date()
        .and_then(|d| DateTime::from_timestamp(d.to_timestamp(), 0))
        .map_or_else(
            || "unknown".to_string(),
            |dt| dt.format("%Y-%m-%d %H:%M %Z").to_string(),
        );

    let body = parsed
        .body_text(0)
        .map_or_else(|| "(no text body)".to_string(), |body| body.into_owned());

    Ok(format!(
        "From:    {from}\nTo:      {to}\nDate:    {msg_date}\nSubject: {subject}\n\n{body}"
    ))
}

/// Render a raw `.eml` as a JSON value.
///
/// # Errors
/// Returns an error if the message cannot be parsed.
pub fn to_json_message(message_id: &str, data: &[u8]) -> Result<serde_json::Value, VivariumError> {
    let parsed = mail_parser::MessageParser::default()
        .parse(data)
        .ok_or_else(|| VivariumError::Parse("failed to parse message".into()))?;

    let msg_date = parsed
        .date()
        .and_then(|d| DateTime::from_timestamp(d.to_timestamp(), 0))
        .map(|dt| dt.to_rfc3339());
    let body = parsed.body_text(0).map(|body| body.into_owned());

    Ok(serde_json::json!({
        "handle": message_id,
        "message_id": parsed.message_id().and_then(normalize_message_id),
        "from": first_address(parsed.from()),
        "to": addresses(parsed.to()),
        "cc": addresses(parsed.cc()),
        "bcc": addresses(parsed.bcc()),
        "date": msg_date,
        "subject": parsed.subject(),
        "body": body,
    }))
}

fn display_address(address: &mail_parser::Addr<'_>) -> String {
    let name = address.name().unwrap_or("");
    let addr = address.address().unwrap_or("");
    if name.is_empty() {
        addr.to_string()
    } else {
        format!("{name} <{addr}>")
    }
}

fn first_address(list: Option<&mail_parser::Address>) -> Option<String> {
    list.and_then(|addresses| addresses.first())
        .and_then(format_address)
}

fn addresses(list: Option<&mail_parser::Address>) -> Vec<String> {
    list.map(|addresses| addresses.iter().filter_map(format_address).collect())
        .unwrap_or_default()
}

fn format_address(address: &mail_parser::Addr<'_>) -> Option<String> {
    let addr = address.address()?;
    let name = address.name().unwrap_or("");
    if name.is_empty() {
        Some(addr.to_string())
    } else {
        Some(format!("{name} <{addr}>"))
    }
}
