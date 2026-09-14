use std::sync::Arc;

use async_graphql::{
    Request, ServerError, ServerResult,
    extensions::{Extension, ExtensionContext, ExtensionFactory, NextPrepareRequest},
    parser::types::{ExecutableDocument, Selection},
};

const MAX_QUERY_BYTES: usize = 64 * 1024;
const MAX_SYNTAX_DEPTH: usize = 32;
const MAX_EXPANDED_SELECTIONS: usize = 10_000;

pub(crate) fn check_query(query: &str) -> Result<(), String> {
    if query.len() > MAX_QUERY_BYTES {
        return Err("GraphQL query exceeds 64 KiB; use variables for large values".into());
    }
    // Schema depth validation runs after parsing. Bound parser recursion first,
    // ignoring delimiters in comments, strings and escaped block strings.
    let bytes = query.as_bytes();
    let (mut i, mut depth) = (0, 0usize);
    while i < bytes.len() {
        match bytes[i] {
            b'#' => {
                while i < bytes.len() && !matches!(bytes[i], b'\r' | b'\n') {
                    i += 1;
                }
            }
            b'"' if bytes[i..].starts_with(b"\"\"\"") => {
                i += 3;
                while i < bytes.len() {
                    if bytes[i..].starts_with(b"\\\"\"\"") {
                        i += 4;
                    } else if bytes[i..].starts_with(b"\"\"\"") {
                        i += 3;
                        break;
                    } else {
                        i += 1;
                    }
                }
            }
            b'"' => {
                i += 1;
                while i < bytes.len() {
                    match bytes[i] {
                        b'\\' => i += 2,
                        b'"' => {
                            i += 1;
                            break;
                        }
                        _ => i += 1,
                    }
                }
            }
            b'{' | b'[' | b'(' => {
                depth += 1;
                if depth > MAX_SYNTAX_DEPTH {
                    return Err("GraphQL syntax nesting exceeds 32 levels".into());
                }
                i += 1;
            }
            b'}' | b']' | b')' => {
                depth = depth.saturating_sub(1);
                i += 1;
            }
            _ => i += 1,
        }
    }
    Ok(())
}

fn check_expansion(
    document: &ExecutableDocument,
    limit: usize,
    max_depth: usize,
) -> ServerResult<()> {
    // The upstream recursive-depth check expands fragments before complexity
    // validation. Include unused definitions, which later validators also visit.
    let roots = document
        .operations
        .iter()
        .map(|(_, op)| &op.node.selection_set)
        .chain(document.fragments.values().map(|f| &f.node.selection_set));
    let mut remaining = limit;
    for root in roots {
        let mut pending = vec![(root, 0)];
        while let Some((selections, depth)) = pending.pop() {
            if depth > max_depth {
                return Err(ServerError::new(
                    format!("Expanded GraphQL nesting exceeds {max_depth} levels"),
                    Some(selections.pos),
                ));
            }
            remaining = remaining
                .checked_sub(selections.node.items.len())
                .ok_or_else(|| {
                    ServerError::new(
                        format!("GraphQL expansion exceeds {limit} selections"),
                        Some(selections.pos),
                    )
                })?;
            for selection in &selections.node.items {
                let children = match &selection.node {
                    Selection::Field(field) => Some(&field.node.selection_set),
                    Selection::InlineFragment(fragment) => Some(&fragment.node.selection_set),
                    Selection::FragmentSpread(spread) => document
                        .fragments
                        .get(&spread.node.fragment_name.node)
                        .map(|fragment| &fragment.node.selection_set),
                };
                if let Some(children) = children
                    && !children.node.items.is_empty()
                {
                    pending.push((children, depth + 1));
                }
            }
        }
    }
    Ok(())
}

pub(super) struct InputLimits;

impl ExtensionFactory for InputLimits {
    fn create(&self) -> Arc<dyn Extension> {
        Arc::new(Self)
    }
}

#[async_graphql::async_trait::async_trait]
impl Extension for InputLimits {
    async fn prepare_request(
        &self,
        ctx: &ExtensionContext<'_>,
        mut request: Request,
        next: NextPrepareRequest<'_>,
    ) -> ServerResult<Request> {
        check_query(&request.query).map_err(|message| ServerError::new(message, None))?;
        check_expansion(
            request.parsed_query()?,
            MAX_EXPANDED_SELECTIONS,
            MAX_SYNTAX_DEPTH,
        )?;
        next.run(ctx, request).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_syntax_before_parsing() {
        assert!(check_query(&"[".repeat(MAX_SYNTAX_DEPTH)).is_ok());
        assert!(check_query(&"[".repeat(MAX_SYNTAX_DEPTH + 1)).is_err());
        assert!(check_query(&" ".repeat(MAX_QUERY_BYTES)).is_ok());
        assert!(check_query(&" ".repeat(MAX_QUERY_BYTES + 1)).is_err());
        assert!(
            check_query("{ emails(filter: {and: [{unread: true}]}) { nodes { id } } }").is_ok()
        );
    }

    #[test]
    fn ignores_string_and_comment_delimiters() {
        let brackets = "[".repeat(40);
        for query in [
            format!("# {brackets}\n{{ __typename }}"),
            format!("{{ field(value: \"{brackets}\\\"{brackets}\") }}"),
            format!("{{ field(value: \"\"\"{brackets}\\\"\"\"{brackets}\"\"\") }}"),
        ] {
            assert!(check_query(&query).is_ok(), "{query}");
        }
    }

    #[tokio::test]
    async fn schema_uses_the_same_preparse_guard() {
        let response = super::super::build_schema().execute("[".repeat(33)).await;
        assert!(response.errors[0].message.contains("syntax nesting"));
    }

    #[tokio::test]
    async fn literal_input_nesting_is_bounded_not_just_selection_depth() {
        let mut filter = "{unread:true}".to_string();
        for _ in 0..16 {
            filter = format!("{{and:[{filter}]}}");
        }
        let query = format!("{{ emails(filter:{filter}) {{ nodes {{ id }} }} }}");
        let response = super::super::build_schema().execute(query).await;
        assert_eq!(response.errors.len(), 1);
        assert_eq!(
            response.errors[0].message,
            "GraphQL syntax nesting exceeds 32 levels"
        );
    }

    #[test]
    fn scanner_handles_truncated_strings_and_cr_terminated_comments() {
        let brackets = "[".repeat(MAX_SYNTAX_DEPTH + 1);
        for ignored in [
            format!("\"{brackets}\\"),
            format!("\"\"\"{brackets}"),
            format!("# {brackets}\r{{ __typename }}"),
        ] {
            assert!(check_query(&ignored).is_ok());
        }
        let after_comment = format!("# ignored\r{brackets}");
        assert!(check_query(&after_comment).is_err());
    }

    #[test]
    fn fragment_expansion_is_bounded_before_recursive_validation() {
        let document = async_graphql::parser::parse_query(
            "{ ...Twice } fragment Twice on QueryRoot { ...Leaf ...Leaf } \
             fragment Leaf on QueryRoot { __typename }",
        )
        .unwrap();
        assert!(check_expansion(&document, 10, 3).is_ok());
        assert!(check_expansion(&document, 9, 3).is_err());
        assert!(check_expansion(&document, 10, 1).is_err());
    }

    #[tokio::test]
    async fn unused_fragment_cycles_are_checked_before_validation() {
        let response = super::super::build_schema()
            .execute("{ __typename } fragment Unused on QueryRoot { ...Unused }")
            .await;
        assert!(
            response.errors[0]
                .message
                .contains("Expanded GraphQL nesting")
        );
    }
}
