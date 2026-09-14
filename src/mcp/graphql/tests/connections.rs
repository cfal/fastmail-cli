use super::*;

#[tokio::test]
async fn page_info_cursors_work_without_selecting_nodes() {
    let server = mock_server(3).await;
    let first = run(
        &server,
        "{ emails(first: 1) { pageInfo { startCursor endCursor hasNextPage } } }",
    )
    .await;
    assert!(first.errors.is_empty(), "{:?}", first.errors);
    let info = first.data.into_json().unwrap()["emails"]["pageInfo"].clone();
    assert_eq!(info["startCursor"], "e0");
    assert_eq!(info["endCursor"], "e0");
    assert_eq!(info["hasNextPage"], true);
    let next = run(
        &server,
        "{ emails(first: 1, after: \"e0\") { pageInfo { endCursor } } }",
    )
    .await;
    assert!(next.errors.is_empty(), "{:?}", next.errors);
    assert_eq!(
        next.data.into_json().unwrap()["emails"]["pageInfo"]["endCursor"],
        "e1"
    );
}

#[tokio::test]
async fn list_connections_paginate_by_id() {
    // `Thread.emails` is the one with enough items in the mock to page through.
    let server = mock_server(5).await;

    let first = run(
        &server,
        "{ thread(emailId: \"e0\") { emails(first: 2) { \
            totalCount pageInfo { hasNextPage hasPreviousPage endCursor } \
            edges { cursor node { id } } } } }",
    )
    .await;
    assert!(first.errors.is_empty(), "{:?}", first.errors);
    let d = first.data.into_json().unwrap();
    let page = &d["thread"]["emails"];

    assert_eq!(page["totalCount"], 5, "totalCount ignores the page");
    assert_eq!(page["pageInfo"]["hasNextPage"], true);
    assert_eq!(page["pageInfo"]["hasPreviousPage"], false);
    let ids: Vec<&str> = page["edges"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["node"]["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["e0", "e1"]);
    // Cursors are IDs, as everywhere else in this schema.
    assert_eq!(page["edges"][0]["cursor"], "e0");
    let end = page["pageInfo"]["endCursor"].as_str().unwrap().to_string();

    let second = run(
        &server,
        &format!(
            "{{ thread(emailId: \"e0\") {{ emails(first: 2, after: \"{end}\") {{ \
                pageInfo {{ hasPreviousPage }} nodes {{ id }} }} }} }}"
        ),
    )
    .await;
    assert!(second.errors.is_empty(), "{:?}", second.errors);
    let d = second.data.into_json().unwrap();
    let ids: Vec<&str> = d["thread"]["emails"]["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["e2", "e3"], "`after` resumes past the cursor");
    assert_eq!(d["thread"]["emails"]["pageInfo"]["hasPreviousPage"], true);
}

#[tokio::test]
async fn list_connections_page_backwards() {
    let server = mock_server(5).await;
    let resp = run(
        &server,
        "{ thread(emailId: \"e0\") { emails(last: 2) { \
            pageInfo { hasNextPage hasPreviousPage } nodes { id } } } }",
    )
    .await;
    assert!(resp.errors.is_empty(), "{:?}", resp.errors);

    let d = resp.data.into_json().unwrap();
    let ids: Vec<&str> = d["thread"]["emails"]["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["e3", "e4"], "`last` takes from the end");
    assert_eq!(d["thread"]["emails"]["pageInfo"]["hasPreviousPage"], true);
    assert_eq!(d["thread"]["emails"]["pageInfo"]["hasNextPage"], false);
}

#[tokio::test]
async fn conflicting_page_args_are_rejected() {
    let server = mock_server(3).await;
    for (args, expected) in [
        ("first: 1, last: 1", "Pass `first` or `last`, not both."),
        (
            "after: \"e0\", before: \"e2\"",
            "Pass `after` or `before`, not both.",
        ),
        (
            "first: 1, last: 1, after: \"e0\", before: \"e2\"",
            "Pass `first` or `last`, not both.",
        ),
    ] {
        for query in [
            format!("{{ thread(emailId: \"e0\") {{ emails({args}) {{ nodes {{ id }} }} }} }}"),
            format!("{{ emails({args}) {{ nodes {{ id }} }} }}"),
        ] {
            let resp = run(&server, &query).await;
            assert_eq!(resp.errors.len(), 1, "{:?}", resp.errors);
            assert_eq!(resp.errors[0].message, expected);
        }
    }
}

#[tokio::test]
async fn a_removed_cursor_says_how_to_recover() {
    let server = mock_server(3).await;
    let resp = run(
        &server,
        "{ thread(emailId: \"e0\") { emails(after: \"nope\") { nodes { id } } } }",
    )
    .await;
    assert!(
        resp.errors
            .iter()
            .any(|e| e.message.contains("Restart pagination")),
        "got {:?}",
        resp.errors
    );
}

#[tokio::test]
async fn total_count_on_an_in_memory_list_is_free() {
    // No `calculateTotal` anywhere — the list is already in hand, so the count
    // is a length. Contrast the email connection, which asks the server.
    let server = mock_server(4).await;
    let resp = run(
        &server,
        "{ thread(emailId: \"e0\") { emails(first: 1) { totalCount } } }",
    )
    .await;
    assert!(resp.errors.is_empty(), "{:?}", resp.errors);

    let d = resp.data.into_json().unwrap();
    assert_eq!(d["thread"]["emails"]["totalCount"], 4);
}

/// The JMAP `filter` argument sent for the first `Email/query` in a request.
async fn sent_filter(server: &MockServer) -> Value {
    query_args(server)
        .await
        .first()
        .map_or(Value::Null, |args| args["filter"].clone())
}

/// Every `Email/query` argument object sent, in order.
async fn query_args(server: &MockServer) -> Vec<Value> {
    let mut out = Vec::new();
    for req in server.received_requests().await.unwrap_or_default() {
        let body: Value = match serde_json::from_slice(&req.body) {
            Ok(b) => b,
            Err(_) => continue,
        };
        for mc in body["methodCalls"].as_array().into_iter().flatten() {
            if mc.get(0).and_then(Value::as_str) == Some("Email/query") {
                out.push(mc[1].clone());
            }
        }
    }
    out
}

#[tokio::test]
async fn or_filter_reaches_jmap_as_a_filter_operator() {
    let server = mock_server(3).await;
    let resp = run(
        &server,
        r#"{ emails(filter: { unread: true,
                              or: [{ from: "a@b.com" }, { from: "c@d.com" }] })
              { nodes { id } } }"#,
    )
    .await;
    assert!(resp.errors.is_empty(), "{:?}", resp.errors);

    assert_eq!(
        sent_filter(&server).await,
        json!({
            "operator": "AND",
            "conditions": [
                { "notKeyword": "$seen" },
                { "operator": "OR", "conditions": [
                    { "from": "a@b.com" }, { "from": "c@d.com" }] }
            ]
        }),
        "the filter tree must survive into JMAP, not be flattened"
    );
}

#[tokio::test]
async fn mailbox_names_in_filters_resolve_to_ids() {
    let server = mock_server(3).await;
    let resp = run(
        &server,
        r#"{ emails(filter: { inMailbox: "INBOX", subject: "x" }) { nodes { id } } }"#,
    )
    .await;
    assert!(resp.errors.is_empty(), "{:?}", resp.errors);
    assert_eq!(sent_filter(&server).await["inMailbox"], "mb1");
}

#[tokio::test]
async fn unknown_mailbox_in_a_filter_is_an_error() {
    let server = mock_server(1).await;
    let resp = run(
        &server,
        r#"{ emails(filter: { inMailbox: "Nope" }) { nodes { id } } }"#,
    )
    .await;
    assert!(
        resp.errors.iter().any(|e| e.message.contains("Nope")),
        "expected an unknown-mailbox error, got {:?}",
        resp.errors
    );
}

#[tokio::test]
async fn mailbox_emails_are_scoped_to_that_mailbox() {
    let server = mock_server(3).await;
    let resp = run(
        &server,
        r#"{ mailbox(name: "INBOX") { emails(filter: { unread: true }) { nodes { id } } } }"#,
    )
    .await;
    assert!(resp.errors.is_empty(), "{:?}", resp.errors);

    assert_eq!(
        sent_filter(&server).await,
        json!({
            "operator": "AND",
            "conditions": [{ "notKeyword": "$seen" }, { "inMailbox": "mb1" }]
        }),
        "the caller's filter must be ANDed with the mailbox constraint"
    );
}

#[tokio::test]
async fn sort_is_passed_through() {
    let server = mock_server(2).await;
    let resp = run(
        &server,
        "{ emails(sort: [{ property: SUBJECT, ascending: true }]) { nodes { id } } }",
    )
    .await;
    assert!(resp.errors.is_empty(), "{:?}", resp.errors);
    assert_eq!(
        query_args(&server).await[0]["sort"],
        json!([{ "property": "subject", "isAscending": true }])
    );
}

#[tokio::test]
async fn cursors_are_ids_and_paginate_forward() {
    let server = mock_server(10).await;
    let resp = run(
        &server,
        "{ emails(first: 3) { edges { cursor node { id } } pageInfo { hasNextPage endCursor } } }",
    )
    .await;
    assert!(resp.errors.is_empty(), "{:?}", resp.errors);

    let data = resp.data.into_json().unwrap();
    let edges = data["emails"]["edges"].as_array().unwrap();
    assert_eq!(edges.len(), 3);
    assert_eq!(
        edges[0]["cursor"], "e0",
        "cursors are email IDs, not positions"
    );
    assert_eq!(edges[0]["node"]["id"], "e0");
    assert_eq!(edges[2]["cursor"], "e2");
    assert_eq!(data["emails"]["pageInfo"]["hasNextPage"], true);

    // Resume from the cursor and confirm we get the next window, not a repeat.
    let server2 = mock_server(10).await;
    let resp = run(
        &server2,
        "{ emails(first: 3, after: \"e2\") { edges { cursor node { id } } \
            pageInfo { hasPreviousPage } } }",
    )
    .await;
    assert!(resp.errors.is_empty(), "{:?}", resp.errors);
    let data = resp.data.into_json().unwrap();
    let edges = data["emails"]["edges"].as_array().unwrap();
    assert_eq!(edges[0]["cursor"], "e3");
    assert_eq!(edges[0]["node"]["id"], "e3");
    assert_eq!(data["emails"]["pageInfo"]["hasPreviousPage"], true);
}

#[tokio::test]
async fn last_paginates_backward_from_the_end() {
    let server = mock_server(10).await;
    let resp = run(
        &server,
        "{ emails(last: 2) { edges { cursor node { id } } } }",
    )
    .await;
    assert!(resp.errors.is_empty(), "{:?}", resp.errors);

    let data = resp.data.into_json().unwrap();
    let edges = data["emails"]["edges"].as_array().unwrap();
    assert_eq!(edges.len(), 2);
    assert_eq!(edges[0]["node"]["id"], "e8");
    assert_eq!(edges[1]["node"]["id"], "e9");
    assert_eq!(edges[1]["cursor"], "e9");
}

#[tokio::test]
async fn total_count_is_only_computed_when_selected() {
    let server = mock_server(7).await;
    let resp = run(&server, "{ emails(first: 2) { nodes { id } } }").await;
    assert!(resp.errors.is_empty(), "{:?}", resp.errors);
    assert_eq!(
        query_args(&server).await[0]["calculateTotal"],
        json!(false),
        "a query that doesn't select totalCount must not pay for it"
    );

    let server = mock_server(7).await;
    let resp = run(&server, "{ emails(first: 2) { totalCount nodes { id } } }").await;
    assert!(resp.errors.is_empty(), "{:?}", resp.errors);
    assert_eq!(query_args(&server).await[0]["calculateTotal"], json!(true));
    assert_eq!(resp.data.into_json().unwrap()["emails"]["totalCount"], 7);
}

#[tokio::test]
async fn counting_alone_fetches_no_emails() {
    let server = mock_server(7).await;
    let resp = run(
        &server,
        r#"{ emails(filter: { unread: true }) { totalCount } }"#,
    )
    .await;
    assert!(resp.errors.is_empty(), "{:?}", resp.errors);
    assert_eq!(resp.data.into_json().unwrap()["emails"]["totalCount"], 7);

    let gets = calls(&server)
        .await
        .into_iter()
        .filter(|c| c.method == "Email/get")
        .count();
    assert_eq!(gets, 0, "asking only for a count must not fetch any email");
}

#[tokio::test]
async fn page_size_is_capped() {
    let server = mock_server(3).await;
    let resp = run(&server, "{ emails(first: 5000) { nodes { id } } }").await;
    assert!(resp.errors.is_empty(), "{:?}", resp.errors);
    assert_eq!(query_args(&server).await[0]["limit"], json!(100));
}

#[tokio::test]
async fn collapse_threads_reaches_jmap() {
    let server = mock_server(3).await;
    let resp = run(
        &server,
        "{ emails(collapseThreads: true) { nodes { id } } }",
    )
    .await;
    assert!(resp.errors.is_empty(), "{:?}", resp.errors);
    assert_eq!(query_args(&server).await[0]["collapseThreads"], json!(true));
}

#[tokio::test]
async fn results_keep_the_sort_order_jmap_returned() {
    let server = mock_server(5).await;
    let resp = run(&server, "{ emails(first: 5) { nodes { id } } }").await;
    assert!(resp.errors.is_empty(), "{:?}", resp.errors);

    let data = resp.data.into_json().unwrap();
    let ids: Vec<&str> = data["emails"]["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["id"].as_str().unwrap())
        .collect();
    // `Email/get` makes no ordering promise, so this asserts we re-apply the
    // order from `Email/query` rather than trusting the response.
    assert_eq!(ids, ["e0", "e1", "e2", "e3", "e4"]);
}

#[tokio::test]
async fn deprecated_search_emails_still_works() {
    let server = mock_server(3).await;
    let resp = run(
        &server,
        r#"{ searchEmails(query: "invoice", unread: true) { nodes { id } } }"#,
    )
    .await;
    assert!(resp.errors.is_empty(), "{:?}", resp.errors);
    assert_eq!(
        sent_filter(&server).await,
        json!({ "text": "invoice", "notKeyword": "$seen" })
    );
}

#[tokio::test]
async fn bodies_batch_across_a_connection_page() {
    let server = mock_server(8).await;
    let resp = run(&server, "{ emails(first: 8) { nodes { id textBody } } }").await;
    assert!(resp.errors.is_empty(), "{:?}", resp.errors);

    let detail: Vec<Call> = calls(&server)
        .await
        .into_iter()
        .filter(|c| c.method == "Email/get" && c.properties.contains(&"textBody".to_string()))
        .collect();
    assert_eq!(detail.len(), 1, "one batched detail fetch for the page");
    assert_eq!(detail[0].ids.len(), 8);
}

#[tokio::test]
async fn cursors_use_jmap_anchors_not_offsets() {
    let server = mock_server(10).await;
    let resp = run(
        &server,
        "{ emails(first: 3, after: \"e2\") { nodes { id } } }",
    )
    .await;
    assert!(resp.errors.is_empty(), "{:?}", resp.errors);

    let args = &query_args(&server).await[0];
    assert_eq!(args["anchor"], "e2");
    assert_eq!(args["anchorOffset"], 1);
    assert!(
        args.get("position").is_none(),
        "an anchored query must not also send a position"
    );
}

#[tokio::test]
async fn pagination_is_stable_when_new_mail_arrives() {
    // Page 1 of a 10-message inbox.
    let server = mock_server(10).await;
    let resp = run(
        &server,
        "{ emails(first: 3) { edges { cursor node { id } } } }",
    )
    .await;
    assert!(resp.errors.is_empty(), "{:?}", resp.errors);
    let page1 = resp.data.into_json().unwrap();
    let last_cursor = page1["emails"]["edges"][2]["cursor"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(last_cursor, "e2");

    // Two new messages land at the head of the result set before page 2. With a
    // positional cursor, `after: "2"` would now return e0..e2 again — the two
    // newcomers having pushed everything down. The anchor still names e2.
    let server = MockServer::start().await;
    let ids: Vec<String> = (0..12).map(|i| format!("e{i}")).collect();
    let shifted = ids.clone();
    Mock::given(method("POST"))
        .and(path("/jmap"))
        .respond_with(move |req: &wiremock::Request| {
            let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
            let mut responses = Vec::new();
            for mc in body["methodCalls"].as_array().into_iter().flatten() {
                let name = mc[0].as_str().unwrap_or("");
                let args = &mc[1];
                let tag = mc[2].as_str().unwrap_or("c0");
                // "new0"/"new1" prepended: every old message shifted by 2.
                let all: Vec<String> = ["new0".to_string(), "new1".to_string()]
                    .into_iter()
                    .chain(shifted.iter().take(10).cloned())
                    .collect();
                let payload = match name {
                    "Email/query" => {
                        let anchor = args["anchor"].as_str().unwrap();
                        let idx = all.iter().position(|i| i == anchor).unwrap() as i64;
                        let off = args["anchorOffset"].as_i64().unwrap_or(0);
                        let start = (idx + off).max(0) as usize;
                        let limit = args["limit"].as_u64().unwrap_or(10) as usize;
                        json!({
                            "ids": all.iter().skip(start).take(limit).collect::<Vec<_>>(),
                            "position": start
                        })
                    }
                    "Email/get" => json!({
                        "list": all.iter().skip(5).take(3)
                            .map(|id| email_json(id, "t1", false)).collect::<Vec<_>>(),
                        "notFound": []
                    }),
                    _ => json!({ "list": [], "notFound": [] }),
                };
                responses.push(json!([name, payload, tag]));
            }
            ResponseTemplate::new(200).set_body_json(json!({ "methodResponses": responses }))
        })
        .mount(&server)
        .await;

    let resp = run(
        &server,
        &format!("{{ emails(first: 3, after: \"{last_cursor}\") {{ nodes {{ id }} }} }}"),
    )
    .await;
    assert!(resp.errors.is_empty(), "{:?}", resp.errors);

    let data = resp.data.into_json().unwrap();
    let ids: Vec<&str> = data["emails"]["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        ["e3", "e4", "e5"],
        "the page after e2 must still start at e3, not repeat e0..e2"
    );
}

#[tokio::test]
async fn last_without_before_uses_a_negative_position() {
    let server = mock_server(10).await;
    let resp = run(&server, "{ emails(last: 2) { nodes { id } } }").await;
    assert!(resp.errors.is_empty(), "{:?}", resp.errors);

    let args = &query_args(&server).await[0];
    assert_eq!(
        args["position"], -2,
        "JMAP counts a negative position from the end"
    );
    assert_eq!(
        args["calculateTotal"], false,
        "counting from the end needs no total"
    );
    assert_eq!(query_args(&server).await.len(), 1, "and only one call");
}

#[tokio::test]
async fn before_anchors_backward_from_the_cursor() {
    let server = mock_server(10).await;
    let resp = run(
        &server,
        "{ emails(last: 3, before: \"e7\") { nodes { id } } }",
    )
    .await;
    assert!(resp.errors.is_empty(), "{:?}", resp.errors);

    let args = &query_args(&server).await[0];
    assert_eq!(args["anchor"], "e7");
    assert_eq!(args["anchorOffset"], -3);

    let data = resp.data.into_json().unwrap();
    let ids: Vec<&str> = data["emails"]["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["e4", "e5", "e6"], "the three ending just before e7");
}

#[tokio::test]
async fn a_stale_cursor_says_how_to_recover() {
    let server = mock_server(5).await;
    let resp = run(
        &server,
        "{ emails(first: 2, after: \"deleted\") { nodes { id } } }",
    )
    .await;
    let msg = resp
        .errors
        .first()
        .map(|e| e.message.clone())
        .unwrap_or_default();
    assert!(
        msg.contains("no longer in this result set") && msg.contains("Restart pagination"),
        "expected actionable stale-anchor advice, got {msg:?}"
    );
}

#[tokio::test]
async fn query_state_and_position_are_exposed() {
    let server = mock_server(10).await;
    let resp = run(
        &server,
        "{ emails(first: 2, after: \"e4\") { position queryState nodes { id } } }",
    )
    .await;
    assert!(resp.errors.is_empty(), "{:?}", resp.errors);

    let data = resp.data.into_json().unwrap();
    assert_eq!(data["emails"]["position"], 5);
    assert_eq!(data["emails"]["queryState"], "qs-1");
}

#[tokio::test]
async fn thread_keyword_and_header_conditions_reach_jmap() {
    let server = mock_server(3).await;
    let resp = run(
        &server,
        r#"{ emails(filter: {
                someInThreadHaveKeyword: "$flagged"
                noneInThreadHaveKeyword: "$junk"
                header: ["List-Id", "rust-lang"]
            }) { nodes { id } } }"#,
    )
    .await;
    assert!(resp.errors.is_empty(), "{:?}", resp.errors);

    assert_eq!(
        sent_filter(&server).await,
        json!({
            "someInThreadHaveKeyword": "$flagged",
            "noneInThreadHaveKeyword": "$junk",
            "header": ["List-Id", "rust-lang"]
        })
    );
}

#[tokio::test]
async fn in_mailbox_other_than_resolves_names() {
    let server = mock_server(3).await;
    let resp = run(
        &server,
        r#"{ emails(filter: { inMailboxOtherThan: ["INBOX"] }) { nodes { id } } }"#,
    )
    .await;
    assert!(resp.errors.is_empty(), "{:?}", resp.errors);
    assert_eq!(
        sent_filter(&server).await,
        json!({ "inMailboxOtherThan": ["mb1"] })
    );
}

#[tokio::test]
async fn keyword_sort_carries_its_keyword() {
    let server = mock_server(3).await;
    let resp = run(
        &server,
        r#"{ emails(sort: [{ property: SOME_IN_THREAD_HAVE_KEYWORD, keyword: "$flagged" }])
              { nodes { id } } }"#,
    )
    .await;
    assert!(resp.errors.is_empty(), "{:?}", resp.errors);
    assert_eq!(
        query_args(&server).await[0]["sort"],
        json!([{ "property": "someInThreadHaveKeyword", "isAscending": false,
                 "keyword": "$flagged" }])
    );
}

#[tokio::test]
async fn keyword_sort_without_keyword_is_rejected_before_any_call() {
    let server = mock_server(3).await;
    let resp = run(
        &server,
        "{ emails(sort: [{ property: HAS_KEYWORD }]) { nodes { id } } }",
    )
    .await;
    assert!(
        resp.errors
            .iter()
            .any(|e| e.message.contains("requires a `keyword`")),
        "got {:?}",
        resp.errors
    );
    assert!(
        query_args(&server).await.is_empty(),
        "an invalid comparator must not reach the server"
    );
}
