// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline public SDK contracts for bounded workspace reads over real send/collect.

#![deny(unnameable_test_items)]

use azure_core::{
    credentials::{AccessToken, TokenRequestOptions},
    error::ErrorKind,
    http::Method,
    time::OffsetDateTime,
};
use fabric_sdk_api::{
    ApiClient, ApiError, Audience, CredentialSelection, ExecutionError, Executor, Limits, Origin,
    Paging, Request, Response, TokenCredential,
    workspace_reads::{
        WorkspaceReadError as ReadError, WorkspaceReadKind as Kind, WorkspaceReadRequest,
        read_workspace,
    },
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

const WORKSPACE: &str = "a1234567-89ab-cdef-0123-456789abcdef";
const OTHER_WORKSPACE: &str = "b1234567-89ab-cdef-0123-456789abcdef";
const TOKEN: &str = "offline-workspace-read-token";
const RESPONSE_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Default)]
struct Credential {
    scopes: Mutex<Vec<Vec<String>>>,
    hold: bool,
    fail: bool,
    dropped: AtomicUsize,
}

// Use the same Azure async-trait ABI as the dependency-free core request tests.
impl TokenCredential for Credential {
    fn get_token<'s, 'scopes, 'scope, 'options, 'future>(
        &'s self,
        scopes: &'scopes [&'scope str],
        options: Option<TokenRequestOptions<'options>>,
    ) -> Pin<Box<dyn Future<Output = azure_core::Result<AccessToken>> + Send + 'future>>
    where
        's: 'future,
        'scopes: 'future,
        'scope: 'future,
        'options: 'future,
        Self: 'future,
    {
        Box::pin(async move {
            assert!(options.is_none());
            assert_eq!(scopes, [Audience::Fabric.scope()]);
            self.scopes
                .lock()
                .unwrap()
                .push(scopes.iter().map(|scope| (*scope).to_owned()).collect());
            if self.hold {
                let _guard = Dropped(&self.dropped);
                pending::<()>().await;
            }
            if self.fail {
                return Err(azure_core::Error::with_message(
                    ErrorKind::Other,
                    "private-provider-secret",
                ));
            }
            Ok(AccessToken::new(TOKEN, OffsetDateTime::UNIX_EPOCH))
        })
    }
}

struct Dropped<'a>(&'a AtomicUsize);

impl Drop for Dropped<'_> {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[derive(Debug, PartialEq)]
struct Observed {
    url: String,
    limit: usize,
}

struct Backend {
    replies: Mutex<VecDeque<Result<Response, ExecutionError>>>,
    requests: Mutex<Vec<Observed>>,
    hold: bool,
    supported: bool,
    dropped: AtomicUsize,
    cancel_after_response: Option<Arc<AtomicBool>>,
}

impl Default for Backend {
    fn default() -> Self {
        Self {
            replies: Mutex::default(),
            requests: Mutex::default(),
            hold: false,
            supported: true,
            dropped: AtomicUsize::default(),
            cancel_after_response: None,
        }
    }
}

impl Backend {
    fn new(responses: impl IntoIterator<Item = Response>) -> Self {
        Self {
            replies: Mutex::new(responses.into_iter().map(Ok).collect()),
            ..Self::default()
        }
    }
}

impl Executor for Backend {
    fn supports(&self, request: &Request) -> bool {
        self.supported && request.origin() == Origin::Fabric && request.headers().is_empty()
    }

    fn execute<'a>(
        &'a self,
        request: &'a Request,
        token: &'a AccessToken,
        limit: NonZeroUsize,
    ) -> Pin<Box<dyn Future<Output = Result<Response, ExecutionError>> + Send + 'a>> {
        Box::pin(async move {
            assert_eq!(request.method(), Method::Get);
            assert!(request.body().is_empty());
            assert_eq!(token.token.secret(), TOKEN);
            self.requests.lock().unwrap().push(Observed {
                url: request.url().to_string(),
                limit: limit.get(),
            });
            if self.hold {
                let _guard = Dropped(&self.dropped);
                pending::<()>().await;
            }
            if let Some(flag) = &self.cancel_after_response {
                flag.store(true, Ordering::SeqCst);
            }
            self.replies
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected additional request")
        })
    }
}

fn nonzero(value: usize) -> NonZeroUsize {
    NonZeroUsize::new(value).unwrap()
}

fn response(status: u16, data: Value) -> Response {
    raw_response(status, serde_json::to_vec(&data).unwrap())
}

fn raw_response(status: u16, body: Vec<u8>) -> Response {
    Response {
        status,
        body,
        headers: BTreeMap::new(),
    }
}

fn metadata() -> Value {
    json!({"id":WORKSPACE,"displayName":"Viewer space"})
}

fn item(id: &str) -> Value {
    json!({"id":id,"displayName":format!("Item {id}"),"type":"Notebook"})
}

fn request(kind: Kind, records: usize, bytes: usize) -> WorkspaceReadRequest {
    match kind {
        Kind::Metadata => WorkspaceReadRequest::metadata(WORKSPACE, nonzero(bytes)),
        Kind::Items => {
            WorkspaceReadRequest::items(WORKSPACE, nonzero(records), nonzero(bytes), false)
        }
        Kind::Workspaces => Ok(WorkspaceReadRequest::workspaces(
            nonzero(records),
            nonzero(bytes),
        )),
    }
    .unwrap()
}

fn payload(kind: Kind) -> Value {
    match kind {
        Kind::Metadata => metadata(),
        Kind::Items => json!({"value":[item("one")]}),
        Kind::Workspaces => {
            json!({"value":[{"id":WORKSPACE,"displayName":"Viewer space","type":"Workspace"}]})
        }
    }
}

fn client(credential: Arc<Credential>, backend: Arc<Backend>) -> ApiClient {
    client_with_audience(credential, backend, Audience::Fabric)
}

fn client_with_audience(
    credential: Arc<Credential>,
    backend: Arc<Backend>,
    audience: Audience,
) -> ApiClient {
    ApiClient::new(
        credential,
        CredentialSelection::new("offline-account", "offline-tenant", audience).unwrap(),
        backend,
    )
}

fn run<T>(future: impl Future<Output = T>) -> T {
    let mut future = pin!(future);
    let mut context = Context::from_waker(Waker::noop());
    for _ in 0..32 {
        if let Poll::Ready(result) = future.as_mut().poll(&mut context) {
            return result;
        }
    }
    panic!("offline future did not complete within its finite poll budget")
}

fn signal(flag: Arc<AtomicBool>) -> impl Future<Output = ()> + Send {
    poll_fn(move |_| {
        if flag.load(Ordering::SeqCst) {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    })
}

fn assert_invalid_continuation(kind: Kind, reply: Response) {
    let credential = Arc::new(Credential::default());
    let backend = Arc::new(Backend::new([reply, response(200, payload(kind))]));
    let client = client(credential.clone(), backend.clone());
    let error = run(read_workspace(&client, &request(kind, 1, 4096), pending())).unwrap_err();
    assert_eq!(error, ReadError::InvalidResponse);
    assert!(!format!("{error} {error:?}").contains("opaque-private"));
    assert_eq!(backend.requests.lock().unwrap().len(), 1);
    assert_eq!(credential.scopes.lock().unwrap().len(), 1);
    assert_eq!(backend.replies.lock().unwrap().len(), 1);
}

#[test]
fn uuid_constructors_canonicalize_only_exact_ascii_hex_layout() {
    let uppercase = WORKSPACE.to_ascii_uppercase();
    let metadata = WorkspaceReadRequest::metadata(&uppercase, nonzero(300)).unwrap();
    assert_eq!(metadata.workspace_id(), Some(WORKSPACE));
    assert_eq!(metadata.kind(), Kind::Metadata);
    assert_eq!(metadata.max_data_bytes(), nonzero(300));
    let items = WorkspaceReadRequest::items(&uppercase, nonzero(2), nonzero(400), true).unwrap();
    assert_eq!(items.workspace_id(), Some(WORKSPACE));
    assert_eq!(items.kind(), Kind::Items);
    assert_eq!(items.max_data_bytes(), nonzero(400));
    assert!(!format!("{metadata:?}").contains(WORKSPACE));
    assert!(!format!("{items:?}").contains(WORKSPACE));
    for invalid in [
        "",
        "workspace",
        "a123456789abcdef0123456789abcdef",
        "a1234567_89ab-cdef-0123-456789abcdef",
        "a1234567-89ab-cdef-0123-456789abcdeg",
        "a1234567-89ab-cdef-0123-456789abcdeé",
        "{a1234567-89ab-cdef-0123-456789abcdef}",
        "/a1234567-89ab-cdef-0123-456789abcdef",
        "a1234567-89ab-cdef-0123-456789abcdef ",
        " a1234567-89ab-cdef-0123-456789abcdef",
        "a1234567-89ab-cdef-0123-456789abcdef/items",
        "a1234567-89ab-cdef-0123-456789abcdef?x=y",
        "../a1234567-89ab-cdef-0123-456789abcdef",
    ] {
        assert_eq!(
            WorkspaceReadRequest::metadata(invalid, nonzero(1)).unwrap_err(),
            ReadError::InvalidInput
        );
        assert_eq!(
            WorkspaceReadRequest::items(invalid, nonzero(1), nonzero(1), false).unwrap_err(),
            ReadError::InvalidInput
        );
    }
}

#[test]
fn root_workspaces_preserve_native_identity_order_unknown_types_and_exact_bounds() {
    let first = json!({"id":WORKSPACE,"displayName":"Same name","type":"FutureWorkspace",
        "capacityId":null,"future":{"n":1}});
    let second = json!({"id":OTHER_WORKSPACE,"displayName":"Same name","type":"Personal"});
    for rows in [
        vec![],
        vec![first.clone()],
        vec![second, first.clone(), first],
    ] {
        let bytes = serde_json::to_vec(&rows).unwrap().len();
        let request = WorkspaceReadRequest::workspaces(nonzero(3), nonzero(bytes));
        assert_eq!(request.workspace_id(), None);
        assert_eq!(request.kind(), Kind::Workspaces);
        let credential = Arc::new(Credential::default());
        let backend = Arc::new(Backend::new([response(200, json!({"value":rows}))]));
        let client = client(credential.clone(), backend.clone());
        let result = run(read_workspace(&client, &request, pending())).unwrap();
        assert_eq!(result.data, json!(rows));
        assert!(result.complete);
        let page = result.page.unwrap();
        assert_eq!(page.pages_received, 1);
        assert_eq!(page.records_observed, rows.len());
        assert_eq!(page.records_returned, rows.len());
        assert!(!page.has_more);
        assert!(!page.output_truncated);
        assert_eq!(page.native_continuation, None);
        assert_eq!(
            *backend.requests.lock().unwrap(),
            [Observed {
                url: "https://api.fabric.microsoft.com/v1/workspaces".into(),
                limit: RESPONSE_BYTES,
            }]
        );
        assert_eq!(credential.scopes.lock().unwrap().len(), 1);
    }
}

#[test]
fn native_next_page_is_evidence_only_and_never_skips_clipped_rows() {
    for kind in [Kind::Items, Kind::Workspaces] {
        let row = if kind == Kind::Items {
            item("one")
        } else {
            json!({"id":WORKSPACE,"displayName":"First","type":"Workspace"})
        };
        let second = if kind == Kind::Items {
            item("two")
        } else {
            json!({"id":OTHER_WORKSPACE,"displayName":"Second","type":"Workspace"})
        };
        for (rows, records, bytes, omitted) in [
            (vec![], 2, 4096, false),
            (vec![row.clone()], 1, 4096, false),
            (vec![row.clone(), second.clone()], 1, 4096, true),
            (
                vec![row.clone(), second],
                2,
                serde_json::to_vec(&vec![row.clone()]).unwrap().len(),
                true,
            ),
        ] {
            let mut reply = response(200, json!({"value":rows}));
            reply
                .headers
                .insert("X-Ms-Continuation".into(), "private-next-page".into());
            let backend = Arc::new(Backend::new([reply, response(200, payload(kind))]));
            let client = client(Arc::new(Credential::default()), backend.clone());
            let result = run(read_workspace(
                &client,
                &request(kind, records, bytes),
                pending(),
            ))
            .unwrap();
            assert!(!result.complete);
            assert!(result.truncated);
            let page = result.page.unwrap();
            assert_eq!(page.records_observed, rows.len());
            assert_eq!(page.records_returned, if omitted { 1 } else { rows.len() });
            assert!(page.has_more);
            assert_eq!(page.output_truncated, omitted);
            assert_eq!(
                page.native_continuation.as_deref(),
                if omitted {
                    None
                } else {
                    Some("private-next-page")
                }
            );
            assert!(!format!("{page:?}").contains("private-next-page"));
            assert_eq!(backend.requests.lock().unwrap().len(), 1);
            assert_eq!(backend.replies.lock().unwrap().len(), 1);
        }
    }
}

#[test]
fn root_missing_or_wrong_native_identity_and_unsafe_next_are_not_empty_lists() {
    for page in [
        json!({}),
        json!({"value":null}),
        json!({"value":{}}),
        json!({"value":[{"id":"not-a-uuid","displayName":"Name","type":"Workspace"}]}),
        json!({"value":[{"id":WORKSPACE,"displayName":"Name"}]}),
        json!({"value":[{"id":WORKSPACE,"type":"Workspace"}]}),
        json!({"value":[],"continuationUri":"https://api.fabric.microsoft.com/v1/workspaces?continuationToken=next"}),
        json!({"value":[],"continuationToken":"next","continuationUri":"https://untrusted.invalid/v1/workspaces"}),
        json!({"value":[],"continuationToken":"next","continuationUri":format!("https://api.fabric.microsoft.com/v1/workspaces/{WORKSPACE}/items")}),
    ] {
        assert_invalid_continuation(Kind::Workspaces, response(200, page));
    }
}

#[test]
fn viewer_primary_is_exactly_one_get_without_enrichment_or_discovery() {
    let data = json!({
        "id":WORKSPACE.to_ascii_uppercase(),
        "displayName":"Viewer space",
        "description":"Unmodified",
        "capacityId":"capacity-private",
        "future":{"array":[1,"é"],"nullable":null},
        "apiEndpoint":"https://untrusted.invalid/do-not-follow"
    });
    let credential = Arc::new(Credential::default());
    let backend = Arc::new(Backend::new([response(200, data.clone())]));
    let client = client(credential.clone(), backend.clone());
    let request =
        WorkspaceReadRequest::metadata(&WORKSPACE.to_ascii_uppercase(), nonzero(4096)).unwrap();
    let result = run(read_workspace(&client, &request, pending())).unwrap();
    assert_eq!(result.data, data);
    assert!(result.complete);
    assert!(!result.truncated);
    assert_eq!(
        *backend.requests.lock().unwrap(),
        [Observed {
            url: format!("https://api.fabric.microsoft.com/v1/workspaces/{WORKSPACE}"),
            limit: RESPONSE_BYTES,
        }]
    );
    assert_eq!(
        *credential.scopes.lock().unwrap(),
        [vec![Audience::Fabric.scope().to_owned()]]
    );
    let debug = format!("{result:?}");
    for private in [
        WORKSPACE,
        "Viewer space",
        "capacity-private",
        "untrusted.invalid",
    ] {
        assert!(!debug.contains(private));
    }
}

#[test]
fn primary_metadata_rejects_active_or_malformed_continuations_without_extra_requests() {
    for field in ["continuationToken", "continuationUri"] {
        for value in [
            json!("opaque-private-next"),
            json!(" "),
            json!(false),
            json!(true),
            json!(0),
            json!(1),
            json!([]),
            json!({}),
            json!(["opaque-private-next"]),
        ] {
            let mut data = metadata();
            data[field] = value;
            assert_invalid_continuation(Kind::Metadata, response(200, data));
        }
    }
    for name in ["x-ms-continuation", "X-Ms-Continuation"] {
        for value in ["opaque-private-next", " "] {
            let mut reply = response(200, metadata());
            reply.headers.insert(name.into(), value.into());
            assert_invalid_continuation(Kind::Metadata, reply);
        }
    }
}

#[test]
fn primary_metadata_preserves_only_inert_null_or_empty_continuation_fields() {
    for token in [Value::Null, json!("")] {
        for uri in [Value::Null, json!("")] {
            for empty_header in [false, true] {
                let mut data = metadata();
                data["continuationToken"] = token.clone();
                data["continuationUri"] = uri.clone();
                let mut reply = response(200, data.clone());
                if empty_header {
                    reply.headers.insert("X-Ms-Continuation".into(), "".into());
                }
                let credential = Arc::new(Credential::default());
                let backend = Arc::new(Backend::new([reply, response(200, metadata())]));
                let client = client(credential.clone(), backend.clone());
                let result = run(read_workspace(
                    &client,
                    &request(Kind::Metadata, 1, 4096),
                    pending(),
                ))
                .unwrap();
                assert_eq!(result.data, data);
                assert!(result.complete);
                assert!(!result.truncated);
                assert_eq!(backend.requests.lock().unwrap().len(), 1);
                assert_eq!(credential.scopes.lock().unwrap().len(), 1);
                assert_eq!(backend.replies.lock().unwrap().len(), 1);
            }
        }
    }
}

#[test]
fn item_arrays_keep_empty_single_many_order_duplicates_unknowns_and_explicit_recursion() {
    for recursive in [false, true] {
        for rows in [
            vec![],
            vec![item("one")],
            vec![
                json!({"id":"two","displayName":"Second","type":"FutureType",
                    "workspaceId":WORKSPACE.to_ascii_uppercase(),"future":{"n":2}}),
                item("one"),
                item("one"),
            ],
        ] {
            let credential = Arc::new(Credential::default());
            let backend = Arc::new(Backend::new([response(200, json!({"value":rows}))]));
            let client = client(credential.clone(), backend.clone());
            let request = WorkspaceReadRequest::items(
                WORKSPACE,
                nonzero(rows.len().max(1)),
                nonzero(4096),
                recursive,
            )
            .unwrap();
            let result = run(read_workspace(&client, &request, pending())).unwrap();
            assert_eq!(result.data, Value::Array(rows));
            assert!(result.complete);
            assert!(!result.truncated);
            assert_eq!(
                *backend.requests.lock().unwrap(),
                [Observed {
                    url: format!(
                        "https://api.fabric.microsoft.com/v1/workspaces/{WORKSPACE}/items?recursive={recursive}"
                    ),
                    limit: RESPONSE_BYTES,
                }]
            );
            assert_eq!(credential.scopes.lock().unwrap().len(), 1);
        }
    }
}

#[test]
fn metadata_uses_exact_compact_utf8_budget_not_wire_length_or_escaped_character_count() {
    let compact = format!(r#"{{"id":"{WORKSPACE}","displayName":"é\n\"\\🦀","future":"\u0001"}}"#);
    let wire = format!(
        r#"{{ "id" : "{WORKSPACE}", "displayName" : "\u00e9\n\"\\\ud83e\udd80", "future" : "\u0001" }}"#
    );
    let expected: Value = serde_json::from_str(&compact).unwrap();
    assert_eq!(serde_json::to_vec(&expected).unwrap(), compact.as_bytes());
    assert!(wire.len() > compact.len());
    for bytes in [compact.len(), compact.len() - 1] {
        let backend = Arc::new(Backend::new([raw_response(200, wire.as_bytes().to_vec())]));
        let client = client(Arc::new(Credential::default()), backend.clone());
        let result = run(read_workspace(
            &client,
            &request(Kind::Metadata, 1, bytes),
            pending(),
        ));
        if bytes == compact.len() {
            let result = result.unwrap();
            assert_eq!(result.data, expected);
            assert!(result.complete);
        } else {
            assert_eq!(result.unwrap_err(), ReadError::BudgetExceeded);
        }
        assert_eq!(backend.requests.lock().unwrap()[0].limit, RESPONSE_BYTES);
    }
}

#[test]
fn item_budget_counts_brackets_commas_utf8_escapes_and_preserves_whole_rows() {
    let first = r#"{"id":"one","displayName":"é\n\"\\🦀","type":"Notebook","future":"\u0001"}"#;
    let second = r#"{"id":"two","displayName":"Second","type":"Lakehouse"}"#;
    let compact_one = format!("[{first}]");
    let compact_two = format!("[{first},{second}]");
    let one: Value = serde_json::from_str(&compact_one).unwrap();
    let two: Value = serde_json::from_str(&compact_two).unwrap();
    assert_eq!(serde_json::to_vec(&one).unwrap(), compact_one.as_bytes());
    assert_eq!(serde_json::to_vec(&two).unwrap(), compact_two.as_bytes());
    for (rows, bytes, expected, truncated) in [
        (one.clone(), compact_one.len(), one.clone(), false),
        (two.clone(), compact_two.len(), two.clone(), false),
        (two.clone(), compact_two.len() - 1, one.clone(), true),
        (two.clone(), compact_one.len(), one.clone(), true),
    ] {
        let backend = Arc::new(Backend::new([response(200, json!({"value":rows}))]));
        let client = client(Arc::new(Credential::default()), backend.clone());
        let result = run(read_workspace(
            &client,
            &request(Kind::Items, 10, bytes),
            pending(),
        ))
        .unwrap();
        assert_eq!(result.data, expected);
        assert_eq!(result.truncated, truncated);
        assert_eq!(result.complete, !truncated);
        assert_eq!(backend.requests.lock().unwrap().len(), 1);
        assert_eq!(backend.requests.lock().unwrap()[0].limit, RESPONSE_BYTES);
    }
    let backend = Arc::new(Backend::new([response(200, json!({"value":one}))]));
    let client = client(Arc::new(Credential::default()), backend);
    assert_eq!(
        run(read_workspace(
            &client,
            &request(Kind::Items, 10, compact_one.len() - 1),
            pending()
        ))
        .unwrap_err(),
        ReadError::BudgetExceeded
    );
}

#[test]
fn empty_array_requires_two_bytes_and_an_unfittable_first_row_is_not_empty_success() {
    for (rows, bytes, success) in [
        (json!([]), 1, false),
        (json!([]), 2, true),
        (json!([item("one")]), 2, false),
    ] {
        let backend = Arc::new(Backend::new([response(200, json!({"value":rows}))]));
        let client = client(Arc::new(Credential::default()), backend);
        let result = run(read_workspace(
            &client,
            &request(Kind::Items, 1, bytes),
            pending(),
        ));
        if success {
            let result = result.unwrap();
            assert_eq!(result.data, json!([]));
            assert!(result.complete);
            assert!(!result.truncated);
        } else {
            assert_eq!(result.unwrap_err(), ReadError::BudgetExceeded);
        }
    }
}

#[test]
fn record_prefix_is_partial_with_or_without_a_native_token_and_never_fetches_page_two() {
    for has_token in [false, true] {
        let mut page = json!({"value":[item("one"),item("two"),item("three")]});
        if has_token {
            page["continuationToken"] = json!("opaque-private-next");
        }
        let backend = Arc::new(Backend::new([
            response(200, page),
            response(500, json!({"message":"page two must not be fetched"})),
        ]));
        let credential = Arc::new(Credential::default());
        let client = client(credential.clone(), backend.clone());
        let result = run(read_workspace(
            &client,
            &request(Kind::Items, 2, 4096),
            pending(),
        ))
        .unwrap();
        assert_eq!(result.data, json!([item("one"), item("two")]));
        assert!(!result.complete);
        assert!(result.truncated);
        assert!(
            !serde_json::to_string(&result.data)
                .unwrap()
                .contains("opaque-private-next")
        );
        assert!(!format!("{result:?}").contains("opaque-private-next"));
        assert_eq!(backend.requests.lock().unwrap().len(), 1);
        assert_eq!(credential.scopes.lock().unwrap().len(), 1);
        assert_eq!(backend.replies.lock().unwrap().len(), 1);
    }
}

#[test]
fn validated_native_page_boundary_is_partial_even_when_every_row_fits() {
    for rows in [vec![], vec![item("one"), item("two")]] {
        for header in [false, true] {
            let mut page = response(
                200,
                json!({"value":rows,
                "continuationToken":if header {Value::Null} else {json!("opaque-private-next")},
                "continuationUri":format!(
                    "https://api.fabric.microsoft.com/v1/workspaces/{WORKSPACE}/items?recursive=false&continuationToken=opaque-private-next"
                )}),
            );
            if header {
                page.headers
                    .insert("X-Ms-Continuation".into(), "opaque-private-next".into());
            }
            let backend = Arc::new(Backend::new([page]));
            let client = client(Arc::new(Credential::default()), backend.clone());
            let result = run(read_workspace(
                &client,
                &request(Kind::Items, 10, 4096),
                pending(),
            ))
            .unwrap();
            assert_eq!(result.data, Value::Array(rows.clone()));
            assert!(!result.complete);
            assert!(result.truncated);
            assert_eq!(backend.requests.lock().unwrap().len(), 1);
            assert!(!format!("{result:?}").contains("opaque-private-next"));
        }
    }
}

#[test]
fn items_reject_conflicting_header_and_body_tokens_without_extra_requests() {
    for (header, body) in [
        ("opaque-private-header", "opaque-private-body"),
        ("", "opaque-private-body"),
        ("opaque-private-header", ""),
    ] {
        let mut reply = response(
            200,
            json!({"value":[item("one"),item("two")],"continuationToken":body}),
        );
        reply
            .headers
            .insert("X-Ms-Continuation".into(), header.into());
        assert_invalid_continuation(Kind::Items, reply);
    }
    let mut reply = response(
        200,
        json!({"value":[item("one")],"continuationToken":"opaque-private-next"}),
    );
    reply
        .headers
        .insert("x-ms-continuation".into(), "opaque-private-next".into());
    reply
        .headers
        .insert("X-Ms-Continuation".into(), "opaque-private-other".into());
    assert_invalid_continuation(Kind::Items, reply);
}

#[test]
fn items_reject_all_non_string_non_null_token_shapes_before_header_precedence() {
    for token in [
        json!(false),
        json!(true),
        json!(0),
        json!(1),
        json!([]),
        json!({}),
        json!(["opaque-private-next"]),
        json!({"token":"opaque-private-next"}),
    ] {
        for header in [None, Some(""), Some("opaque-private-next")] {
            let mut reply = response(
                200,
                json!({"value":[item("one"),item("two")],"continuationToken":token}),
            );
            if let Some(header) = header {
                reply
                    .headers
                    .insert("X-Ms-Continuation".into(), header.into());
            }
            assert_invalid_continuation(Kind::Items, reply);
        }
    }
}

#[test]
fn items_accept_matching_tokens_single_sources_and_inert_values_without_another_page() {
    for (body, header, complete) in [
        (None, None, true),
        (Some(Value::Null), None, true),
        (Some(json!("")), None, true),
        (None, Some(""), true),
        (Some(Value::Null), Some(""), true),
        (Some(json!("")), Some(""), true),
        (Some(json!("opaque-private-next")), None, false),
        (None, Some("opaque-private-next"), false),
        (Some(Value::Null), Some("opaque-private-next"), false),
        (
            Some(json!("opaque-private-next")),
            Some("opaque-private-next"),
            false,
        ),
    ] {
        let mut data = json!({"value":[item("one")],"continuationUri":null});
        if let Some(body) = body {
            data["continuationToken"] = body;
        }
        let mut reply = response(200, data);
        if let Some(header) = header {
            reply
                .headers
                .insert("X-Ms-Continuation".into(), header.into());
            reply
                .headers
                .insert("x-ms-continuation".into(), header.into());
        }
        let credential = Arc::new(Credential::default());
        let backend = Arc::new(Backend::new([
            reply,
            response(500, json!({"message":"a second page must not be fetched"})),
        ]));
        let client = client(credential.clone(), backend.clone());
        let result = run(read_workspace(
            &client,
            &request(Kind::Items, 1, 4096),
            pending(),
        ))
        .unwrap();
        assert_eq!(result.data, json!([item("one")]));
        assert_eq!(result.complete, complete);
        assert_eq!(result.truncated, !complete);
        assert!(!format!("{result:?}").contains("opaque-private"));
        assert_eq!(backend.requests.lock().unwrap().len(), 1);
        assert_eq!(credential.scopes.lock().unwrap().len(), 1);
        assert_eq!(backend.replies.lock().unwrap().len(), 1);
    }
}

#[test]
fn unsafe_or_malformed_active_continuations_are_failures_not_successful_prefixes() {
    for continuation in [
        json!({"continuationToken":true}),
        json!({"continuationToken":1}),
        json!({"continuationToken":["opaque-private-next"]}),
        json!({"continuationToken":{"token":"opaque-private-next"}}),
        json!({"continuationUri":false}),
        json!({"continuationUri":0}),
        json!({"continuationUri":[]}),
        json!({"continuationUri":{}}),
        json!({"continuationUri":format!(
            "https://api.fabric.microsoft.com/v1/workspaces/{WORKSPACE}/items?continuationToken=next")}),
        json!({"continuationToken":"next","continuationUri":7}),
        json!({"continuationToken":"next","continuationUri":"not-a-url"}),
        json!({"continuationToken":"next","continuationUri":format!(
            "https://foreign.invalid/v1/workspaces/{WORKSPACE}/items")}),
        json!({"continuationToken":"next","continuationUri":format!(
            "https://api.fabric.microsoft.com/v1/workspaces/{OTHER_WORKSPACE}/items")}),
        json!({"continuationToken":"next","continuationUri":format!(
            "https://api.fabric.microsoft.com/v1/workspaces/{WORKSPACE}/folders")}),
        json!({"continuationToken":"next","continuationUri":format!(
            "https://private@api.fabric.microsoft.com/v1/workspaces/{WORKSPACE}/items")}),
        json!({"continuationToken":"next","continuationUri":format!(
            "https://api.fabric.microsoft.com/v1/workspaces/{WORKSPACE}/items#private")}),
        json!({"continuationToken":"next","continuationUri":format!(
            "https://api.fabric.microsoft.com/v1/workspaces/{WORKSPACE}/other/../items")}),
    ] {
        let mut page = continuation;
        page["value"] = json!([item("one"), item("two")]);
        let backend = Arc::new(Backend::new([response(200, page)]));
        let client = client(Arc::new(Credential::default()), backend.clone());
        assert_eq!(
            run(read_workspace(
                &client,
                &request(Kind::Items, 1, 4096),
                pending()
            ))
            .unwrap_err(),
            ReadError::InvalidResponse
        );
        assert_eq!(backend.requests.lock().unwrap().len(), 1);
    }
}

#[test]
fn shared_collector_rejects_repeated_tokens_without_replacing_that_guard_in_the_slice() {
    // A cycle requires two pages: the one-page API cannot reach it. Exercise
    // the actual shared collector that owns this validation independently.
    let backend = Arc::new(Backend::new([
        response(
            200,
            json!({"value":[item("one")],"continuationToken":"repeated-private"}),
        ),
        response(
            200,
            json!({"value":[item("two")],"continuationToken":"repeated-private"}),
        ),
    ]));
    let client = client(Arc::new(Credential::default()), backend.clone());
    let route = Request::fabric(Method::Get, &["workspaces", WORKSPACE, "items"])
        .unwrap()
        .with_query(&[("recursive".into(), "false".into())])
        .unwrap();
    let error = run(client.collect(
        route,
        "value",
        Paging::Query,
        Limits {
            max_pages: nonzero(3),
            ..Limits::default()
        },
        pending(),
    ))
    .unwrap_err();
    assert_eq!(error.error, ApiError::Continuation);
    assert_eq!(error.partial.pages, 2);
    let requests = backend.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(
        requests[1]
            .url
            .ends_with("?recursive=false&continuationToken=repeated-private")
    );
}

#[test]
fn metadata_requires_a_matching_workspace_identity_and_string_name() {
    for data in [
        Value::Null,
        json!([]),
        json!({}),
        json!({"id":WORKSPACE}),
        json!({"displayName":"Missing id"}),
        json!({"id":OTHER_WORKSPACE,"displayName":"Wrong workspace"}),
        json!({"id":WORKSPACE.replace('-', ""),"displayName":"Wrong layout"}),
        json!({"id":7,"displayName":"Wrong id type"}),
        json!({"id":WORKSPACE,"displayName":null}),
        json!({"id":WORKSPACE,"displayName":17}),
        json!({"errorCode":"Denied","message":"private-service-body"}),
    ] {
        let backend = Arc::new(Backend::new([response(200, data)]));
        let client = client(Arc::new(Credential::default()), backend.clone());
        assert_eq!(
            run(read_workspace(
                &client,
                &request(Kind::Metadata, 1, 4096),
                pending()
            ))
            .unwrap_err(),
            ReadError::InvalidResponse
        );
        assert_eq!(backend.requests.lock().unwrap().len(), 1);
    }
}

#[test]
fn every_native_item_is_validated_including_rows_outside_the_selected_prefix() {
    for invalid in [
        Value::Null,
        json!([]),
        json!({}),
        json!({"id":"bad","displayName":"Missing type"}),
        json!({"id":null,"displayName":"Wrong id","type":"Notebook"}),
        json!({"id":"bad","displayName":false,"type":"Notebook"}),
        json!({"id":"bad","displayName":"Wrong type","type":3}),
        json!({"id":"bad","displayName":"Foreign","type":"Notebook","workspaceId":OTHER_WORKSPACE}),
        json!({"id":"bad","displayName":"Null workspace","type":"Notebook","workspaceId":null}),
    ] {
        let backend = Arc::new(Backend::new([response(
            200,
            json!({"value":[item("one"),invalid]}),
        )]));
        let client = client(Arc::new(Credential::default()), backend);
        assert_eq!(
            run(read_workspace(
                &client,
                &request(Kind::Items, 1, 4096),
                pending()
            ))
            .unwrap_err(),
            ReadError::InvalidResponse
        );
    }
    for invalid in [Value::Null, json!([]), json!({}), json!({"value":{}})] {
        let backend = Arc::new(Backend::new([response(200, invalid)]));
        let client = client(Arc::new(Credential::default()), backend);
        assert_eq!(
            run(read_workspace(
                &client,
                &request(Kind::Items, 1, 4096),
                pending()
            ))
            .unwrap_err(),
            ReadError::InvalidResponse
        );
    }
}

#[test]
fn statuses_service_failures_and_malformed_json_never_become_empty_success() {
    for kind in [Kind::Metadata, Kind::Items, Kind::Workspaces] {
        for (status, expected) in [
            (201, ReadError::InvalidResponse),
            (202, ReadError::InvalidResponse),
            (204, ReadError::InvalidResponse),
            (206, ReadError::InvalidResponse),
            (301, ReadError::OperationFailed),
            (401, ReadError::Unauthenticated),
            (403, ReadError::AccessDenied),
            (404, ReadError::NotFound),
            (429, ReadError::OperationFailed),
            (500, ReadError::OperationFailed),
        ] {
            let mut reply = response(
                status,
                json!({"errorCode":"private-code","message":"private-service-body"}),
            );
            reply
                .headers
                .insert("x-ms-public-api-error-code".into(), "private-header".into());
            let backend = Arc::new(Backend::new([reply]));
            let client = client(Arc::new(Credential::default()), backend.clone());
            let error =
                run(read_workspace(&client, &request(kind, 10, 4096), pending())).unwrap_err();
            assert_eq!(error, expected);
            assert!(!format!("{error} {error:?}").contains("private"));
            assert_eq!(backend.requests.lock().unwrap().len(), 1);
        }
        for body in [vec![], b"not-json".to_vec(), b"{".to_vec(), vec![0xff]] {
            let backend = Arc::new(Backend::new([raw_response(200, body)]));
            let client = client(Arc::new(Credential::default()), backend);
            assert_eq!(
                run(read_workspace(&client, &request(kind, 10, 4096), pending())).unwrap_err(),
                ReadError::InvalidResponse
            );
        }
    }
}

#[test]
fn shared_response_safety_limit_is_finite_separate_and_exact() {
    for kind in [Kind::Metadata, Kind::Items, Kind::Workspaces] {
        for extra in [0, 1] {
            let mut body = serde_json::to_vec(&payload(kind)).unwrap();
            body.resize(RESPONSE_BYTES + extra, b' ');
            let backend = Arc::new(Backend::new([raw_response(200, body)]));
            let client = client(Arc::new(Credential::default()), backend.clone());
            let result = run(read_workspace(&client, &request(kind, 10, 4096), pending()));
            if extra == 0 {
                assert!(result.unwrap().complete);
            } else {
                assert_eq!(result.unwrap_err(), ReadError::BudgetExceeded);
            }
            assert_eq!(backend.requests.lock().unwrap()[0].limit, RESPONSE_BYTES);
        }
    }
}

#[test]
fn shared_execution_failures_map_to_sanitized_typed_errors() {
    for kind in [Kind::Metadata, Kind::Items, Kind::Workspaces] {
        for (execution, expected) in [
            (ExecutionError::Failed, ReadError::OperationFailed),
            (ExecutionError::RetriesExhausted, ReadError::OperationFailed),
            (ExecutionError::TimedOut, ReadError::Timeout),
            (ExecutionError::Cancelled, ReadError::Cancelled),
            (ExecutionError::ResponseLimit, ReadError::BudgetExceeded),
        ] {
            let backend = Arc::new(Backend {
                replies: Mutex::new([Err(execution)].into()),
                ..Backend::default()
            });
            let client = client(Arc::new(Credential::default()), backend.clone());
            assert_eq!(
                run(read_workspace(&client, &request(kind, 10, 4096), pending())).unwrap_err(),
                expected
            );
            assert_eq!(backend.requests.lock().unwrap().len(), 1);
        }
    }
}

#[test]
fn unsupported_audience_executor_and_failed_credentials_do_not_dispatch() {
    for kind in [Kind::Metadata, Kind::Items, Kind::Workspaces] {
        for audience in [Audience::OneLake, Audience::Arm] {
            let credential = Arc::new(Credential::default());
            let backend = Arc::new(Backend::default());
            let client = client_with_audience(credential.clone(), backend.clone(), audience);
            assert_eq!(
                run(read_workspace(&client, &request(kind, 10, 4096), pending())).unwrap_err(),
                ReadError::UnsupportedCapability
            );
            assert!(credential.scopes.lock().unwrap().is_empty());
            assert!(backend.requests.lock().unwrap().is_empty());
        }
        let credential = Arc::new(Credential::default());
        let backend = Arc::new(Backend {
            supported: false,
            ..Backend::default()
        });
        let unsupported = client(credential.clone(), backend.clone());
        assert_eq!(
            run(read_workspace(
                &unsupported,
                &request(kind, 10, 4096),
                pending()
            ))
            .unwrap_err(),
            ReadError::UnsupportedCapability
        );
        assert!(credential.scopes.lock().unwrap().is_empty());
        assert!(backend.requests.lock().unwrap().is_empty());
        let credential = Arc::new(Credential {
            fail: true,
            ..Credential::default()
        });
        let backend = Arc::new(Backend::default());
        let unauthenticated = client(credential.clone(), backend.clone());
        assert_eq!(
            run(read_workspace(
                &unauthenticated,
                &request(kind, 10, 4096),
                pending()
            ))
            .unwrap_err(),
            ReadError::Unauthenticated
        );
        assert_eq!(credential.scopes.lock().unwrap().len(), 1);
        assert!(backend.requests.lock().unwrap().is_empty());
    }
}

#[test]
fn already_cancelled_reads_do_not_acquire_credentials() {
    for kind in [Kind::Metadata, Kind::Items, Kind::Workspaces] {
        let credential = Arc::new(Credential::default());
        let backend = Arc::new(Backend::default());
        let client = client(credential.clone(), backend.clone());
        assert_eq!(
            run(read_workspace(&client, &request(kind, 10, 4096), ready(()))).unwrap_err(),
            ReadError::Cancelled
        );
        assert!(credential.scopes.lock().unwrap().is_empty());
        assert!(backend.requests.lock().unwrap().is_empty());
    }
}

#[test]
fn cancellation_drops_pending_credentials_before_any_request() {
    for kind in [Kind::Metadata, Kind::Items, Kind::Workspaces] {
        let credential = Arc::new(Credential {
            hold: true,
            ..Credential::default()
        });
        let backend = Arc::new(Backend::default());
        let client = client(credential.clone(), backend.clone());
        let flag = Arc::new(AtomicBool::new(false));
        let request = request(kind, 10, 4096);
        let mut future = pin!(read_workspace(&client, &request, signal(flag.clone())));
        let mut context = Context::from_waker(Waker::noop());
        assert!(future.as_mut().poll(&mut context).is_pending());
        assert_eq!(credential.scopes.lock().unwrap().len(), 1);
        assert!(backend.requests.lock().unwrap().is_empty());
        flag.store(true, Ordering::SeqCst);
        assert_eq!(
            future.as_mut().poll(&mut context),
            Poll::Ready(Err(ReadError::Cancelled))
        );
        assert_eq!(credential.dropped.load(Ordering::SeqCst), 1);
        assert!(backend.requests.lock().unwrap().is_empty());
    }
}

#[test]
fn in_flight_cancellation_drops_the_executor_without_a_second_request() {
    for kind in [Kind::Metadata, Kind::Items, Kind::Workspaces] {
        let credential = Arc::new(Credential::default());
        let backend = Arc::new(Backend {
            hold: true,
            ..Backend::default()
        });
        let client = client(credential.clone(), backend.clone());
        let flag = Arc::new(AtomicBool::new(false));
        let request = request(kind, 10, 4096);
        let mut future = pin!(read_workspace(&client, &request, signal(flag.clone())));
        let mut context = Context::from_waker(Waker::noop());
        assert!(future.as_mut().poll(&mut context).is_pending());
        assert_eq!(backend.requests.lock().unwrap().len(), 1);
        flag.store(true, Ordering::SeqCst);
        assert_eq!(
            future.as_mut().poll(&mut context),
            Poll::Ready(Err(ReadError::Cancelled))
        );
        assert_eq!(backend.dropped.load(Ordering::SeqCst), 1);
        assert_eq!(credential.scopes.lock().unwrap().len(), 1);
        assert_eq!(backend.requests.lock().unwrap().len(), 1);
    }
}

#[test]
fn cancellation_is_rechecked_after_decode_and_budget_before_committing_data() {
    for kind in [Kind::Metadata, Kind::Items, Kind::Workspaces] {
        for bytes in [1, 4096] {
            let flag = Arc::new(AtomicBool::new(false));
            let backend = Arc::new(Backend {
                cancel_after_response: Some(flag.clone()),
                ..Backend::new([response(200, payload(kind))])
            });
            let client = client(Arc::new(Credential::default()), backend.clone());
            assert_eq!(
                run(read_workspace(
                    &client,
                    &request(kind, 10, bytes),
                    signal(flag)
                ))
                .unwrap_err(),
                ReadError::Cancelled
            );
            assert_eq!(backend.requests.lock().unwrap().len(), 1);
        }
    }
}

#[test]
fn error_codes_and_displays_are_static_and_have_no_provider_payload() {
    for (error, code, message) in [
        (
            ReadError::InvalidInput,
            "InvalidInput",
            "Invalid workspace read input.",
        ),
        (
            ReadError::NotConfigured,
            "NotConfigured",
            "The workspace read host is not configured.",
        ),
        (
            ReadError::UnsupportedCapability,
            "UnsupportedCapability",
            "The workspace read capability is unavailable.",
        ),
        (
            ReadError::OutputExists,
            "OutputExists",
            "The workspace read output destination already exists.",
        ),
        (
            ReadError::OutputFailed,
            "OutputFailed",
            "The workspace read output could not be written.",
        ),
        (
            ReadError::InvalidResponse,
            "InvalidResponse",
            "The workspace read response is invalid.",
        ),
        (
            ReadError::Unauthenticated,
            "Unauthenticated",
            "Authentication is required for the workspace read.",
        ),
        (
            ReadError::AccessDenied,
            "AccessDenied",
            "Access to the workspace read was denied.",
        ),
        (
            ReadError::NotFound,
            "NotFound",
            "The requested workspace resource was not found.",
        ),
        (
            ReadError::BudgetExceeded,
            "BudgetExceeded",
            "The workspace read exceeded its finite budget.",
        ),
        (
            ReadError::Cancelled,
            "Cancelled",
            "The workspace read was cancelled.",
        ),
        (
            ReadError::Timeout,
            "Timeout",
            "The workspace read timed out.",
        ),
        (
            ReadError::OperationFailed,
            "OperationFailed",
            "The workspace read failed.",
        ),
    ] {
        assert_eq!(error.code(), code);
        assert_eq!(format!("{error:?}"), code);
        assert_eq!(error.to_string(), message);
        let standard_error: &dyn std::error::Error = &error;
        assert!(standard_error.source().is_none());
    }
}
