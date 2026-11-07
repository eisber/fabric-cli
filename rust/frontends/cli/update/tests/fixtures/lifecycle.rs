// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Locally installed parent fixture; never a release host or key fallback.

#![forbid(unsafe_code)]
#![deny(unnameable_test_items)]

use std::{
    collections::VecDeque,
    fs,
    process::ExitCode,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use fabric_cli_update::{
    Channel, Control, CustodyReceipt, Error, ExitAcknowledgement, FileInstall, HttpRequest,
    HttpResponse, HttpTransport, Identity, InstallContext, InstallCustody, Limits, Manifest,
    ParentLifetime, ReleaseSelector, ResponseSink, RunningInstaller, Snapshot, UpdateEngine,
};
use ring::digest;
use ring::rand::{SecureRandom, SystemRandom};
use serde_json::json;

mod common;

fn hash(bytes: &[u8]) -> [u8; 32] {
    let mut result = [0; 32];
    result.copy_from_slice(digest::digest(&digest::SHA256, bytes).as_ref());
    result
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

struct Clock(Instant);

impl Control for Clock {
    fn snapshot(&self) -> Snapshot {
        Snapshot {
            unix_seconds: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            monotonic_ms: self.0.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
            cancelled: false,
        }
    }
}

struct LocalBytes(VecDeque<Vec<u8>>);

impl HttpTransport for LocalBytes {
    fn get(
        &mut self,
        request: &HttpRequest,
        sink: &mut dyn ResponseSink,
        _control: &dyn Control,
    ) -> Result<HttpResponse, Error> {
        if !request.url.starts_with(
            "https://github.com/fixture-publisher/fixture-product/releases/download/rust-v",
        ) {
            return Err(Error::Origin);
        }
        for chunk in self
            .0
            .pop_front()
            .ok_or(Error::Transport)?
            .chunks(64 * 1024)
        {
            sink.write(chunk)?;
        }
        Ok(HttpResponse {
            status: 200,
            location: None,
        })
    }
}

fn run(lifetime: &mut ParentLifetime, version: &str, mode: &str) -> Result<(), Error> {
    let executable = std::env::current_exe()
        .and_then(fs::canonicalize)
        .map_err(|_| Error::Io)?;
    let root = executable.parent().ok_or(Error::Custody)?;
    if fs::read(root.join(".fixture-authorized")).map_err(|_| Error::Custody)? != b"offline-only" {
        return Err(Error::Custody);
    }
    let receipt: CustodyReceipt = serde_json::from_slice(
        &fs::read(root.join(".fab-rs-install.json")).map_err(|_| Error::Io)?,
    )
    .map_err(|_| Error::Schema)?;
    let old = fs::read(&executable).map_err(|_| Error::Io)?;
    if old.len() >= Limits::default().asset_bytes - 128 {
        return Err(Error::Limit);
    }
    let mut new = old.clone();
    new.extend_from_slice(format!("\0offline-local-upgrade-{version}\0").as_bytes());
    let target = common::target()?;
    let clock = Clock(Instant::now());
    let tag = format!("rust-v{version}");
    let manifest = Manifest {
        protocol: 1,
        product: "fab-rs".to_owned(),
        repository: "fixture-publisher/fixture-product".to_owned(),
        tag: tag.clone(),
        version: version.to_owned(),
        channel: Channel::Stable,
        target: target.as_str().to_owned(),
        variant: common::VARIANT.to_owned(),
        asset: "local-native-fixture.bin".to_owned(),
        bytes: new.len() as u64,
        sha256: hex(&hash(&new)),
        expires_at: clock.snapshot().unix_seconds + 3600,
    };
    let signed = serde_json::to_vec(&manifest).map_err(|_| Error::Schema)?;
    let signature = common::key().sign(&signed).as_ref().to_vec();
    let manifest_name = format!("update-{}-{}.json", target.as_str(), common::VARIANT);
    let release = json!({
        "id":1,
        "url":"https://api.github.com/repos/fixture-publisher/fixture-product/releases/1",
        "html_url":format!("https://github.com/fixture-publisher/fixture-product/releases/tag/{tag}"),
        "tag_name":tag,
        "draft":false,
        "prerelease":false,
        "assets":[
            {"name":manifest_name,"size":signed.len()},
            {"name":format!("{manifest_name}.sig"),"size":64},
            {"name":"local-native-fixture.bin","size":new.len()}
        ]
    });
    let engine = UpdateEngine::new(
        Some(common::trust()?),
        Identity::new(&receipt.version, Channel::Stable, target, common::VARIANT)?,
        Limits::default(),
    )?;
    let mut bytes = LocalBytes(VecDeque::from([signed, signature, new]));
    let checked = engine.check_release_json(
        &ReleaseSelector::exact_tag(&tag)?,
        &serde_json::to_vec(&release).map_err(|_| Error::Schema)?,
        &mut bytes,
        &clock,
    )?;
    let update = engine.download(checked, &mut bytes, &clock)?;
    let mut nonce = [0; 32];
    SystemRandom::new()
        .fill(&mut nonce)
        .map_err(|_| Error::Io)?;
    let mut installer = RunningInstaller::new(
        FileInstall::new(executable.clone(), hash(&old), nonce)?,
        ExitAcknowledgement::ExitImmediatelyAndRetainRecovery,
    );
    let probe = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0));
    let diagnostic = match &probe {
        Ok(_) => {
            json!({"phase":"before_queue","tcp_bind":true,"system_root_present":std::env::var_os("SystemRoot").is_some()})
        }
        Err(error) => {
            json!({"phase":"before_queue","tcp_bind":false,"raw_os_error":error.raw_os_error(),"kind":format!("{:?}",error.kind()),"system_root_present":std::env::var_os("SystemRoot").is_some()})
        }
    };
    fs::write(root.join(".fixture-context.json"), diagnostic.to_string()).map_err(|_| Error::Io)?;
    drop(probe);
    let pending = installer.queue(
        &update,
        InstallContext {
            custody: InstallCustody::Standalone,
            budget_ms: 30_000,
            allow_major: mode == "allow-major",
        },
        &clock,
        lifetime,
    )?;
    fs::write(
        root.join(".fixture-parent-ready.json"),
        serde_json::to_vec(&pending).map_err(|_| Error::Schema)?,
    )
    .map_err(|_| Error::Io)?;
    if mode == "cancel-ready" {
        lifetime.cancel()?;
        return Err(Error::Cancelled);
    }
    if mode == "late-error" {
        return Err(Error::Io);
    }
    if mode == "late-schema-error" {
        return Err(Error::Schema);
    }
    println!(
        "{}",
        serde_json::to_string(&pending).map_err(|_| Error::Schema)?
    );
    Ok(())
}

fn main() -> ExitCode {
    // This is the outermost frame. In particular run() errors/output failures do
    // not release the connection while this frame performs diagnostics/holding.
    let mut lifetime = ParentLifetime::new();
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    if arguments.len() != 3 {
        return ExitCode::FAILURE;
    }
    let hold = match arguments[1].parse::<u64>() {
        Ok(value) if value <= 35_000 => value,
        _ => return ExitCode::FAILURE,
    };
    let result = run(&mut lifetime, &arguments[0], &arguments[2]);
    if let Err(error) = &result {
        eprintln!("{error}");
    }
    if lifetime.is_pending() {
        std::thread::sleep(Duration::from_millis(hold));
    }
    if result.is_ok() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
