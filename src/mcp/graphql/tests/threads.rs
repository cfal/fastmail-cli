use super::super::types::{GqlEmail, GqlThread};
use super::*;
use async_graphql::{Context, EmptyMutation, EmptySubscription, Object, Schema};

fn legacy_thread() -> GqlThread {
    GqlThread {
        id: Some("t1".into()),
        emails: vec![serde_json::from_value(email_json("e0", "t1", true)).unwrap()],
    }
}

struct EmailFirst;

#[Object]
impl EmailFirst {
    async fn email(&self) -> GqlEmail {
        GqlEmail::full(legacy_thread().emails.remove(0))
    }
    async fn legacy_thread(&self) -> GqlThread {
        legacy_thread()
    }
    async fn loaded_thread(&self, ctx: &Context<'_>) -> async_graphql::Result<GqlThread> {
        GqlThread::load(ctx, "t1".into()).await
    }
}

struct LegacyFirst;

#[Object]
impl LegacyFirst {
    async fn legacy_thread(&self) -> GqlThread {
        GqlThread {
            id: None,
            emails: vec![],
        }
    }
    async fn email(&self) -> GqlEmail {
        GqlEmail::full(legacy_thread().emails.remove(0))
    }
}

#[tokio::test]
async fn legacy_thread_literals_register_once_and_keep_full_records() {
    let email_first = Schema::new(EmailFirst, EmptyMutation, EmptySubscription);
    let legacy_first = Schema::new(LegacyFirst, EmptyMutation, EmptySubscription);
    for sdl in [email_first.sdl(), legacy_first.sdl()] {
        assert_eq!(sdl.matches("type Thread {").count(), 1);
    }
    let response = email_first.execute("{ legacyThread { __typename ...Details } } fragment Details on Thread { id total emails(first:1) { nodes { id textBody } } }").await;
    assert!(response.errors.is_empty(), "{:?}", response.errors);
    assert_eq!(
        response.data.into_json().unwrap(),
        json!({"legacyThread":{
            "__typename":"Thread","id":"t1","total":1,"emails":{"nodes":[{"id":"e0","textBody":"Body of e0"}]}
        }})
    );
    let response = legacy_first
        .execute("{ legacyThread { id total emails { totalCount nodes { id } } } }")
        .await;
    assert!(response.errors.is_empty(), "{:?}", response.errors);
    assert_eq!(
        response.data.into_json().unwrap(),
        json!({"legacyThread":{"id":null,"total":0,"emails":{"totalCount":0,"nodes":[]}}})
    );

    let server = mock_server(3).await;
    let response = email_first
        .execute(request(
            "{ legacyThread { emails { nodes { textBody thread { __typename total } } } } }",
            client_for(&server),
            CardDavCreds::default(),
        ))
        .await;
    assert!(response.errors.is_empty(), "{:?}", response.errors);
    let calls = calls(&server).await;
    assert_eq!(calls.iter().filter(|c| c.method == "Thread/get").count(), 1);
    assert!(calls.iter().all(|c| c.method != "Email/get"));
}

#[tokio::test]
async fn public_thread_load_remains_eager_even_for_a_one_email_page() {
    let server = mock_server(3).await;
    let schema = Schema::new(EmailFirst, EmptyMutation, EmptySubscription);
    let response = schema
        .execute(request(
            "{ loadedThread { total emails(first:1) { nodes { textBody } } } }",
            client_for(&server),
            CardDavCreds::default(),
        ))
        .await;
    assert!(response.errors.is_empty(), "{:?}", response.errors);
    assert_eq!(
        response.data.into_json().unwrap()["loadedThread"]["total"],
        3
    );
    let calls = calls(&server).await;
    let gets: Vec<_> = calls.iter().filter(|c| c.method == "Email/get").collect();
    assert_eq!(gets.len(), 1);
    let mut ids = gets[0].ids.clone();
    ids.sort();
    assert_eq!(ids, ["e0", "e1", "e2"]);
    assert!(gets[0].properties.contains(&"textBody".into()));
}

#[tokio::test]
async fn eager_thread_loading_rejects_duplicate_or_empty_member_ids_before_fetching() {
    use wiremock::matchers::body_string_contains;
    for ids in [json!(["e0", "e0"]), json!([""])] {
        let server = mock_server(1).await;
        Mock::given(body_string_contains("Thread/get"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "methodResponses":[["Thread/get",{"accountId":"acct1","state":"s","notFound":[],"list":[{"id":"t1","emailIds":ids}]},"t0"]]
            }))).with_priority(1).mount(&server).await;
        let schema = Schema::new(EmailFirst, EmptyMutation, EmptySubscription);
        let response = schema
            .execute(request(
                "{ loadedThread { total emails(first:1) { edges { cursor node { id } } } } }",
                client_for(&server),
                CardDavCreds::default(),
            ))
            .await;
        assert_eq!(response.errors.len(), 1);
        assert!(
            response.errors[0]
                .message
                .contains("empty or duplicate email IDs")
        );
        assert!(
            calls(&server)
                .await
                .iter()
                .all(|call| call.method != "Email/get")
        );
    }
}

#[tokio::test]
async fn lazy_thread_id_counts_and_page_metadata_do_not_load_members() {
    for (selection, thread_gets) in [
        ("id", 0),
        ("total", 1),
        (
            "emails(first:1) { totalCount position queryState pageInfo { hasNextPage hasPreviousPage } }",
            1,
        ),
    ] {
        let server = mock_server(3).await;
        let response = run(
            &server,
            &format!("{{ thread(emailId:\"e0\") {{ {selection} }} }}"),
        )
        .await;
        assert!(response.errors.is_empty(), "{:?}", response.errors);
        let calls = calls(&server).await;
        assert_eq!(
            calls.iter().filter(|c| c.method == "Thread/get").count(),
            thread_gets
        );
        let gets: Vec<_> = calls.iter().filter(|c| c.method == "Email/get").collect();
        assert_eq!(gets.len(), 1);
        assert_eq!(gets[0].ids, ["e0"]);
        assert!(!gets[0].properties.contains(&"textBody".into()));
    }
    let server = mock_server(3).await;
    let response = run(&server, "{ email(id:\"e0\") { thread { id } } }").await;
    assert!(response.errors.is_empty(), "{:?}", response.errors);
    assert!(
        calls(&server)
            .await
            .iter()
            .all(|c| c.method != "Thread/get")
    );
}

#[tokio::test]
async fn lazy_thread_pages_fetch_only_selected_summaries_and_details() {
    for field in ["subject", "textBody"] {
        let server = mock_server(3).await;
        let response = run(&server, &format!("{{ thread(emailId:\"e0\") {{ a:emails(first:1,after:\"e0\") {{ ...Page }} b:emails(last:1,before:\"e2\") {{ ...Page }} }} }} fragment Page on EmailConnection {{ nodes {{ id {field} }} position totalCount queryState pageInfo {{ startCursor endCursor }} }}")).await;
        assert!(response.errors.is_empty(), "{:?}", response.errors);
        let data = response.data.into_json().unwrap();
        assert_eq!(data["thread"]["a"], data["thread"]["b"]);
        assert_eq!(data["thread"]["a"]["nodes"][0]["id"], "e1");
        assert_eq!(data["thread"]["a"]["position"], 1);
        assert_eq!(data["thread"]["a"]["totalCount"], 3);
        assert!(data["thread"]["a"]["queryState"].is_null());
        let calls = calls(&server).await;
        let details: Vec<_> = calls
            .iter()
            .filter(|c| c.method == "Email/get" && c.properties.contains(&"textBody".into()))
            .collect();
        assert_eq!(details.len(), usize::from(field == "textBody"));
        assert!(details.iter().all(|c| c.ids == ["e1"]));
        let summaries: Vec<_> = calls
            .iter()
            .filter(|c| c.method == "Email/get" && !c.properties.contains(&"textBody".into()))
            .collect();
        assert_eq!(summaries.len(), 2);
        assert_eq!(summaries[1].ids, ["e1"]);
    }
}

#[tokio::test]
async fn lazy_thread_order_is_the_server_member_order_not_get_response_order() {
    use wiremock::matchers::body_string_contains;
    let server = mock_server(3).await;
    Mock::given(body_string_contains("Thread/get")).respond_with(ResponseTemplate::new(200).set_body_json(json!({
        "methodResponses":[["Thread/get",{"accountId":"acct1","state":"s","notFound":[],"list":[{"id":"t1","emailIds":["e2","e0","e1"]}]},"t0"]]
    }))).with_priority(1).mount(&server).await;
    Mock::given(body_string_contains("Email/get")).respond_with(|request: &wiremock::Request| {
        let body: Value = serde_json::from_slice(&request.body).unwrap();
        let call = &body["methodCalls"][0];
        let emails: Vec<_> = call[1]["ids"].as_array().unwrap().iter().rev().map(|id| {
            let mut email = email_json(id.as_str().unwrap(), "t1", false);
            email["receivedAt"] = json!("2024-01-01T00:00:00Z");
            email
        }).collect();
        ResponseTemplate::new(200).set_body_json(json!({"methodResponses":[["Email/get",{"accountId":"acct1","state":"s","notFound":[],"list":emails},call[2]]]}))
    }).with_priority(1).mount(&server).await;
    let response = run(
        &server,
        "{ thread(emailId:\"e0\") { emails { nodes { id } } } }",
    )
    .await;
    assert!(response.errors.is_empty(), "{:?}", response.errors);
    assert_eq!(
        response.data.into_json().unwrap()["thread"]["emails"]["nodes"],
        json!([{"id":"e2"},{"id":"e0"},{"id":"e1"}])
    );
}

#[tokio::test]
async fn lazy_thread_rejects_ambiguous_ids_and_disappeared_members() {
    use wiremock::matchers::body_string_contains;
    for ids in [json!(["e1", "e1"]), json!([""]), json!(["e1"])] {
        let server = mock_server(2).await;
        Mock::given(body_string_contains("Thread/get")).respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "methodResponses":[["Thread/get",{"accountId":"acct1","state":"s","notFound":[],"list":[{"id":"t1","emailIds":ids}]},"t0"]]
        }))).with_priority(1).mount(&server).await;
        Mock::given(body_string_contains(r#""ids":["e1"]"#)).respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "methodResponses":[["Email/get",{"accountId":"acct1","state":"s","notFound":["e1"],"list":[]},"g0"]]
        }))).with_priority(1).mount(&server).await;
        let response = run(
            &server,
            "{ thread(emailId:\"e0\") { emails { nodes { id } } } }",
        )
        .await;
        assert_eq!(response.errors.len(), 1, "{:?}", response.errors);
        assert!(response.errors[0].message.contains("restart pagination"));
    }
}
