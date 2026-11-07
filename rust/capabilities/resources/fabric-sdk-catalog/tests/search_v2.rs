// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline Catalog v2 request and injected-execution contracts.

#![deny(unnameable_test_items)]

use azure_core::{
    credentials::{AccessToken, TokenRequestOptions},
    http::Method,
    time::OffsetDateTime,
};
use fabric_sdk_api::{
    ApiClient, ApiError, Audience, CredentialSelection, ExecutionError, Executor, Request,
    Response, TokenCredential,
};
use fabric_sdk_catalog::{
    CatalogClient, CatalogFilter, CatalogRequest, DEFAULT_CATALOG_PAGE_SIZE, SearchOptions,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    future::{Future, pending, poll_fn, ready},
    num::NonZeroUsize,
    pin::{Pin, pin},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    task::{Context, Poll, Waker},
};

const WORKSPACE: &str = "11111111-2222-3333-4444-555555555555";
const OTHER_WORKSPACE: &str = "AAAAAAAA-BBBB-CCCC-DDDD-EEEEEEEEEEEE";
const TOKEN: &str = "offline-catalog-test-token";

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

fn filter(includes: &[&str], excludes: &[&str], workspaces: &[&str]) -> CatalogFilter {
    CatalogFilter::new(strings(includes), strings(excludes), strings(workspaces)).unwrap()
}

fn initial() -> CatalogRequest {
    CatalogRequest::initial(Some("sales".into()), CatalogFilter::default(), None).unwrap()
}

fn body(request: &CatalogRequest) -> Value {
    let request = request.request().unwrap();
    assert_eq!(request.method(), Method::Post);
    assert_eq!(
        request.url().as_str(),
        "https://api.fabric.microsoft.com/v1/catalog/search"
    );
    serde_json::from_slice(request.body()).unwrap()
}

fn byte_limit() -> NonZeroUsize {
    NonZeroUsize::new(16_384).unwrap()
}

fn run_ready<T>(future: impl Future<Output = T>) -> T {
    let mut future = pin!(future);
    match future
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
    {
        Poll::Ready(value) => value,
        Poll::Pending => panic!("offline mock unexpectedly requires an executor"),
    }
}

#[derive(Debug, Default)]
struct Credential {
    scopes: Mutex<Vec<Vec<String>>>,
}

// Spell out the async-trait ABI to keep this leaf's tests dependency-free.
impl TokenCredential for Credential {
    fn get_token<'life0, 'life1, 'life2, 'life3, 'async_trait>(
        &'life0 self,
        scopes: &'life1 [&'life2 str],
        _: Option<TokenRequestOptions<'life3>>,
    ) -> Pin<Box<dyn Future<Output = azure_core::Result<AccessToken>> + Send + 'async_trait>>
    where
        'life0: 'async_trait,
        'life1: 'async_trait,
        'life2: 'async_trait,
        'life3: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move {
            self.scopes
                .lock()
                .unwrap()
                .push(scopes.iter().map(|value| (*value).to_owned()).collect());
            Ok(AccessToken::new(TOKEN, OffsetDateTime::UNIX_EPOCH))
        })
    }
}

struct DropCounter<'a>(&'a AtomicUsize);

impl Drop for DropCounter<'_> {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

struct RecordedRequest {
    method: Method,
    url: String,
    body: Value,
    byte_limit: usize,
}

#[derive(Default)]
struct MockExecutor {
    responses: Mutex<VecDeque<Response>>,
    requests: Mutex<Vec<RecordedRequest>>,
    pending: bool,
    dropped: AtomicUsize,
}

impl Executor for MockExecutor {
    fn execute<'a>(
        &'a self,
        request: &'a Request,
        token: &'a AccessToken,
        limit: NonZeroUsize,
    ) -> Pin<Box<dyn Future<Output = Result<Response, ExecutionError>> + Send + 'a>> {
        Box::pin(async move {
            assert_eq!(token.token.secret(), TOKEN);
            self.requests.lock().unwrap().push(RecordedRequest {
                method: request.method(),
                url: request.url().to_string(),
                body: serde_json::from_slice(request.body()).unwrap(),
                byte_limit: limit.get(),
            });
            let _guard = DropCounter(&self.dropped);
            if self.pending {
                pending::<()>().await;
            }
            Ok(self
                .responses
                .lock()
                .unwrap()
                .pop_front()
                .expect("extra request"))
        })
    }
}

fn response(status: u16, body: Value) -> Response {
    Response {
        status,
        body: serde_json::to_vec(&body).unwrap(),
        headers: BTreeMap::new(),
    }
}

fn client(
    responses: Vec<Response>,
    is_pending: bool,
) -> (CatalogClient, Arc<Credential>, Arc<MockExecutor>) {
    let credential = Arc::new(Credential::default());
    let executor = Arc::new(MockExecutor {
        responses: Mutex::new(responses.into()),
        pending: is_pending,
        ..MockExecutor::default()
    });
    let api = ApiClient::new(
        credential.clone(),
        CredentialSelection::new("offline-account", "offline-tenant", Audience::Fabric).unwrap(),
        executor.clone(),
    );
    (CatalogClient::new(api), credential, executor)
}

#[test]
fn search_only_body_defaults_and_read_only_fields_are_exact() {
    let request = initial();
    assert_eq!(DEFAULT_CATALOG_PAGE_SIZE, 50);
    assert_eq!(body(&request), json!({"search":"sales","pageSize":50}));
    let CatalogRequest::Initial(fields) = request else {
        panic!("initial variant");
    };
    assert_eq!(fields.search(), Some("sales"));
    assert!(fields.filter().is_empty());
    assert_eq!(fields.page_size().get(), 50);
}

#[test]
fn initial_page_size_boundaries_are_validated_without_io() {
    for size in [1, 1000] {
        let request =
            CatalogRequest::initial(Some("query".into()), CatalogFilter::default(), Some(size))
                .unwrap();
        assert_eq!(body(&request), json!({"search":"query","pageSize":size}));
    }
    for size in [0, 1001, usize::MAX] {
        assert_eq!(
            CatalogRequest::initial(Some("query".into()), CatalogFilter::default(), Some(size)),
            Err(ApiError::InvalidInput)
        );
    }
}

#[test]
fn typed_filter_groups_inclusions_exclusions_and_workspaces() {
    let filter = filter(
        &["Report", "Future.Type"],
        &["Notebook", "Dashboard"],
        &[WORKSPACE, OTHER_WORKSPACE],
    );
    assert_eq!(filter.include_types(), strings(&["Report", "Future.Type"]));
    assert_eq!(filter.exclude_types(), strings(&["Notebook", "Dashboard"]));
    assert_eq!(
        filter.workspace_ids(),
        strings(&[WORKSPACE, OTHER_WORKSPACE])
    );
    let request =
        CatalogRequest::initial(Some(" sales && \"monthly\" ".into()), filter, Some(1)).unwrap();
    assert_eq!(
        body(&request),
        json!({
            "search":" sales && \"monthly\" ",
            "pageSize":1,
            "filter":format!(
                "(Type eq 'Report' or Type eq 'Future.Type') and \
                 (Type ne 'Notebook' and Type ne 'Dashboard') and \
                 (WorkspaceId eq '{WORKSPACE}' or WorkspaceId eq '{OTHER_WORKSPACE}')"
            )
        })
    );
}

#[test]
fn filter_only_is_supported_but_empty_initial_search_is_not() {
    for filter in [
        filter(&["Report"], &[], &[]),
        filter(&[], &["Report"], &[]),
        filter(&[], &[], &[WORKSPACE]),
    ] {
        let request = CatalogRequest::initial(None, filter.clone(), None).unwrap();
        let payload = body(&request);
        assert_eq!(payload.as_object().unwrap().len(), 2);
        assert!(payload.get("search").is_none());
        assert!(payload["filter"].is_string());
        for blank in ["", " ", "\t\r\n"] {
            assert_eq!(
                CatalogRequest::initial(Some(blank.into()), filter.clone(), None),
                Err(ApiError::InvalidInput)
            );
        }
    }
    assert_eq!(
        CatalogRequest::initial(None, CatalogFilter::default(), None),
        Err(ApiError::InvalidInput)
    );
    assert!(
        CatalogFilter::new(vec![], vec![], vec![])
            .unwrap()
            .is_empty()
    );
}

#[test]
fn type_cardinality_is_combined_and_does_not_deduplicate_away_limits() {
    let filter = CatalogFilter::new(
        vec!["Report".into(); 250],
        vec!["Future".into(); 250],
        vec![],
    )
    .unwrap();
    assert_eq!(
        filter.include_types().len() + filter.exclude_types().len(),
        500
    );
    assert!(CatalogRequest::initial(None, filter, None).is_ok());
    for (includes, excludes) in [(500, 1), (0, 501), (501, 0)] {
        assert_eq!(
            CatalogFilter::new(
                vec!["Report".into(); includes],
                vec!["Future".into(); excludes],
                vec![]
            ),
            Err(ApiError::InvalidInput)
        );
    }
}

#[test]
fn type_lengths_count_characters_and_reject_quotes_controls_and_blanks() {
    for accepted in ["a".repeat(50), "界".repeat(50), "Unknown\\Type (v2)".into()] {
        let request = CatalogRequest::initial(
            None,
            CatalogFilter::new(vec![accepted.clone()], vec![], vec![]).unwrap(),
            None,
        )
        .unwrap();
        assert_eq!(body(&request)["filter"], format!("(Type eq '{accepted}')"));
    }
    for rejected in [
        "a".repeat(51),
        "界".repeat(51),
        String::new(),
        " ".into(),
        "' or Type ne 'Report".into(),
        "\"Report\"".into(),
        "Report\n".into(),
        "Report\t".into(),
        "Report\0".into(),
        "Report\u{7f}".into(),
        "Report\u{85}".into(),
    ] {
        assert_eq!(
            CatalogFilter::new(vec![rejected.clone()], vec![], vec![]),
            Err(ApiError::InvalidInput)
        );
        assert_eq!(
            CatalogFilter::new(vec![], vec![rejected], vec![]),
            Err(ApiError::InvalidInput)
        );
    }
}

#[test]
fn workspace_cardinality_and_exact_guid_syntax_are_checked() {
    assert!(CatalogFilter::new(vec![], vec![], vec![WORKSPACE.into(); 12]).is_ok());
    assert_eq!(
        CatalogFilter::new(vec![], vec![], vec![WORKSPACE.into(); 13]),
        Err(ApiError::InvalidInput)
    );
    for rejected in [
        "",
        " ",
        "11111111222233334444555555555555",
        "{11111111-2222-3333-4444-555555555555}",
        "11111111-2222-3333-4444-55555555555g",
        "11111111_2222-3333-4444-555555555555",
        "11111111-2222-3333-4444-555555555555 ",
        "11111111-2222-3333-4444-55555555555'",
        "11111111-2222-3333-4444-55555555555\n",
    ] {
        assert_eq!(
            CatalogFilter::new(vec![], vec![], strings(&[rejected])),
            Err(ApiError::InvalidInput)
        );
    }
    let request =
        CatalogRequest::initial(None, filter(&[], &[], &[OTHER_WORKSPACE]), None).unwrap();
    assert_eq!(
        body(&request)["filter"],
        format!("(WorkspaceId eq '{OTHER_WORKSPACE}')")
    );
}

#[test]
fn resume_is_token_only_and_retains_every_token_byte() {
    let token = " \t+opaque/=%?\"'\\\n界\0 ";
    let request = CatalogRequest::resume(token.into()).unwrap();
    assert_eq!(body(&request), json!({"continuationToken":token}));
    let CatalogRequest::Resume(fields) = &request else {
        panic!("resume variant");
    };
    assert_eq!(fields.token().as_bytes(), token.as_bytes());
    for blank in ["", " ", "\r\n\t"] {
        assert_eq!(
            CatalogRequest::resume(blank.into()),
            Err(ApiError::InvalidInput)
        );
    }
    assert!(!format!("{request:?}").contains("opaque"));
    assert!(!format!("{:?}", initial()).contains("sales"));
}

#[test]
fn page_uses_only_injected_fabric_dependencies_and_preserves_unknown_rows() {
    let rows = json!([
        {"id":"one","type":"Future.Type","catalogEntryType":"FutureEntry","extra":{"kept":true}},
        {"id":"workspace","type":"Workspace","catalogEntryType":"Workspace"},
        {"id":"one","type":"Future.Type","catalogEntryType":"FutureEntry","extra":{"kept":true}}
    ]);
    let (client, credential, executor) = client(
        vec![response(
            200,
            json!({"value":rows,"continuationToken":"next"}),
        )],
        false,
    );
    let page = run_ready(client.search_v2_page(&initial(), byte_limit(), pending())).unwrap();
    assert_eq!(page.rows, rows.as_array().unwrap().clone());
    assert_eq!(page.next_token.as_deref(), Some("next"));
    assert_eq!(
        *credential.scopes.lock().unwrap(),
        [vec![Audience::Fabric.scope().to_owned()]]
    );
    let requests = executor.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, Method::Post);
    assert_eq!(
        requests[0].url,
        "https://api.fabric.microsoft.com/v1/catalog/search"
    );
    assert_eq!(requests[0].body, json!({"search":"sales","pageSize":50}));
    assert_eq!(requests[0].byte_limit, byte_limit().get());
}

#[test]
fn resume_executes_one_token_only_page_and_ignores_continuation_urls() {
    let token = "  service-token+/=界  ";
    let (client, _, executor) = client(
        vec![response(
            200,
            json!({
                "value":[],
                "continuationToken":token,
                "continuationUri":"https://untrusted.invalid/never-follow"
            }),
        )],
        false,
    );
    let request = CatalogRequest::resume("original-token".into()).unwrap();
    let page = run_ready(client.search_v2_page(&request, byte_limit(), pending())).unwrap();
    assert!(page.rows.is_empty());
    assert_eq!(page.next_token.as_deref(), Some(token));
    let requests = executor.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].body,
        json!({"continuationToken":"original-token"})
    );
}

#[test]
fn missing_null_and_empty_next_tokens_are_terminal() {
    for payload in [
        json!({"value":[]}),
        json!({"value":[],"continuationToken":null}),
        json!({"value":[],"continuationToken":""}),
    ] {
        let (client, _, _) = client(vec![response(200, payload)], false);
        let page = run_ready(client.search_v2_page(&initial(), byte_limit(), pending())).unwrap();
        assert!(page.rows.is_empty());
        assert_eq!(page.next_token, None);
    }
}

#[test]
fn malformed_rows_and_page_shapes_are_not_empty_successes() {
    for payload in [
        json!(null),
        json!([]),
        json!({}),
        json!({"value":null}),
        json!({"value":{}}),
        json!({"value":[null]}),
        json!({"value":[1]}),
        json!({"value":[{"id":"valid"},false]}),
    ] {
        let (client, _, _) = client(vec![response(200, payload)], false);
        assert_eq!(
            run_ready(client.search_v2_page(&initial(), byte_limit(), pending())),
            Err(ApiError::InvalidResponse)
        );
    }
    for token in [json!(false), json!(0), json!([]), json!({})] {
        let (client, _, _) = client(
            vec![response(200, json!({"value":[],"continuationToken":token}))],
            false,
        );
        assert_eq!(
            run_ready(client.search_v2_page(&initial(), byte_limit(), pending())),
            Err(ApiError::Continuation)
        );
    }
    let (client, _, _) = client(
        vec![Response {
            status: 200,
            body: b"not-json".to_vec(),
            headers: BTreeMap::new(),
        }],
        false,
    );
    assert_eq!(
        run_ready(client.search_v2_page(&initial(), byte_limit(), pending())),
        Err(ApiError::InvalidJson)
    );
}

#[test]
fn incomplete_statuses_and_service_denials_are_failures_without_replay() {
    for status in [201, 202, 204] {
        let (client, _, executor) = client(vec![response(status, json!({"value":[]}))], false);
        assert_eq!(
            run_ready(client.search_v2_page(&initial(), byte_limit(), pending())),
            Err(ApiError::UnsupportedStatus(status))
        );
        assert_eq!(executor.requests.lock().unwrap().len(), 1);
    }
    for status in [401, 403, 429, 500] {
        let (client, _, executor) = client(
            vec![response(
                status,
                json!({"errorCode":"DeniedOrThrottled","message":"service text"}),
            )],
            false,
        );
        assert_eq!(
            run_ready(client.search_v2_page(&initial(), byte_limit(), pending())),
            Err(ApiError::Service {
                status,
                code: "DeniedOrThrottled".into(),
                message: "service text".into()
            })
        );
        assert_eq!(executor.requests.lock().unwrap().len(), 1);
    }
}

#[test]
fn response_bytes_are_bounded_by_the_shared_api_client() {
    let (client, _, executor) = client(vec![response(200, json!({"value":[]}))], false);
    assert_eq!(
        run_ready(client.search_v2_page(&initial(), NonZeroUsize::new(1).unwrap(), pending())),
        Err(ApiError::Limit)
    );
    assert_eq!(executor.requests.lock().unwrap()[0].byte_limit, 1);
}

#[test]
fn pre_cancelled_request_does_not_acquire_credentials_or_dispatch() {
    let (client, credential, executor) = client(vec![], false);
    assert_eq!(
        run_ready(client.search_v2_page(&initial(), byte_limit(), ready(()))),
        Err(ApiError::Cancelled)
    );
    assert!(credential.scopes.lock().unwrap().is_empty());
    assert!(executor.requests.lock().unwrap().is_empty());
}

#[test]
fn cancellation_drops_pending_injected_execution() {
    let (client, _, executor) = client(vec![], true);
    let cancelled = AtomicBool::new(false);
    let cancellation = poll_fn(|_| {
        if cancelled.load(Ordering::SeqCst) {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    });
    let request = initial();
    let mut future = pin!(client.search_v2_page(&request, byte_limit(), cancellation));
    let mut context = Context::from_waker(Waker::noop());
    assert!(future.as_mut().poll(&mut context).is_pending());
    assert_eq!(executor.requests.lock().unwrap().len(), 1);
    assert_eq!(executor.dropped.load(Ordering::SeqCst), 0);
    cancelled.store(true, Ordering::SeqCst);
    assert_eq!(
        future.as_mut().poll(&mut context),
        Poll::Ready(Err(ApiError::Cancelled))
    );
    assert_eq!(executor.dropped.load(Ordering::SeqCst), 1);
}

#[test]
fn legacy_options_keep_their_original_search_and_resume_contracts() {
    let legacy = SearchOptions {
        search: "legacy".into(),
        page_size: NonZeroUsize::new(30).unwrap(),
        filter: Some("Type eq 'Report'".into()),
    };
    let first = legacy.request(None).unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(first.body()).unwrap(),
        json!({"search":"legacy","pageSize":30,"filter":"Type eq 'Report'"})
    );
    let next = legacy.request(Some("legacy-token")).unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(next.body()).unwrap(),
        json!({"continuationToken":"legacy-token","pageSize":30})
    );
}
