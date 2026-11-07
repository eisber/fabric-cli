// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! One-shot native macOS certificate helper. Never linked into the SDK process.

#![deny(unnameable_test_items)]

use fabric_identity::headless::certificate_macos::{
    MAC_HELPER_WIRE_LIMIT, MacCertificateError, MacCertificateSelector, MacHelperReply,
    MacHelperRequest,
};
use std::io::{Read, Write};

fn main() -> std::process::ExitCode {
    let arguments: Vec<_> = std::env::args_os().skip(1).take(2).collect();
    if arguments.len() == 1 && arguments[0] == "--help" {
        return write_bytes(
            b"fabric-keychain-helper: one schema-1 JSON request on stdin; no keychain or credential arguments.\n",
        );
    }
    if arguments.len() == 1 && arguments[0] == "--version" {
        return write_bytes(b"fabric-keychain-helper schema 1\n");
    }
    let result = if arguments.is_empty() {
        read_request().and_then(|(selector, input)| native_sign(&selector, &input))
    } else {
        Err(MacCertificateError::InvalidInput)
    };
    let reply = match result {
        Ok(reply) => reply,
        Err(code) => MacHelperReply::Error { schema: 1, code },
    };
    let encoded = reply.encode().unwrap_or_else(|_| {
        // A fixed response remains finite even when public DER would exceed
        // the aggregate wire limit after base64 encoding.
        br#"{"status":"error","schema":1,"code":"helper"}"#.to_vec()
    });
    write_bytes(&encoded)
}

fn write_bytes(bytes: &[u8]) -> std::process::ExitCode {
    match std::io::stdout().lock().write_all(bytes) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(_) => std::process::ExitCode::FAILURE,
    }
}

fn read_request() -> Result<(MacCertificateSelector, Vec<u8>), MacCertificateError> {
    let mut bytes = Vec::new();
    std::io::stdin()
        .lock()
        .take((MAC_HELPER_WIRE_LIMIT + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| MacCertificateError::InvalidInput)?;
    MacHelperRequest::decode(&bytes)
}

#[cfg(not(target_os = "macos"))]
fn native_sign(
    _: &MacCertificateSelector,
    _: &[u8],
) -> Result<MacHelperReply, MacCertificateError> {
    Err(MacCertificateError::UnsupportedPlatform)
}

#[cfg(target_os = "macos")]
fn native_sign(
    selector: &MacCertificateSelector,
    input: &[u8],
) -> Result<MacHelperReply, MacCertificateError> {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use fabric_identity::headless::certificate_macos::{
        mac_certificate_rsa_bits, validate_mac_key_policy, verify_mac_public_signature,
    };
    use security_framework::{
        item::{ItemClass, ItemSearchOptions, Limit, Reference, SearchResult},
        key::Algorithm,
        os::macos::keychain::SecKeychain,
    };

    // Security.framework's switch is process-global. This is intentionally in
    // the one-shot executable, not any library function callable by an SDK host.
    // The guard is established only after complete input validation and outlives
    // every native keychain/key handle created below.
    let _no_ui = SecKeychain::disable_user_interaction()
        .map_err(|_| MacCertificateError::InteractionRequired)?;
    let keychain = SecKeychain::open(selector.keychain_path())
        .map_err(|error| native_error(i64::from(error.code())))?;
    let results = ItemSearchOptions::new()
        .keychains(&[keychain])
        .class(ItemClass::identity())
        .load_refs(true)
        .skip_authenticated_items(true)
        .limit(Limit::Max(257))
        .search()
        .map_err(|error| native_error(i64::from(error.code())))?;
    if results.len() > 256 {
        return Err(MacCertificateError::SearchLimit);
    }
    let mut selected = None;
    for result in results {
        let SearchResult::Ref(Reference::Identity(identity)) = result else {
            return Err(MacCertificateError::Selection);
        };
        let certificate = identity
            .certificate()
            .map_err(|_| MacCertificateError::Selection)?
            .to_der();
        if certificate.len()
            > fabric_identity::headless::certificate_macos::MAC_CERTIFICATE_DER_LIMIT
        {
            return Err(MacCertificateError::SearchLimit);
        }
        if ring::digest::digest(&ring::digest::SHA256, &certificate).as_ref()
            == &selector.certificate_sha256()[..]
        {
            if selected.is_some() {
                return Err(MacCertificateError::Selection);
            }
            selected = Some((identity, certificate));
        }
    }
    let (identity, certificate) = selected.ok_or(MacCertificateError::Selection)?;
    let _bits = mac_certificate_rsa_bits(&certificate, &selector.certificate_sha256())?;
    // SecIdentityCopyPrivateKey (via this safe method) establishes the private
    // key class and keeps acquisition bound to this exact held identity.
    let key = identity
        .private_key()
        .map_err(|error| native_error(i64::from(error.code())))?;
    let attributes = key.attributes();
    validate_mac_key_policy(
        native_boolean(&attributes, "extr"),
        native_boolean(&attributes, "sign"),
    )?;
    // The MESSAGE algorithm hashes the original JWS bytes internally. Supplying
    // a digest here would double-hash and is deliberately not supported.
    // The selected certificate is RSA-2048..4096; native PSS signing followed
    // by independent certificate verification proves the key type/association.
    let signature = key
        .create_signature(Algorithm::RSASignatureMessagePSSSHA256, input)
        .map_err(|error| native_error(error.code() as i64))?;
    verify_mac_public_signature(
        &certificate,
        &selector.certificate_sha256(),
        input,
        &signature,
    )?;
    Ok(MacHelperReply::Ok {
        schema: 1,
        certificate: URL_SAFE_NO_PAD.encode(certificate),
        signature: URL_SAFE_NO_PAD.encode(signature),
    })
}

#[cfg(target_os = "macos")]
fn native_error(code: i64) -> MacCertificateError {
    match code {
        -25300 => MacCertificateError::Selection,
        -25308 | -25293 => MacCertificateError::InteractionRequired,
        -50 => MacCertificateError::UnsupportedKey,
        _ => MacCertificateError::Helper,
    }
}

#[cfg(target_os = "macos")]
fn native_boolean(
    attributes: &core_foundation::dictionary::CFDictionary,
    name: &'static str,
) -> Option<bool> {
    use core_foundation::{base::TCFType, boolean::CFBoolean, string::CFString};

    let key = CFString::from_static_string(name);
    let value = *attributes.find(key.as_CFTypeRef())?;
    // CFBoolean has exactly two public singleton instances. Pointer equality
    // against these typed instances establishes both type and truth value with
    // the untyped dictionary's safe API: no raw-pointer wrapping, casts, string
    // coercion, unavailable get_as_CFType API or new unsafe boundary is needed.
    if value == CFBoolean::false_value().as_CFTypeRef() {
        Some(false)
    } else if value == CFBoolean::true_value().as_CFTypeRef() {
        Some(true)
    } else {
        None
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use core_foundation::{
        base::TCFType, boolean::CFBoolean, dictionary::CFDictionary, number::CFNumber,
        string::CFString,
    };

    #[test]
    fn policy_values_must_be_actual_cfboolean_singletons() {
        let extr = CFString::from_static_string("extr");
        let sign = CFString::from_static_string("sign");
        let valid = CFDictionary::from_CFType_pairs(&[
            (extr.as_CFType(), CFBoolean::false_value().as_CFType()),
            (sign.as_CFType(), CFBoolean::true_value().as_CFType()),
        ])
        .to_untyped();
        assert_eq!(native_boolean(&valid, "extr"), Some(false));
        assert_eq!(native_boolean(&valid, "sign"), Some(true));
        assert_eq!(native_boolean(&valid, "expr"), None);
        let wrong_types = CFDictionary::from_CFType_pairs(&[
            (
                extr.as_CFType(),
                CFString::from_static_string("false").as_CFType(),
            ),
            (sign.as_CFType(), CFNumber::from(1_i32).as_CFType()),
        ])
        .to_untyped();
        assert_eq!(native_boolean(&wrong_types, "extr"), None);
        assert_eq!(native_boolean(&wrong_types, "sign"), None);
    }
}
