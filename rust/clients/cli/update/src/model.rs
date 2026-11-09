// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use semver::Version;
use serde::{Deserialize, Serialize};

/// Typed failures never include downloaded content, credentials, or supplied paths.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Error {
    /// Publisher configuration or pinned key is absent.
    #[error("trusted publisher configuration is required")]
    TrustRequired,
    /// A repository, tag, asset name, or identity is invalid.
    #[error("invalid update identity")]
    InvalidIdentity,
    /// The current platform is not an explicitly supported native target.
    #[error("unsupported update target")]
    UnsupportedTarget,
    /// Caller limits are zero, excessive, or inconsistent.
    #[error("invalid update limits")]
    InvalidLimits,
    /// A response exceeds its bounded byte count or item count.
    #[error("update input exceeds its limit")]
    Limit,
    /// The request deadline has elapsed or the monotonic clock moved backwards.
    #[error("update deadline elapsed or clock moved backwards")]
    Deadline,
    /// The caller cancelled this operation.
    #[error("update cancelled")]
    Cancelled,
    /// The transport failed. Provider error text is deliberately not retained.
    #[error("update transport failed")]
    Transport,
    /// The response was not a successful GitHub response.
    #[error("unexpected update response")]
    Response,
    /// An origin, redirect, or release metadata binding is invalid.
    #[error("untrusted update origin")]
    Origin,
    /// A manifest signature is missing, malformed, or invalid.
    #[error("invalid update signature")]
    Signature,
    /// The signed manifest does not satisfy the exact schema.
    #[error("invalid update manifest schema")]
    Schema,
    /// A signed field does not match the caller's requested release identity.
    #[error("update identity does not match")]
    Binding,
    /// The release is draft, on another channel, or not strictly newer.
    #[error("release is not an eligible upgrade")]
    Ineligible,
    /// Crossing the current major version requires an explicit apply opt-in.
    #[error("a major upgrade requires explicit installation opt-in")]
    MajorUpgradeRequiresOptIn,
    /// No Rust release or its required signed update metadata is published.
    #[error("trusted Rust release metadata is not available")]
    ReleaseUnavailable,
    /// The signed timestamp is expired or unreasonably far in the future.
    #[error("update manifest is expired or outside its validity window")]
    Expired,
    /// Downloaded bytes do not match the signed length and digest.
    #[error("update asset integrity check failed")]
    Integrity,
    /// The signed raw asset is not a native executable for the selected target.
    #[error("update asset is not a matching native executable")]
    BinaryFormat,
    /// The package manager, unknown custodian, or another owner controls this install.
    #[error("installation is not caller-owned standalone custody")]
    Custody,
    /// A lock exists or the acquired lock was lost.
    #[error("update installation lock is unavailable or lost")]
    Lock,
    /// A real filesystem operation failed.
    #[error("update filesystem operation failed")]
    Io,
    /// An installed file changed or a rollback/installation name is already in use.
    #[error("installation state changed or already exists")]
    InstallConflict,
    /// Running executable replacement is not implemented by this adapter.
    #[error("running executable replacement requires an approved platform adapter")]
    RunningExecutable,
    /// The install adapter failed without reporting completion.
    #[error("update installation failed")]
    Install,
    /// Native replacement may have moved files. Recovery evidence is retained.
    #[error("native replacement needs reconciliation; retain lock, stage, and rollback")]
    RecoveryRequired,
    /// Installed helper paths or bounded local IPC do not meet the safe boundary.
    #[error("native replacement helper boundary is unsupported")]
    NativeBoundary,
    /// Two prior verified images are retained; the owning installer must reconcile retention.
    #[error("rollback retention is full; the owning installer must reconcile verified backups")]
    Retention,
    /// Another bounded helper still holds the exact transaction lock.
    #[error("the update helper is still processing this transaction")]
    HelperPending,
}

/// A validated public GitHub repository, never a URL or arbitrary hostname.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Repository {
    owner: String,
    name: String,
}

impl Repository {
    /// Validate GitHub owner/repository names without consulting any environment.
    pub fn new(owner: &str, name: &str) -> Result<Self, Error> {
        let owner_valid = !owner.is_empty()
            && owner.len() <= 39
            && !owner.starts_with('-')
            && !owner.ends_with('-')
            && !owner.contains("--")
            && owner
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-');
        let name_valid = !name.is_empty()
            && name.len() <= 100
            && !matches!(name, "." | "..")
            && name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c));
        if !owner_valid || !name_valid {
            return Err(Error::InvalidIdentity);
        }
        Ok(Self {
            owner: owner.to_owned(),
            name: name.to_owned(),
        })
    }

    /// The pinned owner name.
    pub fn owner(&self) -> &str {
        &self.owner
    }

    /// The pinned repository name.
    pub fn name(&self) -> &str {
        &self.name
    }

    pub(crate) fn full_name(&self) -> String {
        format!("{}/{}", self.owner, self.name)
    }

    pub(crate) fn api_base(&self) -> String {
        format!("https://api.github.com/repos/{}", self.full_name())
    }

    pub(crate) fn release_page(&self, tag: &str) -> String {
        format!(
            "https://github.com/{}/releases/tag/{}",
            self.full_name(),
            encode_leaf(tag)
        )
    }

    pub(crate) fn asset_url(&self, tag: &str, asset: &str) -> String {
        format!(
            "https://github.com/{}/releases/download/{}/{}",
            self.full_name(),
            encode_leaf(tag),
            encode_leaf(asset)
        )
    }
}

/// The release channel is signed, not inferred from a URL or environment.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    /// Only stable semantic versions and non-prerelease GitHub releases.
    Stable,
    /// Only semantic prereleases marked as prereleases by GitHub.
    Preview,
}

/// Discovery endpoint selected by the caller.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReleaseSelector {
    /// GitHub's public latest stable release endpoint.
    LatestStable,
    /// Highest stable `rust-vSEMVER` in one bounded public releases page.
    LatestRustStable,
    /// A single exact release tag. The engine never enumerates release history.
    ExactTag(String),
}

/// Explicitly supported raw native executable targets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    /// Windows x86-64.
    WindowsX64,
    /// Windows ARM64.
    WindowsArm64,
    /// Linux x86-64 with glibc.
    LinuxX64,
    /// Linux ARM64 with glibc.
    LinuxArm64,
    /// macOS x86-64.
    MacX64,
    /// macOS ARM64.
    MacArm64,
}

impl Target {
    /// Reject unknown OS/ABI combinations rather than guessing a compatible asset.
    pub fn parse(value: &str) -> Result<Self, Error> {
        match value {
            "x86_64-pc-windows-msvc" => Ok(Self::WindowsX64),
            "aarch64-pc-windows-msvc" => Ok(Self::WindowsArm64),
            "x86_64-unknown-linux-gnu" => Ok(Self::LinuxX64),
            "aarch64-unknown-linux-gnu" => Ok(Self::LinuxArm64),
            "x86_64-apple-darwin" => Ok(Self::MacX64),
            "aarch64-apple-darwin" => Ok(Self::MacArm64),
            _ => Err(Error::UnsupportedTarget),
        }
    }

    /// The exact signed target triple.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::WindowsX64 => "x86_64-pc-windows-msvc",
            Self::WindowsArm64 => "aarch64-pc-windows-msvc",
            Self::LinuxX64 => "x86_64-unknown-linux-gnu",
            Self::LinuxArm64 => "aarch64-unknown-linux-gnu",
            Self::MacX64 => "x86_64-apple-darwin",
            Self::MacArm64 => "aarch64-apple-darwin",
        }
    }
}

/// Immutable caller identity, including a publisher-defined exact feature variant.
#[derive(Clone, Debug)]
pub struct Identity {
    pub(crate) current: Version,
    pub(crate) channel: Channel,
    pub(crate) target: Target,
    pub(crate) variant: String,
}

impl Identity {
    /// Construct a version/channel/target/variant identity without ambient detection.
    ///
    /// `variant` must identify the exact enabled feature set, not just an OS label.
    pub fn new(
        current: &str,
        channel: Channel,
        target: Target,
        variant: &str,
    ) -> Result<Self, Error> {
        let current = Version::parse(current).map_err(|_| Error::InvalidIdentity)?;
        validate_leaf(variant)?;
        if variant.len() > 64 || (channel == Channel::Stable) != current.pre.is_empty() {
            return Err(Error::InvalidIdentity);
        }
        Ok(Self {
            current,
            channel,
            target,
            variant: variant.to_owned(),
        })
    }
}

/// Build/caller-supplied pinned trust. No key is ever obtained from a release.
#[derive(Clone, Debug)]
pub struct Trust {
    pub(crate) repository: Repository,
    pub(crate) key: [u8; 32],
}

impl Trust {
    /// Require an explicit out-of-band repository and Ed25519 public key.
    ///
    /// Establishing the publisher's identity and key distribution is the caller's
    /// release-governance responsibility. This API does not endorse arbitrary keys.
    pub fn new(repository: Repository, public_key: Option<[u8; 32]>) -> Result<Self, Error> {
        let key = public_key.ok_or(Error::TrustRequired)?;
        if key == [0; 32] {
            return Err(Error::TrustRequired);
        }
        Ok(Self { repository, key })
    }
}

/// Installation custody supplied by the owning host, never guessed from PATH.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstallCustody {
    /// Explicitly owned standalone binary with permission to replace it.
    Standalone,
    /// A package manager owns installation and upgrade operations.
    PackageManager,
    /// Ownership could not be established.
    Unknown,
}

/// A single caller-owned snapshot. Monotonic time is in milliseconds.
#[derive(Clone, Copy, Debug)]
pub struct Snapshot {
    /// UTC Unix seconds, used only for signed validity.
    pub unix_seconds: u64,
    /// Monotonic elapsed milliseconds, used for operation budgets.
    pub monotonic_ms: u64,
    /// Explicit cancellation signal.
    pub cancelled: bool,
}

/// Clock and cancellation injection; implementations must not perform network I/O.
pub trait Control {
    /// Read current caller-controlled wall/monotonic clocks and cancellation.
    fn snapshot(&self) -> Snapshot;
}

/// Hard-bounded work limits. No retry or candidate enumeration is implicit.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Maximum GitHub release JSON bytes.
    pub release_bytes: usize,
    /// Maximum exact signed manifest bytes.
    pub manifest_bytes: usize,
    /// Maximum binary bytes in memory and on disk.
    pub asset_bytes: usize,
    /// Maximum asset entries accepted in one release (one release candidate only).
    pub release_assets: usize,
    /// Maximum releases inspected in one public API page, with no pagination.
    pub release_candidates: usize,
    /// Maximum redirects for each asset request.
    pub redirects: u8,
    /// Overall monotonic network/check/download budget in milliseconds.
    pub network_ms: u64,
    /// Maximum allowed remaining signed validity in seconds.
    pub validity_seconds: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            release_bytes: 64 * 1024,
            manifest_bytes: 16 * 1024,
            asset_bytes: 64 * 1024 * 1024,
            release_assets: 64,
            release_candidates: 20,
            redirects: 3,
            network_ms: 30_000,
            validity_seconds: 7 * 24 * 60 * 60,
        }
    }
}

impl Limits {
    pub(crate) fn validate(self) -> Result<Self, Error> {
        if self.release_bytes == 0
            || self.release_bytes > 1024 * 1024
            || self.manifest_bytes == 0
            || self.manifest_bytes > 64 * 1024
            || self.asset_bytes == 0
            || self.asset_bytes > 256 * 1024 * 1024
            || self.release_assets == 0
            || self.release_assets > 128
            || self.release_candidates == 0
            || self.release_candidates > 32
            || self.redirects > 5
            || self.network_ms == 0
            || self.network_ms > 120_000
            || self.validity_seconds == 0
            || self.validity_seconds > 31 * 24 * 60 * 60
        {
            Err(Error::InvalidLimits)
        } else {
            Ok(self)
        }
    }
}

pub(crate) fn validate_leaf(value: &str) -> Result<(), Error> {
    if value.is_empty()
        || value.len() > 160
        || value.starts_with('.')
        || value.contains("..")
        || !value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_.+".contains(&c))
    {
        return Err(Error::InvalidIdentity);
    }
    Ok(())
}

pub(crate) fn tag_version(tag: &str) -> Result<Version, Error> {
    validate_leaf(tag)?;
    Version::parse(tag.strip_prefix("rust-v").ok_or(Error::InvalidIdentity)?)
        .map_err(|_| Error::InvalidIdentity)
}

impl ReleaseSelector {
    /// Construct a validated exact Rust product release tag (`rust-vSEMVER`).
    pub fn exact_tag(tag: &str) -> Result<Self, Error> {
        tag_version(tag)?;
        Ok(Self::ExactTag(tag.to_owned()))
    }
}

pub(crate) fn encode_leaf(value: &str) -> String {
    // The leaf validator permits '+' for semantic build metadata.
    value.replace('+', "%2B")
}

pub(crate) fn check_control(
    control: &dyn Control,
    start: u64,
    deadline: u64,
) -> Result<Snapshot, Error> {
    let now = control.snapshot();
    if now.cancelled {
        Err(Error::Cancelled)
    } else if now.monotonic_ms < start || now.monotonic_ms >= deadline {
        Err(Error::Deadline)
    } else {
        Ok(now)
    }
}
