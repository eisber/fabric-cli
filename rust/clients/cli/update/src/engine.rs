// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use serde::Deserialize;

use crate::{
    Control, Error, Identity, InstallCustody, Limits, ReleaseSelector, Trust, VerifiedManifest,
    model::{check_control, tag_version, validate_leaf},
    protocol::{VerificationContext, sha256, verify_manifest},
};

/// One anonymous, GET-only public GitHub request.
///
/// The transport must disable its own redirects/retries, ambient authentication,
/// cookies, decompression, and credentials. It must enforce the deadline while
/// connecting/reading, stream into the supplied bounded sink, and report each
/// redirect to the engine. A blocked transport cannot be forcibly interrupted by
/// this synchronous engine; an adapter that ignores this contract is unsuitable.
#[derive(Clone, Debug)]
pub struct HttpRequest {
    /// Exact engine-generated or allowlisted HTTPS URL.
    pub url: String,
    /// Overall monotonic operation deadline in milliseconds.
    pub deadline_ms: u64,
    /// Maximum response bytes. No unbounded read-ahead is permitted.
    pub max_bytes: usize,
    /// Fixed GitHub media type.
    pub accept: &'static str,
    /// Fixed public API version.
    pub api_version: &'static str,
}

/// Response metadata; the transport does not follow `location`.
#[derive(Clone, Debug)]
pub struct HttpResponse {
    /// HTTP status code.
    pub status: u16,
    /// The exact response Location, if any.
    pub location: Option<String>,
}

/// Streaming output boundary that checks cancellation, deadline and byte bounds.
pub trait ResponseSink {
    /// Append one bounded chunk. Stop the request immediately on failure.
    fn write(&mut self, chunk: &[u8]) -> Result<(), Error>;
}

/// Caller-supplied real/fake HTTP transport; no SDK or HTTP runtime is linked here.
pub trait HttpTransport {
    /// Execute exactly one anonymous request without implicit redirects or retry.
    fn get(
        &mut self,
        request: &HttpRequest,
        sink: &mut dyn ResponseSink,
        control: &dyn Control,
    ) -> Result<HttpResponse, Error>;
}

/// A checked candidate, not downloaded or installed.
#[derive(Debug)]
pub struct CheckedUpdate {
    verified: VerifiedManifest,
    manifest_bytes: Vec<u8>,
    signature: Vec<u8>,
    asset_url: String,
    start_ms: u64,
    deadline_ms: u64,
    binding: EngineBinding,
    requires_major_opt_in: bool,
}

impl CheckedUpdate {
    /// Inspect the verified release candidate.
    pub fn manifest(&self) -> &VerifiedManifest {
        &self.verified
    }

    /// Whether installation crosses the caller's current major version.
    pub fn requires_major_opt_in(&self) -> bool {
        self.requires_major_opt_in
    }
}

/// Verified binary bytes in bounded memory, not an installed update.
///
/// This type is intentionally not cloneable. Installation consumes it. The
/// caller must refresh its running version identity after a successful install.
#[derive(Debug)]
pub struct DownloadedUpdate {
    verified: VerifiedManifest,
    pub(crate) manifest_bytes: Vec<u8>,
    pub(crate) signature: Vec<u8>,
    bytes: Vec<u8>,
    binding: EngineBinding,
    requires_major_opt_in: bool,
}

impl DownloadedUpdate {
    /// Exact signed metadata and detached signature for independent verification.
    pub fn signed_metadata(&self) -> (&[u8], &[u8]) {
        (&self.manifest_bytes, &self.signature)
    }

    #[cfg(feature = "bounded-helper")]
    pub(crate) fn installed_version(&self) -> &str {
        &self.binding.current
    }
    /// Inspect the signed manifest.
    pub fn manifest(&self) -> &VerifiedManifest {
        &self.verified
    }

    /// Access already size/hash-verified raw bytes without executing them.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Whether installation needs explicit major-upgrade permission.
    pub fn requires_major_opt_in(&self) -> bool {
        self.requires_major_opt_in
    }

    pub(crate) fn require_major_policy(&self, allow_major: bool) -> Result<(), Error> {
        if self.requires_major_opt_in && !allow_major {
            Err(Error::MajorUpgradeRequiresOptIn)
        } else {
            Ok(())
        }
    }
}

/// Explicit install authorization and separate finite installation deadline.
#[derive(Clone, Copy, Debug)]
pub struct InstallContext {
    /// The host-established install owner.
    pub custody: InstallCustody,
    /// Maximum installation time in milliseconds, at most 120 seconds.
    pub budget_ms: u64,
    /// Explicit apply permission to cross a major version.
    ///
    /// Automatic installation must always set this to false. Permission changes
    /// only the major boundary, not signature, custody, target or variant checks.
    pub allow_major: bool,
}

/// A successful install is separate from checking and downloading.
#[derive(Debug)]
pub struct InstallReceipt<R> {
    /// The installed signed semantic version.
    pub version: String,
    /// Adapter-specific durable replacement and rollback evidence.
    pub adapter: R,
}

/// Caller-approved installation boundary, never called before integrity checks.
pub trait Installer {
    /// Adapter-specific replacement evidence, not merely download evidence.
    type Receipt;

    /// Stage and replace without elevation, PATH editing, or running new bytes.
    ///
    /// Failure must not be reported as installation. Check `control` and deadline
    /// at mutation boundaries. Implementations must enforce standalone custody,
    /// exclusive installation ownership, bounded staging and rollback retention.
    fn install(
        &mut self,
        update: &DownloadedUpdate,
        context: InstallContext,
        control: &dyn Control,
    ) -> Result<Self::Receipt, Error>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct EngineBinding {
    repository: String,
    key: [u8; 32],
    current: String,
    target: String,
    variant: String,
    channel: crate::Channel,
}

/// Stateless composition of discovery, pinned verification, download and install.
///
/// Construction performs no I/O. There is one release candidate, no hidden
/// retries, no cache fallback, and no default publisher or automatic scheduling.
pub struct UpdateEngine {
    trust: Trust,
    identity: Identity,
    limits: Limits,
}

impl UpdateEngine {
    /// Require explicit trust; a build without publisher configuration fails shut.
    pub fn new(trust: Option<Trust>, identity: Identity, limits: Limits) -> Result<Self, Error> {
        let limits = limits.validate()?;
        Ok(Self {
            trust: trust.ok_or(Error::TrustRequired)?,
            identity,
            limits,
        })
    }

    /// Discover one public release and verify its detached signed manifest.
    pub fn check(
        &self,
        selector: &ReleaseSelector,
        transport: &mut dyn HttpTransport,
        control: &dyn Control,
    ) -> Result<CheckedUpdate, Error> {
        let (start, deadline) = self.budget(control)?;
        let suffix = match selector {
            ReleaseSelector::LatestStable => {
                if self.identity.channel != crate::Channel::Stable {
                    return Err(Error::Ineligible);
                }
                "latest".to_owned()
            }
            ReleaseSelector::LatestRustStable => {
                if self.identity.channel != crate::Channel::Stable {
                    return Err(Error::Ineligible);
                }
                format!("?per_page={}", self.limits.release_candidates)
            }
            ReleaseSelector::ExactTag(tag) => {
                tag_version(tag)?;
                format!("tags/{}", crate::model::encode_leaf(tag))
            }
        };
        let url = format!(
            "{}/releases{}{suffix}",
            self.trust.repository.api_base(),
            if matches!(selector, ReleaseSelector::LatestRustStable) {
                ""
            } else {
                "/"
            }
        );
        let json = self.fetch(
            url,
            self.limits.release_bytes,
            false,
            (start, deadline),
            transport,
            control,
        )?;
        self.select_json(selector, &json, (start, deadline), transport, control)
    }

    /// Verify caller-provided bounded GitHub release JSON, then fetch its manifest.
    ///
    /// The JSON still must bind its API URL and HTML URL to the pinned repository.
    /// Supplied asset download URLs are not used.
    pub fn check_release_json(
        &self,
        selector: &ReleaseSelector,
        json: &[u8],
        transport: &mut dyn HttpTransport,
        control: &dyn Control,
    ) -> Result<CheckedUpdate, Error> {
        self.select_json(selector, json, self.budget(control)?, transport, control)
    }

    /// Download only the exact signed raw asset and verify byte count and SHA-256.
    ///
    /// The overall network deadline starts at check, not anew for each download.
    pub fn download(
        &self,
        checked: CheckedUpdate,
        transport: &mut dyn HttpTransport,
        control: &dyn Control,
    ) -> Result<DownloadedUpdate, Error> {
        if checked.binding != self.binding() {
            return Err(Error::Binding);
        }
        self.ensure_fresh(&checked.verified, control)?;
        let bytes = self.fetch(
            checked.asset_url,
            checked.verified.manifest.bytes as usize,
            true,
            (checked.start_ms, checked.deadline_ms),
            transport,
            control,
        )?;
        if bytes.len() as u64 != checked.verified.manifest.bytes
            || sha256(&bytes) != checked.verified.digest
        {
            return Err(Error::Integrity);
        }
        crate::protocol::validate_binary(&bytes, self.identity.target)?;
        self.ensure_fresh(&checked.verified, control)?;
        Ok(DownloadedUpdate {
            verified: checked.verified,
            manifest_bytes: checked.manifest_bytes,
            signature: checked.signature,
            bytes,
            binding: checked.binding,
            requires_major_opt_in: checked.requires_major_opt_in,
        })
    }

    /// Consume verified bytes and call a real caller-approved install adapter once.
    pub fn install<I: Installer>(
        &self,
        downloaded: DownloadedUpdate,
        context: InstallContext,
        installer: &mut I,
        control: &dyn Control,
    ) -> Result<InstallReceipt<I::Receipt>, Error> {
        if context.custody != InstallCustody::Standalone {
            return Err(Error::Custody);
        }
        if context.budget_ms == 0 || context.budget_ms > 120_000 {
            return Err(Error::InvalidLimits);
        }
        if downloaded.binding != self.binding() {
            return Err(Error::Binding);
        }
        downloaded.require_major_policy(context.allow_major)?;
        self.ensure_fresh(&downloaded.verified, control)?;
        let now = control.snapshot();
        if now.cancelled {
            return Err(Error::Cancelled);
        }
        let adapter = installer.install(&downloaded, context, control)?;
        // Do not turn a committed replacement into "not installed" if a signal
        // arrives after the adapter's linearization point.
        Ok(InstallReceipt {
            version: downloaded.verified.manifest.version.clone(),
            adapter,
        })
    }

    fn select_json(
        &self,
        selector: &ReleaseSelector,
        json: &[u8],
        budget: (u64, u64),
        transport: &mut dyn HttpTransport,
        control: &dyn Control,
    ) -> Result<CheckedUpdate, Error> {
        if !matches!(selector, ReleaseSelector::LatestRustStable) {
            return self.check_json(selector, json, budget, transport, control);
        }
        check_control(control, budget.0, budget.1)?;
        if self.identity.channel != crate::Channel::Stable {
            return Err(Error::Ineligible);
        }
        if json.is_empty() || json.len() > self.limits.release_bytes {
            return Err(Error::Limit);
        }
        let releases: Vec<Release> = serde_json::from_slice(json).map_err(|_| Error::Schema)?;
        if releases.len() > self.limits.release_candidates {
            return Err(Error::Limit);
        }
        let mut selected: Option<(semver::Version, Release)> = None;
        let mut seen: Vec<semver::Version> = Vec::new();
        for release in releases {
            if !release.tag_name.starts_with("rust-v") || release.draft || release.prerelease {
                continue;
            }
            let version = tag_version(&release.tag_name)?;
            if !version.pre.is_empty() {
                return Err(Error::Ineligible);
            }
            if seen
                .iter()
                .any(|previous| version.cmp_precedence(previous).is_eq())
            {
                return Err(Error::Schema);
            }
            seen.push(version.clone());
            if release.id == 0
                || release.url
                    != format!(
                        "{}/releases/{}",
                        self.trust.repository.api_base(),
                        release.id
                    )
                || release.html_url != self.trust.repository.release_page(&release.tag_name)
            {
                return Err(Error::Origin);
            }
            if selected
                .as_ref()
                .is_none_or(|(previous, _)| version.cmp_precedence(previous).is_gt())
            {
                selected = Some((version, release));
            }
        }
        let (_, selected) = selected.ok_or(Error::ReleaseUnavailable)?;
        let json = serde_json::to_vec(&selected).map_err(|_| Error::Schema)?;
        self.check_json(
            &ReleaseSelector::ExactTag(selected.tag_name),
            &json,
            budget,
            transport,
            control,
        )
    }

    fn check_json(
        &self,
        selector: &ReleaseSelector,
        json: &[u8],
        budget: (u64, u64),
        transport: &mut dyn HttpTransport,
        control: &dyn Control,
    ) -> Result<CheckedUpdate, Error> {
        check_control(control, budget.0, budget.1)?;
        if json.is_empty() || json.len() > self.limits.release_bytes {
            return Err(Error::Limit);
        }
        let release: Release = serde_json::from_slice(json).map_err(|_| Error::Schema)?;
        if release.assets.len() > self.limits.release_assets {
            return Err(Error::Limit);
        }
        let version = tag_version(&release.tag_name)?;
        if release.draft
            || !version.cmp_precedence(&self.identity.current).is_gt()
            || release.prerelease != !version.pre.is_empty()
            || release.prerelease != (self.identity.channel == crate::Channel::Preview)
        {
            return Err(Error::Ineligible);
        }
        match selector {
            ReleaseSelector::ExactTag(tag) if tag != &release.tag_name => {
                return Err(Error::Binding);
            }
            ReleaseSelector::LatestStable if self.identity.channel != crate::Channel::Stable => {
                return Err(Error::Ineligible);
            }
            _ => {}
        }
        if release.id == 0
            || release.url
                != format!(
                    "{}/releases/{}",
                    self.trust.repository.api_base(),
                    release.id
                )
            || release.html_url != self.trust.repository.release_page(&release.tag_name)
        {
            return Err(Error::Origin);
        }
        for (index, asset) in release.assets.iter().enumerate() {
            validate_leaf(&asset.name)?;
            if release.assets[..index].iter().any(|a| a.name == asset.name) {
                return Err(Error::Schema);
            }
        }
        let manifest_name = format!(
            "update-{}-{}.json",
            self.identity.target.as_str(),
            self.identity.variant
        );
        validate_leaf(&manifest_name)?;
        let signature_name = format!("{manifest_name}.sig");
        validate_leaf(&signature_name)?;
        let manifest_asset = release.asset(&manifest_name)?;
        let signature_asset = release.asset(&signature_name)?;
        if manifest_asset.size == 0
            || manifest_asset.size > self.limits.manifest_bytes as u64
            || signature_asset.size != 64
        {
            return Err(Error::Limit);
        }
        let manifest = self.fetch(
            self.trust
                .repository
                .asset_url(&release.tag_name, &manifest_name),
            manifest_asset.size as usize,
            true,
            budget,
            transport,
            control,
        )?;
        if manifest.len() as u64 != manifest_asset.size {
            return Err(Error::Integrity);
        }
        let signature = self.fetch(
            self.trust
                .repository
                .asset_url(&release.tag_name, &signature_name),
            64,
            true,
            budget,
            transport,
            control,
        )?;
        let now = check_control(control, budget.0, budget.1)?;
        let verified = verify_manifest(
            &manifest,
            &signature,
            VerificationContext {
                trust: &self.trust,
                identity: &self.identity,
                tag: &release.tag_name,
                prerelease: release.prerelease,
                now: now.unix_seconds,
                limits: self.limits,
            },
        )?;
        if release.asset(&verified.manifest.asset)?.size != verified.manifest.bytes {
            return Err(Error::Binding);
        }
        let asset_url = self
            .trust
            .repository
            .asset_url(&release.tag_name, &verified.manifest.asset);
        Ok(CheckedUpdate {
            verified,
            manifest_bytes: manifest,
            signature,
            asset_url,
            start_ms: budget.0,
            deadline_ms: budget.1,
            binding: self.binding(),
            requires_major_opt_in: version.major != self.identity.current.major,
        })
    }

    fn budget(&self, control: &dyn Control) -> Result<(u64, u64), Error> {
        let start = control.snapshot();
        if start.cancelled {
            return Err(Error::Cancelled);
        }
        let deadline = start
            .monotonic_ms
            .checked_add(self.limits.network_ms)
            .ok_or(Error::Deadline)?;
        Ok((start.monotonic_ms, deadline))
    }

    fn ensure_fresh(
        &self,
        manifest: &VerifiedManifest,
        control: &dyn Control,
    ) -> Result<(), Error> {
        let now = control.snapshot();
        if now.cancelled {
            return Err(Error::Cancelled);
        }
        if manifest.manifest.expires_at <= now.unix_seconds
            || manifest
                .manifest
                .expires_at
                .saturating_sub(now.unix_seconds)
                > self.limits.validity_seconds
        {
            return Err(Error::Expired);
        }
        Ok(())
    }

    fn binding(&self) -> EngineBinding {
        EngineBinding {
            repository: self.trust.repository.full_name(),
            key: self.trust.key,
            current: self.identity.current.to_string(),
            target: self.identity.target.as_str().to_owned(),
            variant: self.identity.variant.clone(),
            channel: self.identity.channel,
        }
    }

    fn fetch(
        &self,
        mut url: String,
        max_bytes: usize,
        asset: bool,
        budget: (u64, u64),
        transport: &mut dyn HttpTransport,
        control: &dyn Control,
    ) -> Result<Vec<u8>, Error> {
        for hop in 0..=self.limits.redirects {
            check_control(control, budget.0, budget.1)?;
            let request = HttpRequest {
                url: url.clone(),
                deadline_ms: budget.1,
                max_bytes,
                accept: if asset {
                    "application/octet-stream"
                } else {
                    "application/vnd.github+json"
                },
                api_version: "2022-11-28",
            };
            let mut sink = BoundedSink {
                bytes: Vec::new(),
                max: max_bytes,
                budget,
                control,
                failure: None,
            };
            let response = transport.get(&request, &mut sink, control)?;
            if let Some(error) = sink.failure {
                return Err(error);
            }
            check_control(control, budget.0, budget.1)?;
            if response.status == 200 && response.location.is_none() {
                return Ok(sink.bytes);
            }
            if response.status == 404 {
                return Err(Error::ReleaseUnavailable);
            }
            if !matches!(response.status, 301 | 302 | 303 | 307 | 308) {
                return Err(Error::Response);
            }
            if !asset || hop == self.limits.redirects {
                return Err(Error::Origin);
            }
            let location = response.location.ok_or(Error::Origin)?;
            if !is_allowed_release_cdn(&location) {
                return Err(Error::Origin);
            }
            url = location;
        }
        Err(Error::Origin)
    }
}

struct BoundedSink<'a> {
    bytes: Vec<u8>,
    max: usize,
    budget: (u64, u64),
    control: &'a dyn Control,
    failure: Option<Error>,
}

impl ResponseSink for BoundedSink<'_> {
    fn write(&mut self, chunk: &[u8]) -> Result<(), Error> {
        let result = check_control(self.control, self.budget.0, self.budget.1).and_then(|_| {
            if self.bytes.len().saturating_add(chunk.len()) > self.max {
                Err(Error::Limit)
            } else {
                self.bytes.extend_from_slice(chunk);
                Ok(())
            }
        });
        if let Err(ref error) = result {
            self.failure = Some(match error {
                Error::Cancelled => Error::Cancelled,
                Error::Deadline => Error::Deadline,
                _ => Error::Limit,
            });
        }
        result
    }
}

/// Validate only the bounded anonymous GitHub release-CDN redirect allowlist.
///
/// This grants no authorization, signature trust, or permission to forward
/// credentials. Every request, including an allowed CDN, must remain anonymous.
pub fn is_allowed_release_cdn(url: &str) -> bool {
    if url.len() > 4096
        || !url.is_ascii()
        || url
            .bytes()
            .any(|c| c.is_ascii_control() || c.is_ascii_whitespace() || c == b'\\' || c == b'#')
    {
        return false;
    }
    [
        "https://release-assets.githubusercontent.com/",
        "https://objects.githubusercontent.com/",
        "https://github-releases.githubusercontent.com/",
    ]
    .iter()
    .any(|prefix| {
        url.strip_prefix(prefix)
            .is_some_and(|path| !path.is_empty())
    })
}

#[derive(Deserialize, serde::Serialize)]
struct Release {
    id: u64,
    url: String,
    html_url: String,
    tag_name: String,
    draft: bool,
    prerelease: bool,
    assets: Vec<ReleaseAsset>,
}

impl Release {
    fn asset(&self, name: &str) -> Result<&ReleaseAsset, Error> {
        self.assets
            .iter()
            .find(|asset| asset.name == name)
            .ok_or(Error::ReleaseUnavailable)
    }
}

#[derive(Deserialize, serde::Serialize)]
struct ReleaseAsset {
    name: String,
    size: u64,
}
