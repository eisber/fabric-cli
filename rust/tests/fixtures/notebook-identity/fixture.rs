// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

#![allow(
    dead_code,
    reason = "Shared synthetic fixture supports distinct boundary suites."
)]

use azure_core::time::OffsetDateTime;
use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use ring::{
    rand::SystemRandom,
    signature::{RSA_PKCS1_SHA256, RsaKeyPair},
};
use serde_json::{Value, json};

pub const TENANT: &str = "11111111-1111-1111-1111-111111111111";
pub const CLIENT: &str = "22222222-2222-2222-2222-222222222222";
pub const OBJECT: &str = "33333333-3333-3333-3333-333333333333";
pub const CAPACITY: &str = "44444444-4444-4444-4444-444444444444";
pub const WORKSPACE: &str = "55555555-5555-5555-5555-555555555555";
pub const NOTEBOOK: &str = "66666666-6666-6666-6666-666666666666";
pub const OTHER: &str = "77777777-7777-7777-7777-777777777777";
pub const PBI: &str = "https://analysis.windows.net/powerbi/api";
pub const FABRIC: &str = "https://api.fabric.microsoft.com";
pub const CLUSTER: &str = "https://selected-cluster.analysis.windows.net";
pub const WORKLOAD: &str = "https://selected-notebook.fabric.microsoft.com";
pub const MWC: &str = "synthetic-offline-mwc-grant";

pub fn key() -> Value {
    serde_json::from_str(include_str!("rsa-test-key.json")).unwrap()
}

pub fn claims(audience: &str) -> Value {
    let now = OffsetDateTime::now_utc().unix_timestamp();
    json!({
        "aud": audience, "iss": format!("https://sts.windows.net/{TENANT}/"),
        "tid": TENANT, "oid": OBJECT, "appid": CLIENT, "idtyp": "app", "ver": "1.0",
        "iat": now - 60, "nbf": now - 60, "exp": now + 3500,
    })
}

pub fn header(kid: &str) -> Value {
    json!({"alg": "RS256", "typ": "JWT", "kid": kid})
}

pub fn signed(header: &Value, claims: &Value) -> String {
    let key = key();
    let der = STANDARD.decode(key["pkcs8"].as_str().unwrap()).unwrap();
    let key = RsaKeyPair::from_pkcs8(&der).unwrap();
    let input = format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(header).unwrap()),
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(claims).unwrap())
    );
    let mut signature = vec![0; key.public().modulus_len()];
    key.sign(
        &RSA_PKCS1_SHA256,
        &SystemRandom::new(),
        input.as_bytes(),
        &mut signature,
    )
    .unwrap();
    format!("{input}.{}", URL_SAFE_NO_PAD.encode(signature))
}

pub fn token(audience: &str) -> String {
    signed(&header("offline-key"), &claims(audience))
}

pub fn jwks(kid: &str) -> Value {
    let key = key();
    json!({"keys": [{
        "kid": kid, "kty": "RSA", "n": key["n"], "e": key["e"],
        "issuer": "https://sts.windows.net/{tenantid}/", "alg": "RS256", "use": "sig",
    }]})
}

pub fn grant() -> Value {
    let expires = chrono::Utc::now() + chrono::Duration::seconds(3000);
    json!({
        "Token": MWC, "TargetUriHost": WORKLOAD,
        "CapacityObjectId": CAPACITY, "Expiry": expires.to_rfc3339(),
    })
}
