use std::sync::Arc;

use async_graphql::{
    ServerError, ServerResult, Variables,
    extensions::{Extension, ExtensionContext, ExtensionFactory, NextParseQuery},
    parser::types::ExecutableDocument,
};

const MAX_QUERY_BYTES: usize = 64 * 1024;
const MAX_SYNTAX_DEPTH: usize = 32;

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

pub(super) struct InputLimits;

impl ExtensionFactory for InputLimits {
    fn create(&self) -> Arc<dyn Extension> {
        Arc::new(Self)
    }
}

#[async_graphql::async_trait::async_trait]
impl Extension for InputLimits {
    async fn parse_query(
        &self,
        ctx: &ExtensionContext<'_>,
        query: &str,
        variables: &Variables,
        next: NextParseQuery<'_>,
    ) -> ServerResult<ExecutableDocument> {
        check_query(query).map_err(|message| ServerError::new(message, None))?;
        next.run(ctx, query, variables).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_syntax_before_parsing() {
        assert!(check_query(&"[".repeat(MAX_SYNTAX_DEPTH)).is_ok());
        assert!(check_query(&"[".repeat(MAX_SYNTAX_DEPTH + 1)).is_err());
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
}
