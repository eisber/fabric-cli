// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use ring::{digest, signature};
use serde::{Deserialize, Serialize};

use crate::{
    Channel, Error, Identity, Limits, Target, Trust,
    model::{tag_version, validate_leaf},
};

/// Version-one manifest: exact UTF-8 JSON bytes are signed, not canonicalized JSON.
///
/// Every field is required. Unknown and duplicate fields are rejected. The
/// detached signature is exactly 64 raw Ed25519 bytes, not base64 or a key bundle.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// Update wire protocol; currently exactly `1`.
    pub protocol: u32,
    /// Exact native product identifier, currently `fab-rs`.
    pub product: String,
    /// Exact `owner/name` of the pinned public GitHub repository.
    pub repository: String,
    /// Exact GitHub tag, including any leading `v`.
    pub tag: String,
    /// Canonical semantic version without the optional tag prefix.
    pub version: String,
    /// Exact release channel.
    pub channel: Channel,
    /// Exact native target triple.
    pub target: String,
    /// Exact publisher-defined feature identity.
    pub variant: String,
    /// Validated leaf name of one raw executable asset.
    pub asset: String,
    /// Exact asset byte count; zero is forbidden.
    pub bytes: u64,
    /// Lowercase 64-character hexadecimal SHA-256 digest.
    pub sha256: String,
    /// Exclusive expiry, UTC Unix seconds.
    pub expires_at: u64,
}

/// A signature-verified, identity-bound manifest. Only the verifier constructs it.
#[derive(Clone, Debug)]
pub struct VerifiedManifest {
    pub(crate) manifest: Manifest,
    pub(crate) digest: [u8; 32],
}

impl VerifiedManifest {
    /// Read the verified signed fields.
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }
}

/// Verify bounded exact bytes **before** parsing JSON, then bind every identity.
///
/// `tag` and `prerelease` must come from an already origin-bound GitHub release.
/// The key must be pinned independently of that release. This function performs
/// no I/O and does not authorize an install.
pub fn verify_manifest(
    bytes: &[u8],
    signature_bytes: &[u8],
    context: VerificationContext<'_>,
) -> Result<VerifiedManifest, Error> {
    let VerificationContext {
        trust,
        identity,
        tag,
        prerelease,
        now,
        limits,
    } = context;
    let limits = limits.validate()?;
    if bytes.is_empty() || bytes.len() > limits.manifest_bytes {
        return Err(Error::Limit);
    }
    if signature_bytes.len() != 64 {
        return Err(Error::Signature);
    }
    signature::UnparsedPublicKey::new(&signature::ED25519, trust.key)
        .verify(bytes, signature_bytes)
        .map_err(|_| Error::Signature)?;
    let manifest: Manifest = serde_json::from_slice(bytes).map_err(|_| Error::Schema)?;
    let version = tag_version(tag)?;
    let manifest_version = semver::Version::parse(&manifest.version).map_err(|_| Error::Schema)?;
    validate_leaf(&manifest.asset).map_err(|_| Error::Schema)?;
    if manifest.protocol != 1
        || manifest.product != "fab-rs"
        || manifest.repository != trust.repository.full_name()
        || manifest.tag != tag
        || manifest_version.to_string() != manifest.version
        || manifest_version != version
        || manifest.channel != identity.channel
        || manifest.target != identity.target.as_str()
        || manifest.variant != identity.variant
    {
        return Err(Error::Binding);
    }
    if !version.cmp_precedence(&identity.current).is_gt()
        || prerelease != !version.pre.is_empty()
        || prerelease != (identity.channel == Channel::Preview)
    {
        return Err(Error::Ineligible);
    }
    if manifest.expires_at <= now
        || manifest.expires_at.saturating_sub(now) > limits.validity_seconds
    {
        return Err(Error::Expired);
    }
    if manifest.bytes == 0 || manifest.bytes > limits.asset_bytes as u64 {
        return Err(Error::Limit);
    }
    let digest = decode_digest(&manifest.sha256)?;
    Ok(VerifiedManifest { manifest, digest })
}

/// All caller/release bindings required for detached signature verification.
#[derive(Clone, Copy)]
pub struct VerificationContext<'a> {
    /// Out-of-band publisher repository/key.
    pub trust: &'a Trust,
    /// Current exact version, target, channel and feature identity.
    pub identity: &'a Identity,
    /// Exact tag from origin-validated release metadata.
    pub tag: &'a str,
    /// GitHub's release prerelease flag.
    pub prerelease: bool,
    /// Current caller-provided UTC Unix seconds.
    pub now: u64,
    /// Explicit work and validity caps.
    pub limits: Limits,
}

pub(crate) fn decode_digest(value: &str) -> Result<[u8; 32], Error> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    {
        return Err(Error::Schema);
    }
    let mut result = [0; 32];
    for (index, pair) in value.as_bytes().as_chunks::<2>().0.iter().enumerate() {
        let digit = |c: u8| if c <= b'9' { c - b'0' } else { c - b'a' + 10 };
        result[index] = digit(pair[0]) * 16 + digit(pair[1]);
    }
    Ok(result)
}

pub(crate) fn sha256(bytes: &[u8]) -> [u8; 32] {
    let digest = digest::digest(&digest::SHA256, bytes);
    let mut result = [0; 32];
    result.copy_from_slice(digest.as_ref());
    result
}

pub(crate) fn validate_binary(bytes: &[u8], target: Target) -> Result<(), Error> {
    let word = |offset: usize| {
        bytes
            .get(offset..offset + 2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
    };
    let dword = |offset: usize| {
        bytes
            .get(offset..offset + 4)
            .map(|v| u32::from_le_bytes([v[0], v[1], v[2], v[3]]))
    };
    let valid = match target {
        Target::WindowsX64 | Target::WindowsArm64 => {
            let pe = dword(0x3c).map(|offset| offset as usize);
            bytes.starts_with(b"MZ")
                && pe.is_some_and(|offset| {
                    offset >= 64
                        && bytes.get(offset..offset.saturating_add(4)) == Some(b"PE\0\0")
                        && word(offset + 4)
                            == Some(if target == Target::WindowsX64 {
                                0x8664
                            } else {
                                0xaa64
                            })
                        && word(offset + 22)
                            .is_some_and(|flags| flags & 0x0002 != 0 && flags & 0x2000 == 0)
                        && word(offset + 24) == Some(0x020b)
                })
        }
        Target::LinuxX64 | Target::LinuxArm64 => {
            bytes.len() >= 64
                && bytes.starts_with(b"\x7fELF\x02\x01\x01")
                && matches!(word(16), Some(2 | 3))
                && word(18) == Some(if target == Target::LinuxX64 { 62 } else { 183 })
        }
        Target::MacX64 | Target::MacArm64 => {
            bytes.len() >= 32
                && bytes.starts_with(b"\xcf\xfa\xed\xfe")
                && dword(4)
                    == Some(if target == Target::MacX64 {
                        0x0100_0007
                    } else {
                        0x0100_000c
                    })
                && dword(12) == Some(2)
        }
    };
    if valid {
        Ok(())
    } else {
        Err(Error::BinaryFormat)
    }
}
