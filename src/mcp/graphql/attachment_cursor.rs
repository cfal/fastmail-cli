use std::collections::HashMap;

use async_graphql::connection::{Connection, Edge};
use async_graphql::{Error, Result};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};

use super::connection::{CountFields, ListConnection, PageArgs, page_of};
use super::types::GqlAttachment;

pub(super) fn attachment_connection(
    attachments: Vec<GqlAttachment>,
    mut args: PageArgs,
) -> Result<ListConnection<GqlAttachment>> {
    let mut occurrences = HashMap::new();
    let cursors: Vec<_> = attachments
        .iter()
        .map(|attachment| {
            let part = attachment
                .part_id
                .as_deref()
                .filter(|part| !part.is_empty());
            let occurrence = occurrences
                .entry((&attachment.blob_id, part))
                .or_insert(0usize);
            let identity = serde_json::to_vec(&(&attachment.blob_id, part, *occurrence))?;
            *occurrence += 1;
            Ok(format!(
                "attachment-v1:{}",
                URL_SAFE_NO_PAD.encode(identity)
            ))
        })
        .collect::<Result<_>>()?;

    for cursor in [&mut args.after, &mut args.before].into_iter().flatten() {
        if cursors.contains(cursor) {
            continue;
        }
        let mut matches = attachments
            .iter()
            .enumerate()
            .filter(|(_, attachment)| attachment.blob_id == *cursor);
        if let Some((index, _)) = matches.next() {
            if matches.next().is_some() {
                return Err(Error::new(
                    "Ambiguous legacy attachment cursor. Restart pagination to obtain per-part cursors.",
                ));
            }
            *cursor = cursors[index].clone();
        }
    }
    let page = page_of(
        attachments.into_iter().enumerate().collect(),
        args,
        |(index, _)| cursors[*index].clone(),
    )?;
    let mut connection = Connection::with_additional_fields(
        page.has_previous,
        page.has_next,
        CountFields {
            total_count: page.total as u64,
        },
    );
    connection.edges.extend(
        page.items
            .into_iter()
            .map(|(cursor, (_, node))| Edge::new(cursor, node)),
    );
    Ok(connection)
}
