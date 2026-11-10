// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline public-boundary updater contracts. Every key here is synthetic.

#![forbid(unsafe_code)]
#![deny(unnameable_test_items)]

use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

mod support;

use fabric_cli_update::{
    AutomaticContext, Channel, CheckSchedule, Control, DownloadedUpdate, Error, FileInstall,
    FileInstaller, HttpRequest, HttpResponse, HttpTransport, Identity, InstallContext,
    InstallCustody, Installer, Limits, Manifest, Policy, ReleaseSelector, Repository, ResponseSink,
    Snapshot, StartupDecision, Target, Trust, UpdateEngine, VerificationContext, verify_manifest,
};
use ring::{digest, signature::Ed25519KeyPair, signature::KeyPair};
use serde_json::{Value, json};

const NOW: u64 = 1_800_000_000;
const TAG: &str = "rust-v1.10.0";
const VARIANT: &str = "default-no-features";
const MANIFEST_NAME: &str = "update-x86_64-pc-windows-msvc-default-no-features.json";
const ASSET: &str = "fab-rs-windows-x64.exe";

struct Clock {
    value: Cell<Snapshot>,
}

impl Clock {
    fn new() -> Self {
        Self {
            value: Cell::new(Snapshot {
                unix_seconds: NOW,
                monotonic_ms: 100,
                cancelled: false,
            }),
        }
    }

    fn alter(&self, action: impl FnOnce(&mut Snapshot)) {
        let mut value = self.value.get();
        action(&mut value);
        self.value.set(value);
    }
}

impl Control for Clock {
    fn snapshot(&self) -> Snapshot {
        self.value.get()
    }
}

struct HookClock<F: FnMut()> {
    clock: Clock,
    hook: RefCell<F>,
}

impl<F: FnMut()> Control for HookClock<F> {
    fn snapshot(&self) -> Snapshot {
        (self.hook.borrow_mut())();
        self.clock.snapshot()
    }
}

struct Reply {
    expected: String,
    body: Vec<u8>,
    status: u16,
    location: Option<String>,
}

struct FakeTransport {
    replies: VecDeque<Reply>,
    requests: Vec<String>,
}

impl HttpTransport for FakeTransport {
    fn get(
        &mut self,
        request: &HttpRequest,
        sink: &mut dyn ResponseSink,
        _control: &dyn Control,
    ) -> Result<HttpResponse, Error> {
        self.requests.push(request.url.clone());
        let reply = self.replies.pop_front().ok_or(Error::Transport)?;
        assert_eq!(request.url, reply.expected);
        assert_eq!(request.api_version, "2022-11-28");
        assert!(request.deadline_ms > 100);
        for chunk in reply.body.chunks(7) {
            sink.write(chunk)?;
        }
        Ok(HttpResponse {
            status: reply.status,
            location: reply.location,
        })
    }
}

struct Case {
    key: Ed25519KeyPair,
    trust: Trust,
    identity: Identity,
    manifest: Manifest,
    binary: Vec<u8>,
}

impl Case {
    fn new() -> Self {
        // Local test-only seed; this is never a publisher or production key.
        let key = Ed25519KeyPair::from_seed_unchecked(&[7; 32]).unwrap();
        let mut public = [0; 32];
        public.copy_from_slice(key.public_key().as_ref());
        let binary = binary();
        let trust = Trust::new(repository(), Some(public)).unwrap();
        Self {
            key,
            trust,
            identity: identity("1.9.0"),
            manifest: Manifest {
                protocol: 1,
                product: "fab-rs".to_owned(),
                repository: "fixture-publisher/fixture-product".to_owned(),
                tag: TAG.to_owned(),
                version: "1.10.0".to_owned(),
                channel: Channel::Stable,
                target: Target::WindowsX64.as_str().to_owned(),
                variant: VARIANT.to_owned(),
                asset: ASSET.to_owned(),
                bytes: binary.len() as u64,
                sha256: hex(&hash(&binary)),
                expires_at: NOW + 3600,
            },
            binary,
        }
    }

    fn engine(&self) -> UpdateEngine {
        UpdateEngine::new(
            Some(self.trust.clone()),
            self.identity.clone(),
            Limits::default(),
        )
        .unwrap()
    }

    fn signed(&self) -> (Vec<u8>, Vec<u8>) {
        let bytes = serde_json::to_vec(&self.manifest).unwrap();
        let signature = self.key.sign(&bytes).as_ref().to_vec();
        (bytes, signature)
    }

    fn verify(&self, bytes: &[u8], signature: &[u8]) -> Result<(), Error> {
        verify_manifest(
            bytes,
            signature,
            VerificationContext {
                trust: &self.trust,
                identity: &self.identity,
                tag: TAG,
                prerelease: false,
                now: NOW,
                limits: Limits::default(),
            },
        )
        .map(|_| ())
    }

    fn json(&self) -> Value {
        let (manifest, _) = self.signed();
        json!({
            "id": 123,
            "url": "https://api.github.com/repos/fixture-publisher/fixture-product/releases/123",
            "html_url": format!("https://github.com/fixture-publisher/fixture-product/releases/tag/{}", self.manifest.tag),
            "tag_name": self.manifest.tag,
            "draft": false,
            "prerelease": false,
            "assets": [
                {"name": MANIFEST_NAME, "size": manifest.len(), "browser_download_url": "https://evil.invalid/ignored"},
                {"name": format!("{MANIFEST_NAME}.sig"), "size": 64},
                {"name": ASSET, "size": self.manifest.bytes}
            ]
        })
    }

    fn transport(&self, discovery: bool) -> FakeTransport {
        let (manifest, signature) = self.signed();
        let mut replies = VecDeque::new();
        if discovery {
            replies.push_back(reply(
                "https://api.github.com/repos/fixture-publisher/fixture-product/releases/latest",
                serde_json::to_vec(&self.json()).unwrap(),
            ));
        }
        replies.push_back(reply(&self.asset_url(MANIFEST_NAME), manifest));
        replies.push_back(reply(
            &self.asset_url(&format!("{MANIFEST_NAME}.sig")),
            signature,
        ));
        replies.push_back(reply(&self.asset_url(ASSET), self.binary.clone()));
        FakeTransport {
            replies,
            requests: Vec::new(),
        }
    }

    fn asset_url(&self, name: &str) -> String {
        format!(
            "https://github.com/fixture-publisher/fixture-product/releases/download/{}/{name}",
            self.manifest.tag
        )
    }

    fn candidate_version(&mut self, version: &str) {
        self.manifest.version = version.to_owned();
        self.manifest.tag = format!("rust-v{version}");
    }

    fn download(&self) -> DownloadedUpdate {
        let engine = self.engine();
        let clock = Clock::new();
        let mut transport = self.transport(true);
        let checked = engine
            .check(&ReleaseSelector::LatestStable, &mut transport, &clock)
            .unwrap();
        engine.download(checked, &mut transport, &clock).unwrap()
    }
}

fn repository() -> Repository {
    Repository::new("fixture-publisher", "fixture-product").unwrap()
}

fn identity(version: &str) -> Identity {
    Identity::new(version, Channel::Stable, Target::WindowsX64, VARIANT).unwrap()
}

fn asset_url(asset: &str) -> String {
    format!("https://github.com/fixture-publisher/fixture-product/releases/download/{TAG}/{asset}")
}

fn reply(url: &str, body: Vec<u8>) -> Reply {
    Reply {
        expected: url.to_owned(),
        body,
        status: 200,
        location: None,
    }
}

fn binary() -> Vec<u8> {
    // A non-running PE header fixture. It is not downloaded or launched.
    let mut bytes = vec![0; 256];
    bytes[..2].copy_from_slice(b"MZ");
    bytes[0x3c..0x40].copy_from_slice(&64_u32.to_le_bytes());
    bytes[64..68].copy_from_slice(b"PE\0\0");
    bytes[68..70].copy_from_slice(&0x8664_u16.to_le_bytes());
    bytes[86..88].copy_from_slice(&0x0002_u16.to_le_bytes());
    bytes[88..90].copy_from_slice(&0x020b_u16.to_le_bytes());
    bytes
}

fn hash(bytes: &[u8]) -> [u8; 32] {
    let mut value = [0; 32];
    value.copy_from_slice(digest::digest(&digest::SHA256, bytes).as_ref());
    value
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|value| format!("{value:02x}")).collect()
}

fn install_context() -> InstallContext {
    InstallContext {
        custody: InstallCustody::Standalone,
        budget_ms: 1000,
        allow_major: false,
    }
}

#[derive(Default)]
struct CountingInstaller {
    calls: usize,
}

impl Installer for CountingInstaller {
    type Receipt = ();

    fn install(
        &mut self,
        update: &DownloadedUpdate,
        _context: InstallContext,
        _control: &dyn Control,
    ) -> Result<(), Error> {
        assert_eq!(update.bytes(), binary());
        self.calls += 1;
        Ok(())
    }
}

#[test]
fn major_upgrade_check_reports_opt_in_and_explicit_install_is_the_only_override() {
    let mut case = Case::new();
    case.candidate_version("2.0.0");
    let engine = case.engine();
    let mut transport = case.transport(true);
    let checked = engine
        .check(
            &ReleaseSelector::LatestStable,
            &mut transport,
            &Clock::new(),
        )
        .unwrap();
    assert!(checked.requires_major_opt_in());
    assert_eq!(checked.manifest().manifest().version, "2.0.0");
    let downloaded = engine
        .download(checked, &mut transport, &Clock::new())
        .unwrap();
    assert!(downloaded.requires_major_opt_in());
    let mut installer = CountingInstaller::default();
    assert!(matches!(
        engine.install(downloaded, install_context(), &mut installer, &Clock::new()),
        Err(Error::MajorUpgradeRequiresOptIn)
    ));
    assert_eq!(installer.calls, 0);
    let context = InstallContext {
        allow_major: true,
        ..install_context()
    };
    let receipt = engine
        .install(case.download(), context, &mut installer, &Clock::new())
        .unwrap();
    assert_eq!(receipt.version, "2.0.0");
    assert_eq!(installer.calls, 1);
}

#[test]
fn same_major_including_zero_major_minor_changes_need_no_major_override() {
    for (current, candidate) in [("1.9.0", "1.10.0"), ("0.1.0", "0.2.0")] {
        let mut case = Case::new();
        case.identity = identity(current);
        case.candidate_version(candidate);
        let downloaded = case.download();
        assert!(!downloaded.requires_major_opt_in());
        let mut installer = CountingInstaller::default();
        case.engine()
            .install(downloaded, install_context(), &mut installer, &Clock::new())
            .unwrap();
        assert_eq!(installer.calls, 1);
    }
}

#[test]
fn repository_and_target_are_pinned_not_urls_or_unknown_platforms() {
    for (owner, name) in [
        ("https://github.com", "repo"),
        ("owner@github.com", "repo"),
        ("owner", "../repo"),
        ("owner", "repo?token=secret"),
        ("", "repo"),
        ("-owner", "repo"),
    ] {
        assert_eq!(Repository::new(owner, name), Err(Error::InvalidIdentity));
    }
    assert_eq!(
        Target::parse("unknown-unknown-unknown"),
        Err(Error::UnsupportedTarget)
    );
    assert!(Trust::new(repository(), None).is_err());
    assert!(Trust::new(repository(), Some([0; 32])).is_err());
    assert!(matches!(
        UpdateEngine::new(None, identity("1.9.0"), Limits::default()),
        Err(Error::TrustRequired)
    ));
}

#[test]
fn valid_signature_uses_semantic_not_lexical_version_order() {
    let case = Case::new();
    let (bytes, signature) = case.signed();
    assert_eq!(case.verify(&bytes, &signature), Ok(()));
}

#[test]
fn exact_bytes_are_verified_before_json_parsing() {
    let case = Case::new();
    let (mut bytes, signature) = case.signed();
    bytes[0] = b'!';
    assert_eq!(case.verify(&bytes, &signature), Err(Error::Signature));
    let signed_invalid_json = case.key.sign(&bytes);
    assert_eq!(
        case.verify(&bytes, signed_invalid_json.as_ref()),
        Err(Error::Schema)
    );
}

#[test]
fn signature_tampering_missing_signature_and_wrong_pinned_key_fail() {
    let mut case = Case::new();
    let (bytes, mut signature) = case.signed();
    signature[0] ^= 1;
    assert_eq!(case.verify(&bytes, &signature), Err(Error::Signature));
    assert_eq!(case.verify(&bytes, &[]), Err(Error::Signature));
    let (bytes, signature) = case.signed();
    case.trust = Trust::new(repository(), Some([3; 32])).unwrap();
    assert_eq!(case.verify(&bytes, &signature), Err(Error::Signature));
}

#[test]
fn strict_signed_schema_rejects_unknown_duplicate_missing_and_protocol_fields() {
    let case = Case::new();
    let (bytes, _) = case.signed();
    let text = String::from_utf8(bytes).unwrap();
    for bad in [
        text.replacen('{', "{\"unknown\":0,", 1),
        text.replacen('{', "{\"protocol\":1,", 1),
        text.replace("\"protocol\":1,", ""),
    ] {
        let signature = case.key.sign(bad.as_bytes());
        assert_eq!(
            case.verify(bad.as_bytes(), signature.as_ref()),
            Err(Error::Schema)
        );
    }
    let mut case = case;
    case.manifest.protocol = 2;
    let (bytes, signature) = case.signed();
    assert_eq!(case.verify(&bytes, &signature), Err(Error::Binding));
}

#[test]
fn every_signed_identity_field_is_bound() {
    let mut case = Case::new();
    for field in [
        "product",
        "repository",
        "tag",
        "version",
        "target",
        "variant",
        "channel",
    ] {
        let mut value = serde_json::to_value(&case.manifest).unwrap();
        value[field] = match field {
            "version" => json!("1.11.0"),
            "channel" => json!("preview"),
            _ => json!("other"),
        };
        let bytes = serde_json::to_vec(&value).unwrap();
        let signature = case.key.sign(&bytes);
        assert_eq!(
            case.verify(&bytes, signature.as_ref()),
            Err(Error::Binding),
            "{field}"
        );
    }
    case.manifest.asset = "../fab.exe".to_owned();
    let (bytes, signature) = case.signed();
    assert_eq!(case.verify(&bytes, &signature), Err(Error::Schema));
}

#[test]
fn expiry_invalid_digest_and_size_caps_fail_closed() {
    for expiry in [NOW, NOW - 1, NOW + 32 * 24 * 3600] {
        let mut case = Case::new();
        case.manifest.expires_at = expiry;
        let (bytes, signature) = case.signed();
        assert_eq!(case.verify(&bytes, &signature), Err(Error::Expired));
    }
    let mut case = Case::new();
    case.manifest.sha256 = "A".repeat(64);
    let (bytes, signature) = case.signed();
    assert_eq!(case.verify(&bytes, &signature), Err(Error::Schema));
    case.manifest.bytes = 0;
    let (bytes, signature) = case.signed();
    assert_eq!(case.verify(&bytes, &signature), Err(Error::Limit));
    case.manifest.bytes = u64::MAX;
    let (bytes, signature) = case.signed();
    assert_eq!(case.verify(&bytes, &signature), Err(Error::Limit));
    assert_eq!(
        case.verify(&vec![0; Limits::default().manifest_bytes + 1], &signature),
        Err(Error::Limit)
    );
}

#[test]
fn downgrade_equal_version_and_build_metadata_replay_are_ineligible() {
    for current in ["1.10.0", "1.11.0", "1.10.0+build2"] {
        let mut case = Case::new();
        case.identity = identity(current);
        let (bytes, signature) = case.signed();
        assert_eq!(case.verify(&bytes, &signature), Err(Error::Ineligible));
    }
}

#[test]
fn preview_requires_exact_channel_tag_and_prerelease_flag() {
    let mut case = Case::new();
    let tag = "rust-v1.10.0-beta.1";
    case.identity = Identity::new(
        "1.9.0-beta.1",
        Channel::Preview,
        Target::WindowsX64,
        VARIANT,
    )
    .unwrap();
    case.manifest.tag = tag.to_owned();
    case.manifest.version = "1.10.0-beta.1".to_owned();
    case.manifest.channel = Channel::Preview;
    let mut json = case.json();
    json["tag_name"] = json!(tag);
    json["prerelease"] = json!(true);
    json["html_url"] = json!(format!(
        "https://github.com/fixture-publisher/fixture-product/releases/tag/{tag}"
    ));
    let clock = Clock::new();
    let mut transport = case.transport(false);
    let checked = case
        .engine()
        .check_release_json(
            &ReleaseSelector::ExactTag(tag.to_owned()),
            &serde_json::to_vec(&json).unwrap(),
            &mut transport,
            &clock,
        )
        .unwrap();
    assert_eq!(checked.manifest().manifest().channel, Channel::Preview);
    assert!(matches!(
        case.engine()
            .check(&ReleaseSelector::LatestStable, &mut transport, &clock),
        Err(Error::Ineligible)
    ));
}

#[test]
fn public_discovery_generated_urls_download_and_injected_install_end_to_end() {
    struct RecordingInstaller {
        calls: usize,
    }
    impl Installer for RecordingInstaller {
        type Receipt = usize;
        fn install(
            &mut self,
            update: &DownloadedUpdate,
            context: InstallContext,
            _control: &dyn Control,
        ) -> Result<Self::Receipt, Error> {
            assert_eq!(context.custody, InstallCustody::Standalone);
            assert_eq!(update.bytes(), binary());
            self.calls += 1;
            Ok(self.calls)
        }
    }
    let case = Case::new();
    let engine = case.engine();
    let clock = Clock::new();
    let mut transport = case.transport(true);
    let checked = engine
        .check(&ReleaseSelector::LatestStable, &mut transport, &clock)
        .unwrap();
    assert_eq!(checked.manifest().manifest().version, "1.10.0");
    assert_eq!(transport.requests.len(), 3);
    let downloaded = engine.download(checked, &mut transport, &clock).unwrap();
    assert_eq!(downloaded.bytes(), case.binary);
    let mut installer = RecordingInstaller { calls: 0 };
    let receipt = engine
        .install(downloaded, install_context(), &mut installer, &clock)
        .unwrap();
    assert_eq!(receipt.version, "1.10.0");
    assert_eq!(receipt.adapter, 1);
    assert_eq!(installer.calls, 1);
    assert_eq!(transport.requests.len(), 4);
    assert!(transport.replies.is_empty());
}

#[test]
fn rust_discovery_ignores_python_releases_and_selects_semantic_maximum_once() {
    let case = Case::new();
    let mut python = case.json();
    python["tag_name"] = json!("v99.0.0");
    let mut older = case.json();
    older["id"] = json!(122);
    older["url"] =
        json!("https://api.github.com/repos/fixture-publisher/fixture-product/releases/122");
    older["tag_name"] = json!("rust-v1.9.9");
    older["html_url"] =
        json!("https://github.com/fixture-publisher/fixture-product/releases/tag/rust-v1.9.9");
    let mut transport = case.transport(true);
    let discovery = transport.replies.front_mut().unwrap();
    discovery.expected =
        "https://api.github.com/repos/fixture-publisher/fixture-product/releases?per_page=20"
            .to_owned();
    discovery.body = serde_json::to_vec(&vec![python, older, case.json()]).unwrap();
    let checked = case
        .engine()
        .check(
            &ReleaseSelector::LatestRustStable,
            &mut transport,
            &Clock::new(),
        )
        .unwrap();
    assert_eq!(checked.manifest().manifest().version, "1.10.0");
    assert_eq!(transport.requests.len(), 3);
}

#[test]
fn rust_discovery_candidate_cap_and_ambiguous_versions_fail_without_asset_requests() {
    let case = Case::new();
    for duplicate in [false, true] {
        let releases = if duplicate {
            vec![case.json(), case.json()]
        } else {
            vec![case.json(); Limits::default().release_candidates + 1]
        };
        let mut transport = case.transport(false);
        let result = case.engine().check_release_json(
            &ReleaseSelector::LatestRustStable,
            &serde_json::to_vec(&releases).unwrap(),
            &mut transport,
            &Clock::new(),
        );
        assert!(
            matches!(result, Err(error) if error == if duplicate { Error::Schema } else { Error::Limit })
        );
        assert!(transport.requests.is_empty());
    }
}

#[test]
fn python_only_releases_and_missing_rust_metadata_are_not_configured_not_upgrades() {
    let mut case = Case::new();
    case.identity = identity("0.1.0");
    let mut python = case.json();
    python["tag_name"] = json!("v1.7.0");
    python["assets"] = json!([]);
    let mut transport = case.transport(false);
    assert!(matches!(
        case.engine().check_release_json(
            &ReleaseSelector::LatestRustStable,
            &serde_json::to_vec(&vec![python.clone()]).unwrap(),
            &mut transport,
            &Clock::new(),
        ),
        Err(Error::ReleaseUnavailable)
    ));
    assert!(transport.requests.is_empty());
    assert!(matches!(
        case.engine().check_release_json(
            &ReleaseSelector::LatestStable,
            &serde_json::to_vec(&python).unwrap(),
            &mut transport,
            &Clock::new(),
        ),
        Err(Error::InvalidIdentity)
    ));
    assert!(transport.requests.is_empty());
    let mut rust_without_metadata = case.json();
    rust_without_metadata["assets"] = json!([]);
    assert!(matches!(
        case.engine().check_release_json(
            &ReleaseSelector::LatestRustStable,
            &serde_json::to_vec(&vec![rust_without_metadata]).unwrap(),
            &mut transport,
            &Clock::new(),
        ),
        Err(Error::ReleaseUnavailable)
    ));
    assert!(transport.requests.is_empty());
}

#[test]
fn unpublished_selected_rust_release_never_falls_back_to_an_older_signed_candidate() {
    let case = Case::new();
    let mut newer = case.json();
    newer["id"] = json!(124);
    newer["url"] =
        json!("https://api.github.com/repos/fixture-publisher/fixture-product/releases/124");
    newer["html_url"] =
        json!("https://github.com/fixture-publisher/fixture-product/releases/tag/rust-v1.11.0");
    newer["tag_name"] = json!("rust-v1.11.0");
    newer["assets"] = json!([]);
    let mut transport = case.transport(false);
    assert!(matches!(
        case.engine().check_release_json(
            &ReleaseSelector::LatestRustStable,
            &serde_json::to_vec(&vec![newer, case.json()]).unwrap(),
            &mut transport,
            &Clock::new(),
        ),
        Err(Error::ReleaseUnavailable)
    ));
    assert!(transport.requests.is_empty());
}

#[test]
fn caller_json_still_rejects_wrong_repository_release_and_duplicate_assets() {
    let case = Case::new();
    let clock = Clock::new();
    for field in ["url", "html_url"] {
        let mut value = case.json();
        value[field] = json!("https://github.com/attacker/product");
        let mut transport = case.transport(false);
        assert!(matches!(
            case.engine().check_release_json(
                &ReleaseSelector::LatestStable,
                &serde_json::to_vec(&value).unwrap(),
                &mut transport,
                &clock
            ),
            Err(Error::Origin)
        ));
        assert!(transport.requests.is_empty());
    }
    let mut value = case.json();
    let duplicate = value["assets"][0].clone();
    value["assets"].as_array_mut().unwrap().push(duplicate);
    let mut transport = case.transport(false);
    assert!(matches!(
        case.engine().check_release_json(
            &ReleaseSelector::LatestStable,
            &serde_json::to_vec(&value).unwrap(),
            &mut transport,
            &clock
        ),
        Err(Error::Schema)
    ));
}

#[test]
fn draft_prerelease_wrong_exact_tag_and_asset_count_fail_before_network() {
    let case = Case::new();
    let clock = Clock::new();
    for field in ["draft", "prerelease"] {
        let mut value = case.json();
        value[field] = json!(true);
        let mut transport = case.transport(false);
        assert!(matches!(
            case.engine().check_release_json(
                &ReleaseSelector::LatestStable,
                &serde_json::to_vec(&value).unwrap(),
                &mut transport,
                &clock
            ),
            Err(Error::Ineligible)
        ));
        assert!(transport.requests.is_empty());
    }
    let mut transport = case.transport(false);
    assert!(matches!(
        case.engine().check_release_json(
            &ReleaseSelector::ExactTag("rust-v1.11.0".to_owned()),
            &serde_json::to_vec(&case.json()).unwrap(),
            &mut transport,
            &clock
        ),
        Err(Error::Binding)
    ));
    let limits = Limits {
        release_assets: 2,
        ..Limits::default()
    };
    let engine =
        UpdateEngine::new(Some(case.trust.clone()), case.identity.clone(), limits).unwrap();
    assert!(matches!(
        engine.check_release_json(
            &ReleaseSelector::LatestStable,
            &serde_json::to_vec(&case.json()).unwrap(),
            &mut transport,
            &clock
        ),
        Err(Error::Limit)
    ));
}

#[test]
fn tampered_truncated_and_oversized_downloads_never_reach_install() {
    for mutation in 0..3 {
        let case = Case::new();
        let engine = case.engine();
        let clock = Clock::new();
        let mut transport = case.transport(true);
        let checked = engine
            .check(&ReleaseSelector::LatestStable, &mut transport, &clock)
            .unwrap();
        let response = transport.replies.front_mut().unwrap();
        match mutation {
            0 => response.body[100] ^= 1,
            1 => {
                response.body.pop();
            }
            _ => response.body.push(1),
        }
        assert_eq!(
            engine
                .download(checked, &mut transport, &clock)
                .unwrap_err(),
            if mutation == 2 {
                Error::Limit
            } else {
                Error::Integrity
            }
        );
    }
}

#[test]
fn signed_script_archive_or_wrong_architecture_is_not_a_raw_native_binary() {
    for bytes in [
        b"#!/bin/sh\nexit 0\n".to_vec(),
        b"PK\x03\x04zip".to_vec(),
        {
            let mut bytes = binary();
            bytes[68..70].copy_from_slice(&0xaa64_u16.to_le_bytes());
            bytes
        },
    ] {
        let mut case = Case::new();
        case.binary = bytes;
        case.manifest.bytes = case.binary.len() as u64;
        case.manifest.sha256 = hex(&hash(&case.binary));
        let engine = case.engine();
        let clock = Clock::new();
        let mut transport = case.transport(true);
        let checked = engine
            .check(&ReleaseSelector::LatestStable, &mut transport, &clock)
            .unwrap();
        assert_eq!(
            engine
                .download(checked, &mut transport, &clock)
                .unwrap_err(),
            Error::BinaryFormat
        );
    }
}

#[test]
fn only_allowlisted_https_cdn_redirects_are_followed_without_auth() {
    let case = Case::new();
    let clock = Clock::new();
    for location in [
        "https://evil.invalid/asset",
        "http://objects.githubusercontent.com/asset",
        "https://objects.githubusercontent.com.evil.invalid/asset",
        "https://user@objects.githubusercontent.com/asset",
        "https://objects.githubusercontent.com:443/asset",
        "//objects.githubusercontent.com/asset",
        "https://objects.githubusercontent.com/asset#fragment",
        "https://objects.githubusercontent.com/\\evil",
    ] {
        let mut transport = case.transport(false);
        let first = transport.replies.front_mut().unwrap();
        first.body.clear();
        first.status = 302;
        first.location = Some(location.to_owned());
        assert!(matches!(
            case.engine().check_release_json(
                &ReleaseSelector::LatestStable,
                &serde_json::to_vec(&case.json()).unwrap(),
                &mut transport,
                &clock
            ),
            Err(Error::Origin)
        ));
        assert_eq!(transport.requests.len(), 1);
    }
    let mut transport = case.transport(false);
    let redirected = transport.replies.pop_front().unwrap();
    transport.replies.push_front(reply(
        "https://release-assets.githubusercontent.com/github-production-release-asset/asset?sig=fixture",
        redirected.body,
    ));
    transport.replies.push_front(Reply {
        expected: redirected.expected,
        body: Vec::new(),
        status: 302,
        location: Some("https://release-assets.githubusercontent.com/github-production-release-asset/asset?sig=fixture".to_owned()),
    });
    assert!(
        case.engine()
            .check_release_json(
                &ReleaseSelector::LatestStable,
                &serde_json::to_vec(&case.json()).unwrap(),
                &mut transport,
                &clock,
            )
            .is_ok()
    );
    assert_eq!(transport.requests.len(), 3);
}

#[test]
fn redirect_chain_and_service_errors_are_not_retried_or_cached() {
    let case = Case::new();
    let clock = Clock::new();
    let mut transport = case.transport(false);
    transport.replies.clear();
    for hop in 0..4 {
        transport.replies.push_back(Reply {
            expected: if hop == 0 {
                asset_url(MANIFEST_NAME)
            } else {
                "https://objects.githubusercontent.com/loop".to_owned()
            },
            body: Vec::new(),
            status: 302,
            location: Some("https://objects.githubusercontent.com/loop".to_owned()),
        });
    }
    assert!(matches!(
        case.engine().check_release_json(
            &ReleaseSelector::LatestStable,
            &serde_json::to_vec(&case.json()).unwrap(),
            &mut transport,
            &clock
        ),
        Err(Error::Origin)
    ));
    assert_eq!(transport.requests.len(), 4);
    let mut transport = case.transport(true);
    transport.replies.front_mut().unwrap().status = 503;
    assert!(matches!(
        case.engine()
            .check(&ReleaseSelector::LatestStable, &mut transport, &clock),
        Err(Error::Response)
    ));
    assert_eq!(transport.requests.len(), 1);
}

#[test]
fn cancellation_deadline_backwards_clock_and_expired_candidate_prevent_download() {
    for change in 0..4 {
        let case = Case::new();
        let clock = Clock::new();
        let engine = case.engine();
        let mut transport = case.transport(true);
        let checked = engine
            .check(&ReleaseSelector::LatestStable, &mut transport, &clock)
            .unwrap();
        clock.alter(|snapshot| match change {
            0 => snapshot.cancelled = true,
            1 => snapshot.monotonic_ms += Limits::default().network_ms,
            2 => snapshot.monotonic_ms = 0,
            _ => snapshot.unix_seconds += 4000,
        });
        assert_eq!(
            engine
                .download(checked, &mut transport, &clock)
                .unwrap_err(),
            match change {
                0 => Error::Cancelled,
                1 | 2 => Error::Deadline,
                _ => Error::Expired,
            }
        );
        assert_eq!(transport.requests.len(), 3);
    }
}

#[test]
fn transport_must_not_hide_sink_overflow_or_stream_cancellation() {
    struct Misbehaving;
    impl HttpTransport for Misbehaving {
        fn get(
            &mut self,
            request: &HttpRequest,
            sink: &mut dyn ResponseSink,
            _control: &dyn Control,
        ) -> Result<HttpResponse, Error> {
            let _ = sink.write(&vec![0; request.max_bytes + 1]);
            Ok(HttpResponse {
                status: 200,
                location: None,
            })
        }
    }
    let case = Case::new();
    assert!(matches!(
        case.engine().check(
            &ReleaseSelector::LatestStable,
            &mut Misbehaving,
            &Clock::new()
        ),
        Err(Error::Limit)
    ));
    struct SignalDuringStream<'a> {
        clock: &'a Clock,
        cancel: bool,
    }
    impl HttpTransport for SignalDuringStream<'_> {
        fn get(
            &mut self,
            _request: &HttpRequest,
            sink: &mut dyn ResponseSink,
            _control: &dyn Control,
        ) -> Result<HttpResponse, Error> {
            sink.write(b"{")?;
            self.clock.alter(|snapshot| {
                if self.cancel {
                    snapshot.cancelled = true;
                } else {
                    snapshot.monotonic_ms += Limits::default().network_ms;
                }
            });
            let _ = sink.write(b"}");
            Ok(HttpResponse {
                status: 200,
                location: None,
            })
        }
    }
    for cancel in [false, true] {
        let clock = Clock::new();
        let mut transport = SignalDuringStream {
            clock: &clock,
            cancel,
        };
        assert!(matches!(
            case.engine()
                .check(&ReleaseSelector::LatestStable, &mut transport, &clock),
            Err(error) if error == if cancel { Error::Cancelled } else { Error::Deadline }
        ));
    }
}

#[test]
fn install_custody_and_injected_write_copy_failures_are_typed_and_not_success() {
    struct Failure {
        error: Option<Error>,
        calls: usize,
    }
    impl Installer for Failure {
        type Receipt = ();
        fn install(
            &mut self,
            _update: &DownloadedUpdate,
            _context: InstallContext,
            _control: &dyn Control,
        ) -> Result<(), Error> {
            self.calls += 1;
            Err(self.error.take().unwrap())
        }
    }
    let case = Case::new();
    for custody in [InstallCustody::PackageManager, InstallCustody::Unknown] {
        let mut installer = Failure {
            error: Some(Error::Io),
            calls: 0,
        };
        let mut context = install_context();
        context.custody = custody;
        assert_eq!(
            case.engine()
                .install(case.download(), context, &mut installer, &Clock::new())
                .unwrap_err(),
            Error::Custody
        );
        assert_eq!(installer.calls, 0);
    }
    for error in [Error::Io, Error::Install, Error::Lock, Error::Cancelled] {
        let expected = match error {
            Error::Io => Error::Io,
            Error::Install => Error::Install,
            Error::Lock => Error::Lock,
            _ => Error::Cancelled,
        };
        let mut installer = Failure {
            error: Some(error),
            calls: 0,
        };
        assert_eq!(
            case.engine()
                .install(
                    case.download(),
                    install_context(),
                    &mut installer,
                    &Clock::new()
                )
                .unwrap_err(),
            expected
        );
        assert_eq!(installer.calls, 1);
    }
}

#[test]
fn automatic_policy_is_explicit_finite_and_never_networks_help_ci_or_redirects() {
    let terminal = AutomaticContext {
        terminal: true,
        help_or_version: false,
        automation: false,
    };
    let mut schedule = CheckSchedule::default();
    assert_eq!(
        schedule.startup(Policy::Off, terminal, 100),
        StartupDecision::Skip
    );
    for context in [
        AutomaticContext {
            terminal: false,
            ..terminal
        },
        AutomaticContext {
            help_or_version: true,
            ..terminal
        },
        AutomaticContext {
            automation: true,
            ..terminal
        },
    ] {
        assert_eq!(
            schedule.startup(Policy::Install, context, 100),
            StartupDecision::Skip
        );
    }
    assert_eq!(
        schedule.startup(Policy::Check, terminal, 100),
        StartupDecision::Check
    );
    assert_eq!(
        schedule.startup(Policy::Install, terminal, 101),
        StartupDecision::Skip
    );
    assert_eq!(
        schedule.startup(Policy::Install, terminal, 100 + 24 * 3600 * 1000),
        StartupDecision::CheckAndInstall
    );
    assert_eq!(
        schedule.startup(Policy::Check, terminal, 1),
        StartupDecision::Skip
    );
    assert_eq!(
        schedule.startup(Policy::Check, terminal, 2),
        StartupDecision::Skip
    );
}

static FIXTURE_ID: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    directory: PathBuf,
    path: PathBuf,
    installation: FileInstall,
    old: Vec<u8>,
}

impl Fixture {
    fn new() -> Self {
        let root = support::test_root();
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = root.join(format!(
            "updater-fixture-{}-{unique:x}-{}",
            std::process::id(),
            FIXTURE_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&directory).unwrap();
        let path = directory.join("fixture.exe");
        let mut old = binary();
        old[255] = 9;
        fs::write(&path, &old).unwrap();
        let installation = FileInstall::new(path.clone(), hash(&old), [11; 32]).unwrap();
        Self {
            directory,
            path,
            installation,
            old,
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        for path in [
            self.path.clone(),
            self.installation.stage_path(),
            self.installation.rollback_path(),
            self.installation.lock_path(),
        ] {
            let _ = fs::remove_file(&path);
            let _ = fs::remove_dir(&path);
        }
        let _ = fs::remove_dir(&self.directory);
    }
}

#[test]
fn real_non_running_fixture_replacement_retains_rollback_and_rejects_replay() {
    let fixture = Fixture::new();
    let case = Case::new();
    let mut installer = FileInstaller::new(fixture.installation.clone());
    let receipt = case
        .engine()
        .install(
            case.download(),
            install_context(),
            &mut installer,
            &Clock::new(),
        )
        .unwrap();
    assert_eq!(fs::read(&fixture.path).unwrap(), case.binary);
    assert_eq!(
        fs::read(&receipt.adapter.rollback_path).unwrap(),
        fixture.old
    );
    assert_eq!(receipt.adapter.installed_sha256, hash(&case.binary));
    assert_eq!(receipt.adapter.previous_sha256, hash(&fixture.old));
    assert!(!fixture.installation.stage_path().exists());
    assert!(!fixture.installation.lock_path().exists());
    assert_eq!(
        case.engine()
            .install(
                case.download(),
                install_context(),
                &mut installer,
                &Clock::new(),
            )
            .unwrap_err(),
        Error::InstallConflict
    );
    assert_eq!(fs::read(&fixture.path).unwrap(), case.binary);
}

#[test]
fn both_real_adapters_refuse_major_changes_before_any_installation_mutation() {
    let fixture = Fixture::new();
    let mut case = Case::new();
    case.candidate_version("2.0.0");
    let downloaded = case.download();
    let mut installer = FileInstaller::new(fixture.installation.clone());
    assert!(matches!(
        installer.install(&downloaded, install_context(), &Clock::new()),
        Err(Error::MajorUpgradeRequiresOptIn)
    ));
    #[cfg(feature = "running-replacement")]
    {
        let mut running = fabric_cli_update::RunningInstaller::new(
            fixture.installation.clone(),
            fabric_cli_update::ExitAcknowledgement::ExitImmediatelyAndRetainRecovery,
        );
        assert!(matches!(
            running.queue(
                &downloaded,
                install_context(),
                &Clock::new(),
                &mut fabric_cli_update::ParentLifetime::new()
            ),
            Err(Error::MajorUpgradeRequiresOptIn)
        ));
    }
    assert_eq!(fs::read(&fixture.path).unwrap(), fixture.old);
    assert!(!fixture.installation.lock_path().exists());
    assert!(!fixture.installation.stage_path().exists());
    assert!(!fixture.installation.rollback_path().exists());
}

#[test]
fn existing_lock_stage_or_rollback_never_overwrites_or_deletes_other_files() {
    for conflict in 0..3 {
        let fixture = Fixture::new();
        let blocker = match conflict {
            0 => fixture.installation.lock_path(),
            1 => fixture.installation.stage_path(),
            _ => fixture.installation.rollback_path(),
        };
        fs::write(&blocker, b"other-owner").unwrap();
        let case = Case::new();
        let mut installer = FileInstaller::new(fixture.installation.clone());
        assert_eq!(
            case.engine()
                .install(
                    case.download(),
                    install_context(),
                    &mut installer,
                    &Clock::new(),
                )
                .unwrap_err(),
            if conflict == 0 {
                Error::Lock
            } else {
                Error::InstallConflict
            }
        );
        assert_eq!(fs::read(&fixture.path).unwrap(), fixture.old);
        assert_eq!(fs::read(&blocker).unwrap(), b"other-owner");
    }
}

#[test]
fn lost_lock_is_detected_and_foreign_lock_not_cleaned_up() {
    let fixture = Fixture::new();
    let case = Case::new();
    let stage = fixture.installation.stage_path();
    let lock = fixture.installation.lock_path();
    let clock = HookClock {
        clock: Clock::new(),
        hook: RefCell::new(|| {
            if stage.exists() {
                fs::write(&lock, [99; 32]).unwrap();
            }
        }),
    };
    let mut installer = FileInstaller::new(fixture.installation.clone());
    assert_eq!(
        case.engine()
            .install(case.download(), install_context(), &mut installer, &clock)
            .unwrap_err(),
        Error::Lock
    );
    assert_eq!(fs::read(&fixture.path).unwrap(), fixture.old);
    assert_eq!(fs::read(&lock).unwrap(), [99; 32]);
}

#[test]
fn real_replacement_failure_retains_current_file_and_rollback() {
    let fixture = Fixture::new();
    let case = Case::new();
    let stage = fixture.installation.stage_path();
    let rollback = fixture.installation.rollback_path();
    let sabotaged = Cell::new(false);
    let clock = HookClock {
        clock: Clock::new(),
        hook: RefCell::new(|| {
            if rollback.exists() && !sabotaged.replace(true) {
                fs::remove_file(&stage).unwrap();
                fs::create_dir(&stage).unwrap();
            }
        }),
    };
    let mut installer = FileInstaller::new(fixture.installation.clone());
    assert_eq!(
        case.engine()
            .install(case.download(), install_context(), &mut installer, &clock)
            .unwrap_err(),
        Error::Install
    );
    assert_eq!(fs::read(&fixture.path).unwrap(), fixture.old);
    assert_eq!(fs::read(&rollback).unwrap(), fixture.old);
}

#[test]
fn cancelled_install_does_not_stage_or_replace() {
    let fixture = Fixture::new();
    let case = Case::new();
    let clock = Clock::new();
    clock.alter(|snapshot| snapshot.cancelled = true);
    let mut installer = FileInstaller::new(fixture.installation.clone());
    assert_eq!(
        case.engine()
            .install(case.download(), install_context(), &mut installer, &clock)
            .unwrap_err(),
        Error::Cancelled
    );
    assert_eq!(fs::read(&fixture.path).unwrap(), fixture.old);
    assert!(!fixture.installation.stage_path().exists());
}

#[test]
fn readonly_install_is_rejected_without_changing_permissions() {
    let fixture = Fixture::new();
    let original = fs::metadata(&fixture.path).unwrap().permissions();
    let mut readonly = original.clone();
    readonly.set_readonly(true);
    fs::set_permissions(&fixture.path, readonly).unwrap();
    let case = Case::new();
    let mut installer = FileInstaller::new(fixture.installation.clone());
    let result = case.engine().install(
        case.download(),
        install_context(),
        &mut installer,
        &Clock::new(),
    );
    fs::set_permissions(&fixture.path, original).unwrap();
    assert_eq!(result.unwrap_err(), Error::Custody);
    assert_eq!(fs::read(&fixture.path).unwrap(), fixture.old);
}

#[test]
fn midstage_cancellation_and_expiry_do_not_replace_the_current_binary() {
    struct StageSignal {
        stage: PathBuf,
        expire: bool,
    }
    impl Control for StageSignal {
        fn snapshot(&self) -> Snapshot {
            let staged = self.stage.exists();
            Snapshot {
                unix_seconds: if staged && self.expire {
                    NOW + 4000
                } else {
                    NOW
                },
                monotonic_ms: 100,
                cancelled: staged && !self.expire,
            }
        }
    }
    for expire in [false, true] {
        let fixture = Fixture::new();
        let case = Case::new();
        let clock = StageSignal {
            stage: fixture.installation.stage_path(),
            expire,
        };
        let mut installer = FileInstaller::new(fixture.installation.clone());
        assert_eq!(
            case.engine()
                .install(case.download(), install_context(), &mut installer, &clock)
                .unwrap_err(),
            if expire {
                Error::Expired
            } else {
                Error::Cancelled
            }
        );
        assert_eq!(fs::read(&fixture.path).unwrap(), fixture.old);
        assert!(!fixture.installation.stage_path().exists());
        assert!(!fixture.installation.lock_path().exists());
    }
}

#[test]
fn final_snapshot_cancellation_deadline_and_clock_rollback_abort_before_replace() {
    struct LastSnapshotSignal {
        rollback: PathBuf,
        after_rollback: Cell<usize>,
        mode: u8,
    }

    impl Control for LastSnapshotSignal {
        fn snapshot(&self) -> Snapshot {
            let mut snapshot = Snapshot {
                unix_seconds: NOW,
                monotonic_ms: 100,
                cancelled: false,
            };
            if self.rollback.exists() {
                let count = self.after_rollback.get() + 1;
                self.after_rollback.set(count);
                // The 256-byte rollback hash has two checkpoints, followed by
                // one lock checkpoint and the final pre-commit snapshot.
                if count == 4 {
                    match self.mode {
                        0 => snapshot.cancelled = true,
                        1 => snapshot.monotonic_ms += install_context().budget_ms,
                        _ => snapshot.monotonic_ms = 99,
                    }
                }
            }
            snapshot
        }
    }

    for mode in 0..3 {
        let fixture = Fixture::new();
        let case = Case::new();
        let clock = LastSnapshotSignal {
            rollback: fixture.installation.rollback_path(),
            after_rollback: Cell::new(0),
            mode,
        };
        let mut installer = FileInstaller::new(fixture.installation.clone());
        assert_eq!(
            case.engine()
                .install(case.download(), install_context(), &mut installer, &clock)
                .unwrap_err(),
            if mode == 0 {
                Error::Cancelled
            } else {
                Error::Deadline
            }
        );
        assert_eq!(clock.after_rollback.get(), 4);
        assert_eq!(fs::read(&fixture.path).unwrap(), fixture.old);
        assert_eq!(
            fs::read(fixture.installation.rollback_path()).unwrap(),
            fixture.old
        );
        assert!(!fixture.installation.stage_path().exists());
        assert!(!fixture.installation.lock_path().exists());
    }
}

#[test]
fn cancellation_observed_after_commit_does_not_relabel_installed_bytes_as_failure() {
    struct PostCommitSignal {
        path: PathBuf,
        installed_bytes: Vec<u8>,
    }

    impl Control for PostCommitSignal {
        fn snapshot(&self) -> Snapshot {
            let committed = fs::read(&self.path).is_ok_and(|bytes| bytes == self.installed_bytes);
            Snapshot {
                unix_seconds: if committed { NOW + 4000 } else { NOW },
                monotonic_ms: if committed { 0 } else { 100 },
                cancelled: committed,
            }
        }
    }

    let fixture = Fixture::new();
    let case = Case::new();
    let clock = PostCommitSignal {
        path: fixture.path.clone(),
        installed_bytes: case.binary.clone(),
    };
    let mut installer = FileInstaller::new(fixture.installation.clone());
    let receipt = case
        .engine()
        .install(case.download(), install_context(), &mut installer, &clock)
        .unwrap();
    assert!(clock.snapshot().cancelled);
    assert_eq!(receipt.adapter.installed_sha256, hash(&case.binary));
    assert_eq!(fs::read(&fixture.path).unwrap(), case.binary);
    assert_eq!(
        fs::read(fixture.installation.rollback_path()).unwrap(),
        fixture.old
    );
    assert!(!fixture.installation.stage_path().exists());
    assert!(!fixture.installation.lock_path().exists());
}

#[test]
fn standalone_custody_is_not_permission_to_replace_a_non_binary_file() {
    let fixture = Fixture::new();
    fs::write(&fixture.path, b"not-an-installed-native-program").unwrap();
    let installation = FileInstall::new(
        fixture.path.clone(),
        hash(b"not-an-installed-native-program"),
        [22; 32],
    )
    .unwrap();
    let case = Case::new();
    let mut installer = FileInstaller::new(installation);
    assert_eq!(
        case.engine()
            .install(
                case.download(),
                install_context(),
                &mut installer,
                &Clock::new(),
            )
            .unwrap_err(),
        Error::BinaryFormat
    );
    assert_eq!(
        fs::read(&fixture.path).unwrap(),
        b"not-an-installed-native-program"
    );
}

#[test]
fn running_test_executable_is_rejected_without_launching_any_process() {
    let path = std::env::current_exe().unwrap();
    let installation = FileInstall::new(path, [1; 32], [11; 32]).unwrap();
    let case = Case::new();
    let mut installer = FileInstaller::new(installation);
    assert_eq!(
        case.engine()
            .install(
                case.download(),
                install_context(),
                &mut installer,
                &Clock::new(),
            )
            .unwrap_err(),
        Error::RunningExecutable
    );
}
