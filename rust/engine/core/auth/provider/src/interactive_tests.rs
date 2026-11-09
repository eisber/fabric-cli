// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
#![deny(unnameable_test_items)]

use super::*;
use azure_core::http::{AsyncRawResponse, StatusCode, headers::Headers};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

const TENANT: &str = "11111111-2222-3333-4444-555555555555";
const CLIENT: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
const OBJECT: &str = "99999999-8888-7777-6666-555555555555";
const ACCOUNT: &str = "user@example.test";

fn options() -> InteractiveOptions {
    InteractiveOptions::new(TENANT, CLIENT, Some(ACCOUNT.into())).unwrap()
}

fn identity() -> SessionIdentity {
    SessionIdentity {
        tenant: TENANT.into(),
        object_id: OBJECT.into(),
        subject: "pairwise-subject".into(),
        account: ACCOUNT.into(),
    }
}

fn session() -> RefreshSession {
    RefreshSession {
        session_id: 1,
        client: CLIENT.into(),
        identity: identity(),
        refresh_token: Secret::new("refresh-canary-0"),
    }
}

fn id_token(tenant: &str, object: &str) -> String {
    let now = OffsetDateTime::now_utc().unix_timestamp();
    let payload = json!({
        "aud": CLIENT, "tid": tenant, "oid": object, "sub": "pairwise-subject",
        "iss": format!("https://login.microsoftonline.com/{tenant}/v2.0"),
        "preferred_username": ACCOUNT, "exp": now + 3600, "nbf": now - 60,
    });
    format!(
        "header.{}.signature",
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&payload).unwrap())
    )
}

fn token(refresh: Option<&str>) -> Value {
    let mut value = json!({
        "token_type": "Bearer", "expires_in": 3600,
        "access_token": "opaque-access-canary-not-a-jwt",
        "id_token": id_token(TENANT, OBJECT),
    });
    if let Some(refresh) = refresh {
        value["refresh_token"] = refresh.into();
    }
    value
}

fn device(expires: u64) -> Value {
    json!({
        "device_code": "device-canary-secret", "user_code": "ABCD-EFGH",
        "verification_uri": "https://microsoft.com/devicelogin",
        "expires_in": expires, "interval": 1,
        "message": "provider-canary-must-never-be-displayed",
    })
}

struct FakeStore {
    state: Mutex<SessionSnapshot>,
    fail: AtomicBool,
}

impl FakeStore {
    fn empty() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(SessionSnapshot {
                revision: 0,
                session: None,
            }),
            fail: AtomicBool::new(false),
        })
    }
    fn selected() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(SessionSnapshot {
                revision: 1,
                session: Some(session()),
            }),
            fail: AtomicBool::new(false),
        })
    }
}

#[async_trait]
impl SessionStore for FakeStore {
    async fn load(&self) -> Result<SessionSnapshot, InteractiveError> {
        Ok(self.state.lock().unwrap().clone())
    }
    async fn compare_exchange(
        &self,
        revision: u64,
        session: Option<RefreshSession>,
    ) -> Result<u64, InteractiveError> {
        if self.fail.load(Ordering::SeqCst) {
            return Err(InteractiveError::Store);
        }
        let mut state = self.state.lock().unwrap();
        if state.revision != revision {
            return Err(InteractiveError::SessionChanged);
        }
        state.revision += 1;
        state.session = session;
        Ok(state.revision)
    }
}

struct Call {
    url: String,
    form: BTreeMap<String, String>,
    when: Instant,
}
struct FakeHttp {
    responses: Mutex<VecDeque<(StatusCode, Vec<u8>)>>,
    calls: Mutex<Vec<Call>>,
    invalidate: Option<Arc<FakeStore>>,
}

impl FakeHttp {
    fn new(responses: Vec<(StatusCode, Value)>) -> Arc<Self> {
        Arc::new(Self {
            responses: Mutex::new(
                responses
                    .into_iter()
                    .map(|(status, value)| (status, serde_json::to_vec(&value).unwrap()))
                    .collect(),
            ),
            calls: Mutex::new(Vec::new()),
            invalidate: None,
        })
    }
}

impl fmt::Debug for FakeHttp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("FakeHttp")
    }
}

#[async_trait]
impl HttpClient for FakeHttp {
    async fn execute_request(&self, request: &Request) -> azure_core::Result<AsyncRawResponse> {
        assert_eq!(request.method(), Method::Post);
        assert_eq!(
            request.headers().get_str(&"content-type".into()).unwrap(),
            "application/x-www-form-urlencoded"
        );
        let bytes = azure_core::Bytes::from(request.body());
        let form = std::str::from_utf8(&bytes).unwrap();
        let parsed = Url::parse(&format!("https://form.invalid/?{form}")).unwrap();
        self.calls.lock().unwrap().push(Call {
            url: request.url().to_string(),
            form: parsed
                .query_pairs()
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect(),
            when: Instant::now(),
        });
        if let Some(store) = &self.invalidate {
            store.compare_exchange(1, None).await.unwrap();
        }
        let (status, bytes) = self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected HTTP call");
        let mut headers = Headers::new();
        headers.insert("content-type", "application/json");
        Ok(AsyncRawResponse::from_bytes(status, headers, bytes))
    }
}

#[derive(Default)]
struct Ui(Mutex<Vec<DeviceChallenge>>);
impl DeviceCodeUi for Ui {
    fn present(&self, challenge: &DeviceChallenge) -> Result<(), InteractiveError> {
        self.0.lock().unwrap().push(challenge.clone());
        Ok(())
    }
}

#[tokio::test(start_paused = true)]
async fn login_uses_exact_sequential_resource_grants_with_pending_and_slow_down() {
    let http = FakeHttp::new(vec![
        (StatusCode::Ok, device(600)),
        (
            StatusCode::BadRequest,
            json!({"error":"authorization_pending"}),
        ),
        (StatusCode::BadRequest, json!({"error":"slow_down"})),
        (StatusCode::Ok, token(Some("refresh-canary-1"))),
        (StatusCode::Ok, token(Some("refresh-canary-2"))),
        (StatusCode::Ok, token(Some("refresh-canary-3"))),
    ]);
    let store = FakeStore::empty();
    let ui = Ui::default();
    let started = Instant::now();
    let credential = InteractiveCredential::login(options(), http.clone(), store.clone(), &ui)
        .await
        .unwrap();
    assert_eq!(credential.status().await.unwrap().len(), 3);
    let calls = http.calls.lock().unwrap();
    assert_eq!(calls.len(), 6);
    assert_eq!(
        calls[0].url,
        format!("https://login.microsoftonline.com/{TENANT}/oauth2/v2.0/devicecode")
    );
    assert_eq!(
        calls[0].form["scope"],
        format!("{} openid profile offline_access", SCOPES[0])
    );
    assert_eq!(
        calls[1].form["grant_type"],
        "urn:ietf:params:oauth:grant-type:device_code"
    );
    assert_eq!(calls[1].form["device_code"], "device-canary-secret");
    assert_eq!(
        calls[1].when.duration_since(started),
        Duration::from_secs(5)
    );
    assert_eq!(
        calls[2].when.duration_since(calls[1].when),
        Duration::from_secs(5)
    );
    assert_eq!(
        calls[3].when.duration_since(calls[2].when),
        Duration::from_secs(10)
    );
    for (call, scope, refresh) in [
        (&calls[4], SCOPES[1], "refresh-canary-1"),
        (&calls[5], SCOPES[2], "refresh-canary-2"),
    ] {
        assert_eq!(call.form["grant_type"], "refresh_token");
        assert_eq!(
            call.form["scope"],
            format!("{scope} openid profile offline_access")
        );
        assert_eq!(call.form["refresh_token"], refresh);
        assert_eq!(call.form["client_id"], CLIENT);
    }
    assert_eq!(
        store
            .state
            .lock()
            .unwrap()
            .session
            .as_ref()
            .unwrap()
            .refresh_token
            .secret(),
        "refresh-canary-3"
    );
    let shown = format!("{:?}", ui.0.lock().unwrap());
    assert!(shown.contains("ABCD-EFGH"));
    assert!(!shown.contains("device-canary") && !shown.contains("provider-canary"));
}

#[tokio::test(start_paused = true)]
async fn silent_refresh_rotates_before_publication_and_reuses_memory_without_prompt() {
    let http = FakeHttp::new(vec![(StatusCode::Ok, token(Some("rotated-canary")))]);
    let store = FakeStore::selected();
    let credential = InteractiveCredential::connect(options(), http.clone(), store.clone())
        .await
        .unwrap();
    let access = credential.get_token(&[SCOPES[0]], None).await.unwrap();
    assert_eq!(access.token.secret(), "opaque-access-canary-not-a-jwt");
    assert_eq!(
        store
            .state
            .lock()
            .unwrap()
            .session
            .as_ref()
            .unwrap()
            .refresh_token
            .secret(),
        "rotated-canary"
    );
    credential.get_token(&[SCOPES[0]], None).await.unwrap();
    assert_eq!(http.calls.lock().unwrap().len(), 1);
    assert!(!http.calls.lock().unwrap()[0].url.contains("devicecode"));
    assert!(
        credential
            .get_token(&[SCOPES[0], SCOPES[1]], None)
            .await
            .is_err()
    );
}

#[tokio::test(start_paused = true)]
async fn failed_rotation_does_not_publish_access_token() {
    let http = FakeHttp::new(vec![(StatusCode::Ok, token(Some("rotated-canary")))]);
    let store = FakeStore::selected();
    store.fail.store(true, Ordering::SeqCst);
    let credential = InteractiveCredential::connect(options(), http.clone(), store.clone())
        .await
        .unwrap();
    let error = credential.get_token(&[SCOPES[0]], None).await.unwrap_err();
    assert!(error.to_string().contains("OS-protected"));
    assert!(!error.to_string().contains("canary"));
    assert!(credential.state.lock().await.tokens.is_empty());
    assert_eq!(store.state.lock().unwrap().revision, 1);
}

#[tokio::test(start_paused = true)]
async fn logout_invalidates_cached_and_inflight_credentials_and_cannot_clear_new_login() {
    let store = FakeStore::selected();
    let http = FakeHttp::new(vec![(StatusCode::Ok, token(None))]);
    let credential = InteractiveCredential::connect(options(), http.clone(), store.clone())
        .await
        .unwrap();
    credential.get_token(&[SCOPES[0]], None).await.unwrap();
    credential.logout().await.unwrap();
    assert!(credential.get_token(&[SCOPES[0]], None).await.is_err());
    assert_eq!(http.calls.lock().unwrap().len(), 1);
    let mut newer = session();
    newer.session_id = 4;
    store.compare_exchange(3, Some(newer)).await.unwrap();
    assert_eq!(
        credential.logout().await.unwrap_err(),
        InteractiveError::SessionChanged
    );
    assert!(store.state.lock().unwrap().session.is_some());

    let store = FakeStore::selected();
    let http = Arc::new(FakeHttp {
        responses: Mutex::new(VecDeque::from([(
            StatusCode::Ok,
            serde_json::to_vec(&token(Some("rotated"))).unwrap(),
        )])),
        calls: Mutex::new(Vec::new()),
        invalidate: Some(store.clone()),
    });
    let credential = InteractiveCredential::connect(options(), http, store.clone())
        .await
        .unwrap();
    assert!(credential.get_token(&[SCOPES[0]], None).await.is_err());
    assert!(store.state.lock().unwrap().session.is_none());
}

#[tokio::test(start_paused = true)]
async fn expiry_cancellation_decline_and_partial_resource_failure_never_publish() {
    for responses in [
        vec![
            (StatusCode::Ok, device(6)),
            (
                StatusCode::BadRequest,
                json!({"error":"authorization_pending"}),
            ),
        ],
        vec![
            (StatusCode::Ok, device(60)),
            (
                StatusCode::BadRequest,
                json!({"error":"authorization_declined","error_description":"provider-canary"}),
            ),
        ],
        vec![
            (StatusCode::Ok, device(60)),
            (StatusCode::Ok, token(Some("refresh"))),
            (StatusCode::BadRequest, json!({"error":"invalid_grant"})),
        ],
    ] {
        let http = FakeHttp::new(responses);
        let store = FakeStore::empty();
        let result =
            InteractiveCredential::login(options(), http, store.clone(), &Ui::default()).await;
        assert!(result.is_err());
        assert!(store.state.lock().unwrap().session.is_none());
    }
    let http = FakeHttp::new(vec![(StatusCode::Ok, device(60))]);
    let store = FakeStore::empty();
    let ui = Ui::default();
    let result = tokio::time::timeout(
        Duration::from_secs(2),
        InteractiveCredential::login(options(), http.clone(), store.clone(), &ui),
    )
    .await;
    assert!(result.is_err());
    assert_eq!(http.calls.lock().unwrap().len(), 1);
    assert!(store.state.lock().unwrap().session.is_none());
}

#[tokio::test(start_paused = true)]
async fn wrong_tenant_account_client_and_refresh_identity_fail_closed() {
    for (claim, value) in [
        ("tid", json!(CLIENT)),
        ("aud", json!(TENANT)),
        ("oid", json!(CLIENT)),
        ("sub", json!("another-user")),
        ("preferred_username", json!("other@example.test")),
        ("exp", json!(0)),
    ] {
        let mut response = token(Some("canary"));
        let id = response["id_token"].as_str().unwrap();
        let parts: Vec<_> = id.split('.').collect();
        let mut claims: Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[1]).unwrap()).unwrap();
        claims[claim] = value;
        response["id_token"] = format!(
            "header.{}.signature",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap())
        )
        .into();
        let http = FakeHttp::new(vec![(StatusCode::Ok, response)]);
        let store = FakeStore::selected();
        let credential = InteractiveCredential::connect(options(), http, store.clone())
            .await
            .unwrap();
        assert!(
            credential.get_token(&[SCOPES[0]], None).await.is_err(),
            "{claim}"
        );
        assert_eq!(store.state.lock().unwrap().revision, 1);
    }
}

#[tokio::test(start_paused = true)]
async fn missing_session_and_interaction_required_never_authorize_devicecode() {
    let empty = FakeStore::empty();
    let http = FakeHttp::new(vec![]);
    let result = InteractiveCredential::connect(options(), http.clone(), empty).await;
    assert!(matches!(result, Err(InteractiveError::NotConfigured)));
    assert!(http.calls.lock().unwrap().is_empty());
    let http = FakeHttp::new(vec![(
        StatusCode::BadRequest,
        json!({"error":"interaction_required","error_description":"secret-canary"}),
    )]);
    let credential = InteractiveCredential::connect(options(), http.clone(), FakeStore::selected())
        .await
        .unwrap();
    let error = credential.status().await.unwrap_err();
    assert_eq!(error, InteractiveError::InteractionRequired);
    assert!(!format!("{error:?} {error}").contains("canary"));
    assert_eq!(http.calls.lock().unwrap().len(), 1);
}

#[test]
fn protocol_metadata_bounds_duplicates_and_opaque_tokens_are_explicit() {
    for bytes in [
        br#"{"expires_in":1,"expires_in":2}"#.as_slice(),
        br#"{"nested":{"key":1,"key":2}}"#,
        br#"{"a":[[[[[[[[[[[[[[[[[[0]]]]]]]]]]]]]]]]]]}"#,
        b"{}{}",
    ] {
        assert!(json::parse(bytes).is_err());
    }
    let mut response = token(None);
    assert!(decode_token(&options(), response.clone(), Some(&identity())).is_ok());
    for key in ["expires_in", "id_token", "access_token", "token_type"] {
        let mut missing = response.clone();
        missing.as_object_mut().unwrap().remove(key);
        assert!(decode_token(&options(), missing, None).is_err());
    }
    response["expires_in"] = json!(0);
    assert!(decode_token(&options(), response, None).is_err());
    assert!(InteractiveOptions::new("organizations", CLIENT, None).is_err());
    assert!(InteractiveOptions::new(TENANT, CLIENT, Some("bad\nhint".into())).is_err());
}

#[tokio::test(start_paused = true)]
async fn redirects_untrusted_challenge_and_server_errors_are_not_followed_or_echoed() {
    for (status, value, expected) in [
        (
            StatusCode::Found,
            json!({"error_description":"canary"}),
            InteractiveError::Rejected,
        ),
        (
            StatusCode::TooManyRequests,
            json!({"error_description":"canary"}),
            InteractiveError::Unavailable,
        ),
        (
            StatusCode::Ok,
            {
                let mut value = device(60);
                value["verification_uri"] = "https://evil.example/".into();
                value
            },
            InteractiveError::Response,
        ),
    ] {
        let http = FakeHttp::new(vec![(status, value)]);
        let result = InteractiveCredential::login(
            options(),
            http.clone(),
            FakeStore::empty(),
            &Ui::default(),
        )
        .await;
        assert!(matches!(result, Err(error) if error == expected));
        assert_eq!(http.calls.lock().unwrap().len(), 1);
    }
}
