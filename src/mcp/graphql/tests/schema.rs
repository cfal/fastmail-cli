use super::*;

#[tokio::test]
async fn excessive_graphql_fanout_is_rejected_before_network_requests() {
    let server = mock_server(1).await;
    let response = run(&server, "{ emails(first: 100) { nodes { thread { emails(first: 100) { nodes { attachments { nodes { text base64 image } } } } } } } }").await;
    assert!(
        response
            .errors
            .iter()
            .any(|e| e.message.to_lowercase().contains("complex")),
        "{:?}",
        response.errors
    );
    assert!(calls(&server).await.is_empty());
    assert_eq!(
        super::super::connection::page_complexity(Some(100), None, usize::MAX),
        super::super::MAX_COMPLEXITY + 1
    );
}

#[tokio::test]
async fn enormous_graphql_costs_cannot_overflow_the_upstream_accumulator() {
    let mut inner = "subject".to_owned();
    for _ in 0..25 {
        inner = format!("thread {{ emails(first: 100) {{ nodes {{ {inner} }} }} }}");
    }
    let branch = format!("emails(first: 100) {{ nodes {{ {inner} }} }}");
    let response = build_schema()
        .execute(format!("{{ a:{branch} b:{branch} }}"))
        .await;
    assert!(!response.errors.is_empty());
}

#[tokio::test]
async fn schema_prose_names_no_removed_construct() {
    // The SDL's doc comments are what a model reads after `schema_sdl`, and
    // they are the easiest thing in the repo to leave behind: nothing compiles
    // them. Guard the constructs this schema has actually dropped.
    let sdl = build_schema().sdl();

    let mut prose = String::new();
    let mut in_doc = false;
    for line in sdl.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("\"\"\"") && !(trimmed.len() > 6 && trimmed.ends_with("\"\"\"")) {
            in_doc = !in_doc;
            continue;
        }
        if in_doc || trimmed.starts_with("\"\"\"") {
            prose.push_str(trimmed);
            prose.push('\n');
        }
    }

    for (gone, replacement) in [
        (
            "content {",
            "`Attachment.content` split into base64/image/text",
        ),
        ("textContent", "`AttachmentContent` is gone — use `text`"),
        (
            "base64Content",
            "`AttachmentContent` is gone — use `base64`",
        ),
        ("EmailSummary", "folded into `Email`"),
        (
            "mailbox:",
            "`emails(mailbox:)` is now `filter: { inMailbox: }`",
        ),
    ] {
        assert!(
            !prose.contains(gone),
            "schema prose still mentions `{gone}` — {replacement}"
        );
    }
}

/// Every operation in a fenced block of the advertised tool descriptions and
/// server instructions.
///
/// Scraped from what the server actually publishes, rather than copied here:
/// these exist so a model can compose a query without fetching the schema
/// first, which is worth nothing if they are wrong. One operation per line,
/// which is how they are written.
fn documented_shapes() -> Vec<String> {
    use rmcp::ServerHandler;

    let mcp = crate::mcp::FastmailMcp::http();
    let published: Vec<String> = mcp
        .tool_router
        .list_all()
        .into_iter()
        .filter_map(|t| t.description.map(|d| d.to_string()))
        .chain(mcp.get_info().instructions)
        .collect();

    published
        .iter()
        .flat_map(|text| text.split("```").skip(1).step_by(2))
        .flat_map(|block| {
            block
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .collect()
}

#[tokio::test]
async fn documented_examples_execute() {
    // The query shapes in the README and the MCP server instructions, run
    // against the real schema so a documented example cannot drift into being
    // invalid without a test noticing.
    let server = mock_server(10).await;
    let documented = [
        "{ emails(filter: { inMailbox: \"INBOX\" }, first: 10) { nodes { \
            subject from { name email } readableBody { format content isTruncated isEncodingProblem warnings } \
            attachments { nodes { name contentType size cid text } } } } }",
        "{ mailbox(name: \"INBOX\") { name children { nodes { name unreadEmails } } \
            emails(first: 5) { nodes { subject \
              thread { total emails { nodes { subject readableBody { format content warnings } } } } \
              mailboxes { name role } } } } }",
        "{ emails(filter: { unread: true }, sort: [{ property: SIZE, ascending: false }], \
            first: 20) { totalCount nodes { subject size } } }",
    ];

    let scraped = documented_shapes();
    assert!(
        scraped.len() >= 3,
        "expected the tool descriptions to carry worked examples, found {scraped:?}"
    );

    for query in documented.iter().map(|q| q.to_string()).chain(scraped) {
        let resp = run(&server, &query).await;
        assert!(
            resp.errors.is_empty(),
            "documented example failed: {:?}\nquery: {query}",
            resp.errors
        );
    }
}

#[tokio::test]
async fn the_inlined_schema_sketch_names_only_real_fields() {
    // The `graphql` description inlines a slimmed schema so everyday mail needs
    // no `schema_sdl` round trip at all. That trade only holds while it is
    // true — a sketch that outlives a rename sends models at fields that no
    // longer exist, which is worse than making them fetch the real thing.
    let mcp = crate::mcp::FastmailMcp::http();
    let sdl = build_schema().sdl();
    let description = mcp
        .tool_router
        .list_all()
        .into_iter()
        .find(|t| t.name == "graphql")
        .and_then(|t| t.description)
        .expect("the graphql tool is described");

    // Only the indented signature lines, minus their `#` comments — the prose
    // around them mentions things like "CardDAV" that are deliberately not
    // schema names.
    let mut checked = 0;
    for line in description.lines().filter(|l| l.starts_with("  ")) {
        let code = line.split('#').next().unwrap_or_default();
        for ident in code.split(|c: char| !c.is_alphanumeric() && c != '_') {
            // Field and argument names only: types are capitalised, and
            // `true`/`false` are values rather than anything to look up.
            if ident.len() < 2
                || !ident.starts_with(|c: char| c.is_ascii_lowercase())
                || matches!(ident, "true" | "false" | "null")
            {
                continue;
            }
            checked += 1;
            assert!(
                sdl.contains(&format!("{ident}:")) || sdl.contains(&format!("{ident}(")),
                "the inlined sketch names `{ident}`, which the schema does not define"
            );
        }
    }
    assert!(
        checked > 50,
        "expected a real sketch, only checked {checked}"
    );
}

#[tokio::test]
async fn depth_limit_rejects_runaway_nesting() {
    let server = mock_server(1).await;
    // `parent` carries no fan-out cost, so this trips the depth guard rather
    // than the complexity guard.
    let deep = format!(
        "{{ mailbox(name: \"INBOX\") {{ {} id {} }} }}",
        "parent { ".repeat(16),
        "}".repeat(16)
    );
    let resp = run(&server, &deep).await;
    assert!(
        resp.errors.iter().any(|e| e.message.contains("too deep")),
        "expected a depth-limit error, got {:?}",
        resp.errors
    );
}

#[tokio::test]
async fn depth_limit_leaves_realistic_queries_alone() {
    let server = mock_server(1).await;
    let resp = run(
        &server,
        "{ mailbox(name: \"INBOX\") { emails(first: 1) { nodes { thread { emails { nodes { \
            attachments { nodes { name size } } } } } } } } }",
    )
    .await;
    assert!(resp.errors.is_empty(), "{:?}", resp.errors);
}

#[tokio::test]
async fn deep_cyclic_queries_are_still_refused() {
    // Keep fanout at one to exercise depth independently of complexity.
    let server = mock_server(1).await;

    let mut inner = "name".to_string();
    for _ in 0..20 {
        inner = format!("parent {{ {inner} }}");
    }
    let resp = run(
        &server,
        &format!("{{ mailbox(name: \"INBOX\") {{ {inner} }} }}"),
    )
    .await;

    assert!(
        resp.errors
            .iter()
            .any(|e| e.message.contains("nested too deep")),
        "expected a depth-limit error, got {:?}",
        resp.errors
    );
}

#[test]
fn schema_exposes_the_nested_edges() {
    let sdl = build_schema().sdl();
    for edge in [
        "emails(",    // Mailbox.emails
        "children(",  // Mailbox.children — a connection, so it takes page args
        "parent:",    // Mailbox.parent
        "thread:",    // Email.thread
        "mailboxes:", // Email.mailboxes
    ] {
        assert!(sdl.contains(edge), "SDL missing `{edge}`:\n{sdl}");
    }
    assert!(
        !sdl.contains("EmailSummary"),
        "EmailSummary should be folded into Email"
    );
}
