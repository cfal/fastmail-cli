use std::borrow::Cow;
use std::collections::HashSet;

use async_graphql::parser::types::Field;
use async_graphql::registry::Registry;
use async_graphql::{
    Context, ContextSelectionSet, Object, OutputType, Positioned, Result, ServerResult, Value,
};

use super::connection::{
    EmailConnection, Page, PageArgs, page_complexity, page_of, thread_page_connection,
    wants_connection_nodes,
};
use super::loaders::{Threads, load_summaries, to_gql_error};
use super::types::{GqlEmail, GqlThread};

pub(crate) struct ThreadView(LazyThread);

impl ThreadView {
    pub(crate) fn new(id: String) -> Self {
        Self(LazyThread { id })
    }
}

// Register only the public type, including in schemas that also return legacy
// GqlThread literals. The private resolver must never register a second Thread.
impl OutputType for ThreadView {
    fn type_name() -> Cow<'static, str> {
        GqlThread::type_name()
    }

    fn create_type_info(registry: &mut Registry) -> String {
        GqlThread::create_type_info(registry)
    }

    async fn resolve(
        &self,
        ctx: &ContextSelectionSet<'_>,
        field: &Positioned<Field>,
    ) -> ServerResult<Value> {
        self.0.resolve(ctx, field).await
    }
}

struct LazyThread {
    id: String,
}

impl LazyThread {
    async fn email_ids(&self, ctx: &Context<'_>) -> Result<Vec<String>> {
        let ids = ctx
            .data::<Threads>()?
            .load_one(self.id.clone())
            .await
            .map_err(to_gql_error)?
            .unwrap_or_default();
        let mut seen = HashSet::with_capacity(ids.len());
        if ids.iter().any(|id| id.is_empty() || !seen.insert(id)) {
            return Err(async_graphql::Error::new(
                "Thread contains empty or duplicate email IDs; restart pagination",
            ));
        }
        Ok(ids)
    }
}

#[Object(name = "Thread")]
impl LazyThread {
    async fn id(&self) -> Option<&str> {
        Some(&self.id)
    }

    #[graphql(complexity = "page_complexity(first, last, child_complexity)")]
    async fn emails(
        &self,
        ctx: &Context<'_>,
        after: Option<String>,
        before: Option<String>,
        first: Option<i32>,
        last: Option<i32>,
    ) -> Result<EmailConnection> {
        let args = PageArgs {
            after,
            before,
            first,
            last,
        };
        args.validate()?;
        // RFC 8621 defines this order as oldest-first with stable server ties.
        let page = page_of(self.email_ids(ctx).await?, args, String::clone)?;
        let mut items = Vec::new();
        if wants_connection_nodes(ctx) {
            let mut summaries =
                load_summaries(ctx, page.items.iter().map(|(_, id)| id.clone()).collect()).await?;
            for (cursor, id) in page.items {
                let email = summaries.remove(&id).ok_or_else(|| {
                    async_graphql::Error::new(format!(
                        "Thread email {id} no longer exists; restart pagination"
                    ))
                })?;
                items.push((cursor, GqlEmail::summary_shared(email)));
            }
        }
        Ok(thread_page_connection(Page {
            items,
            start: page.start,
            total: page.total,
            has_previous: page.has_previous,
            has_next: page.has_next,
        }))
    }

    async fn total(&self, ctx: &Context<'_>) -> Result<usize> {
        Ok(self.email_ids(ctx).await?.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_graphql::registry::MetaType;

    type Shape = Vec<(String, String, Vec<(String, String)>)>;

    fn shape(register: fn(&mut Registry) -> String) -> Shape {
        let mut registry = Registry::default();
        // Only this scratch registry sees both types; production registers the
        // canonical public type exclusively.
        registry.ignore_name_conflicts.insert("Thread".into());
        register(&mut registry);
        let Some(MetaType::Object { fields, .. }) = registry.types.remove("Thread") else {
            panic!("Thread is not an object");
        };
        fields
            .into_values()
            .map(|field| {
                (
                    field.name,
                    field.ty,
                    field
                        .args
                        .into_values()
                        .map(|arg| (arg.name, arg.ty))
                        .collect(),
                )
            })
            .collect()
    }

    #[test]
    fn lazy_fields_match_the_canonical_thread_schema() {
        assert_eq!(
            shape(LazyThread::create_type_info),
            shape(GqlThread::create_type_info)
        );
    }
}
