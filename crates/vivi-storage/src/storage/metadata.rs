use vivi_mail::message::normalize_message_id;

#[derive(Debug, Clone)]
pub(super) struct ParsedMetadata {
    pub(super) date: String,
    pub(super) from_addr: String,
    pub(super) to_addr: String,
    pub(super) cc_addr: String,
    pub(super) bcc_addr: String,
    pub(super) subject: String,
    pub(super) normalized_message_id: Option<String>,
}

pub(super) fn parse_metadata(data: &[u8]) -> ParsedMetadata {
    let Some(parsed) = mail_parser::MessageParser::default().parse(data) else {
        return ParsedMetadata {
            date: String::new(),
            from_addr: String::new(),
            to_addr: String::new(),
            cc_addr: String::new(),
            bcc_addr: String::new(),
            subject: String::new(),
            normalized_message_id: None,
        };
    };

    ParsedMetadata {
        date: parsed
            .date()
            .and_then(|d| chrono::DateTime::from_timestamp(d.to_timestamp(), 0))
            .map(|dt| dt.to_rfc3339())
            .unwrap_or_default(),
        from_addr: address_list(parsed.from()),
        to_addr: address_list(parsed.to()),
        cc_addr: address_list(parsed.cc()),
        bcc_addr: address_list(parsed.bcc()),
        subject: parsed.subject().unwrap_or_default().to_string(),
        normalized_message_id: parsed.message_id().and_then(normalize_message_id),
    }
}

fn address_list(list: Option<&mail_parser::Address<'_>>) -> String {
    list.map(|addresses| {
        addresses
            .iter()
            .filter_map(|addr| {
                let email = addr.address()?;
                let name = addr.name().unwrap_or("");
                if name.is_empty() {
                    Some(email.to_string())
                } else {
                    Some(format!("{name} <{email}>"))
                }
            })
            .collect::<Vec<_>>()
            .join(", ")
    })
    .unwrap_or_default()
}
