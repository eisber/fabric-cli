// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Private CNG bridge, mounted only beneath the existing native interop module.

use crate::certificate::{
    CertificateError, CertificateSelector, CertificateStage, CertificateStore, WorkOutcome,
    validate_key_policy,
};
use std::{mem::size_of, ptr, slice};
use windows::{
    Win32::{
        Foundation::{
            CRYPT_E_NOT_FOUND, ERROR_NO_MORE_FILES, GetLastError, NTE_SILENT_CONTEXT, NTSTATUS,
        },
        Security::{Cryptography::*, OBJECT_SECURITY_INFORMATION},
    },
    core::{BOOL, PCWSTR, w},
};

const CERTIFICATE_LIMIT: usize = 4096;
const CERTIFICATE_BYTES_LIMIT: usize = 64 * 1024;
const TOTAL_CERTIFICATE_BYTES_LIMIT: usize = 8 * 1024 * 1024;
const PROPERTY_BYTES_LIMIT: usize = 1024;

fn native_error(stage: CertificateStage, error: windows::core::Error) -> CertificateError {
    let code = error.code().0;
    if code == NTE_SILENT_CONTEXT.0 {
        CertificateError::InteractionRequired
    } else {
        CertificateError::Native { stage, code }
    }
}

fn status_result(status: NTSTATUS, stage: CertificateStage) -> Result<(), CertificateError> {
    if status.0 >= 0 {
        Ok(())
    } else {
        Err(CertificateError::Native {
            stage,
            code: status.0,
        })
    }
}

fn check_cancel(cancelled: &dyn Fn() -> bool) -> Result<(), CertificateError> {
    if cancelled() {
        Err(CertificateError::Cancelled)
    } else {
        Ok(())
    }
}

fn hash(bytes: &[u8]) -> Result<[u8; 32], CertificateError> {
    if bytes.len() > CERTIFICATE_BYTES_LIMIT {
        return Err(CertificateError::CertificateMetadata);
    }
    let mut digest = [0; 32];
    // SAFETY: C01: the OS-owned SHA-256 pseudo-handle is never closed. Input and
    // input capped at 64 KiB and exact 32-byte output remain live for this call; no key,
    // retained pointer, secret buffer or caller-owned native resource is created.
    let status = unsafe { BCryptHash(BCRYPT_SHA256_ALG_HANDLE, None, bytes, &mut digest) };
    status_result(status, CertificateStage::Hash)?;
    Ok(digest)
}

fn property(
    handle: NCRYPT_HANDLE,
    name: PCWSTR,
    limit: usize,
    stage: CertificateStage,
) -> Result<Vec<u8>, CertificateError> {
    let mut length = 0;
    let silent = OBJECT_SECURITY_INFORMATION(NCRYPT_SILENT_FLAG.0);
    // SAFETY: C02: handle is a live acquired key or owned associated-provider
    // handle held by Resources. name is a static SDK PCWSTR. This size-only call
    // writes to one valid u32 and never permits UI or transfers handle ownership.
    unsafe { NCryptGetProperty(handle, name, None, &mut length, silent) }
        .map_err(|error| native_error(stage, error))?;
    let length = length as usize;
    if length == 0 || length > limit || length > PROPERTY_BYTES_LIMIT {
        return Err(CertificateError::KeyPolicyUnavailable);
    }
    let mut bytes = vec![0; length];
    let mut written = 0;
    // SAFETY: C03: the same live handle/static property name is used. The output
    // is bounded before allocation and passed with its actual slice length.
    // No pointer escapes; changed/malformed result lengths are rejected below.
    unsafe { NCryptGetProperty(handle, name, Some(&mut bytes), &mut written, silent) }
        .map_err(|error| native_error(stage, error))?;
    if written as usize != bytes.len() {
        return Err(CertificateError::KeyPolicyUnavailable);
    }
    Ok(bytes)
}

fn dword(handle: NCRYPT_HANDLE, name: PCWSTR) -> Result<u32, CertificateError> {
    let bytes = property(handle, name, 4, CertificateStage::ReadKeyPolicy)?;
    let bytes: [u8; 4] = bytes
        .try_into()
        .map_err(|_| CertificateError::KeyPolicyUnavailable)?;
    Ok(u32::from_ne_bytes(bytes))
}

fn wide_string(bytes: &[u8]) -> Result<String, CertificateError> {
    let (pairs, remainder) = bytes.as_chunks::<2>();
    if bytes.len() < 2 || !remainder.is_empty() {
        return Err(CertificateError::KeyPolicyUnavailable);
    }
    let mut words = pairs
        .iter()
        .map(|pair| u16::from_ne_bytes([pair[0], pair[1]]))
        .collect::<Vec<_>>();
    if words.pop() != Some(0) || words.contains(&0) {
        return Err(CertificateError::KeyPolicyUnavailable);
    }
    String::from_utf16(&words).map_err(|_| CertificateError::KeyPolicyUnavailable)
}

struct AcquiredKey {
    handle: HCRYPTPROV_OR_NCRYPT_KEY_HANDLE,
    specification: CERT_KEY_SPEC,
    caller_free: bool,
}

#[derive(Default)]
struct Resources {
    store: Option<HCERTSTORE>,
    cursor: Option<*const CERT_CONTEXT>,
    matched: Option<*const CERT_CONTEXT>,
    key: Option<AcquiredKey>,
    provider: Option<NCRYPT_HANDLE>,
    public_key: Option<BCRYPT_KEY_HANDLE>,
    #[cfg(test)]
    vector_algorithm: Option<BCRYPT_ALG_HANDLE>,
}

impl Resources {
    fn open(&mut self, store: CertificateStore) -> Result<(), CertificateError> {
        let location = match store {
            CertificateStore::CurrentUserMy => CERT_SYSTEM_STORE_CURRENT_USER,
            CertificateStore::LocalMachineMy => CERT_SYSTEM_STORE_LOCAL_MACHINE,
        };
        let flags = CERT_STORE_READONLY_FLAG
            | CERT_STORE_OPEN_EXISTING_FLAG
            | CERT_OPEN_STORE_FLAGS(location);
        // SAFETY: C04: the static local registry-store provider and static "MY"
        // name forbid remote/file/implicit store selection. No store is created.
        // A successful handle is immediately owned by self and closed on every
        // result/unwind path; the static UTF-16 pointer lives through the call.
        let store = unsafe {
            CertOpenStore(
                CERT_STORE_PROV_SYSTEM_REGISTRY_W,
                CERT_QUERY_ENCODING_TYPE(0),
                None,
                flags,
                Some(w!("MY").as_ptr().cast()),
            )
        }
        .map_err(|error| native_error(CertificateStage::OpenStore, error))?;
        self.store = Some(store);
        Ok(())
    }

    fn select(
        &mut self,
        selector: &CertificateSelector,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<(), CertificateError> {
        let store = self.store.ok_or(CertificateError::CertificateMetadata)?;
        let mut total = 0usize;
        for count in 0..=CERTIFICATE_LIMIT {
            check_cancel(cancelled)?;
            let previous = self.cursor.take();
            // SAFETY: C05: store is owned and open. previous, when present, is
            // exactly the preceding enumeration reference. The API consumes it
            // even on failure, so it was removed from self before this call.
            // The retained matched duplicate has a separate owned reference.
            let current = unsafe { CertEnumCertificatesInStore(store, previous) };
            if current.is_null() {
                // SAFETY: C06: read this thread's last error immediately after
                // the failed enumeration, before any other native call.
                let error = unsafe { GetLastError() };
                if error.0 == CRYPT_E_NOT_FOUND.0 as u32 || error == ERROR_NO_MORE_FILES {
                    return self.matched.map(|_| ()).ok_or(CertificateError::NotFound);
                }
                return Err(CertificateError::Native {
                    stage: CertificateStage::ReadCertificate,
                    code: error.0 as i32,
                });
            }
            self.cursor = Some(current);
            if count == CERTIFICATE_LIMIT {
                return Err(CertificateError::SearchLimit);
            }
            let der = self.der(current)?;
            total = total
                .checked_add(der.len())
                .filter(|total| *total <= TOTAL_CERTIFICATE_BYTES_LIMIT)
                .ok_or(CertificateError::SearchLimit)?;
            if hash(der)? != selector.certificate_sha256() {
                continue;
            }
            if self.matched.is_some() {
                return Err(CertificateError::AmbiguousMatch);
            }
            // SAFETY: C07: current is a live enumeration context. Duplicate
            // increments the same context's reference count, not a second store
            // lookup. self owns this extra reference until key/sign/verify end,
            // independently of enumeration consuming current on its next call.
            let retained = unsafe { CertDuplicateCertificateContext(Some(current)) };
            if retained.is_null() {
                return Err(CertificateError::CertificateMetadata);
            }
            self.matched = Some(retained);
        }
        Err(CertificateError::SearchLimit)
    }

    fn der(&self, context: *const CERT_CONTEXT) -> Result<&[u8], CertificateError> {
        if context.is_null() || !(self.cursor == Some(context) || self.matched == Some(context)) {
            return Err(CertificateError::CertificateMetadata);
        }
        // SAFETY: C08: context is an OS-returned live reference owned by self as
        // cursor or matched. Its CERT_CONTEXT allocation remains alive for this
        // borrow; this method cannot advance enumeration or free that reference.
        let context = unsafe { &*context };
        let length = context.cbCertEncoded as usize;
        if context.pbCertEncoded.is_null() || length == 0 || length > CERTIFICATE_BYTES_LIMIT {
            return Err(CertificateError::CertificateMetadata);
        }
        // SAFETY: C09: the live OS context owns cbCertEncoded readable bytes.
        // Length is nonzero and capped at 64 KiB before constructing the slice.
        // The returned borrow is tied to self and cannot outlive its reference.
        Ok(unsafe { slice::from_raw_parts(context.pbCertEncoded, length) })
    }

    fn certificate_info(&self) -> Result<&CERT_INFO, CertificateError> {
        let context = self.matched.ok_or(CertificateError::CertificateMetadata)?;
        // SAFETY: C10: matched is the retained, nonnull OS context. Its owned
        // reference remains in self throughout all key/sign/verify operations.
        let info = unsafe { (*context).pCertInfo };
        if info.is_null() {
            return Err(CertificateError::CertificateMetadata);
        }
        // SAFETY: C11: pCertInfo is OS-decoded data owned by the retained context.
        // This reference borrows self, preventing cleanup during its use.
        Ok(unsafe { &*info })
    }

    fn validate_time(&self) -> Result<(), CertificateError> {
        let info = self.certificate_info()?;
        // SAFETY: C12: info belongs to the same retained certificate. A null
        // comparison time requests the OS clock; this call performs no chain
        // retrieval, network access, key access or ownership transfer.
        if unsafe { CertVerifyTimeValidity(None, info) } != 0 {
            return Err(CertificateError::CertificateMetadata);
        }
        Ok(())
    }

    fn acquire(&mut self) -> Result<NCRYPT_KEY_HANDLE, CertificateError> {
        let context = self.matched.ok_or(CertificateError::CertificateMetadata)?;
        let mut handle = HCRYPTPROV_OR_NCRYPT_KEY_HANDLE::default();
        let mut specification = CERT_KEY_SPEC::default();
        let mut caller_free = BOOL::default();
        let flags = CRYPT_ACQUIRE_ONLY_NCRYPT_KEY_FLAG
            | CRYPT_ACQUIRE_SILENT_FLAG
            | CRYPT_ACQUIRE_NO_HEALING
            | CRYPT_ACQUIRE_COMPARE_KEY_FLAG;
        // SAFETY: C13: context is the same retained match, not re-resolved.
        // Outputs are valid local slots. Flags prohibit CAPI fallback, UI,
        // healing and requested key caching. No provider name is passed: the
        // certificate's associated key/provider is acquired, never rebound.
        unsafe {
            CryptAcquireCertificatePrivateKey(
                context,
                flags,
                None,
                &mut handle,
                Some(&mut specification),
                Some(&mut caller_free),
            )
        }
        .map_err(|error| native_error(CertificateStage::AcquireKey, error))?;
        self.key = Some(AcquiredKey {
            handle,
            specification,
            caller_free: caller_free.as_bool(),
        });
        if specification != CERT_NCRYPT_KEY_SPEC || handle.0 == 0 {
            return Err(CertificateError::UnsupportedKey);
        }
        Ok(NCRYPT_KEY_HANDLE(handle.0))
    }

    fn validate_policy(
        &mut self,
        selector: &CertificateSelector,
        key: NCRYPT_KEY_HANDLE,
    ) -> Result<u32, CertificateError> {
        let bytes = property(
            NCRYPT_HANDLE(key.0),
            NCRYPT_PROVIDER_HANDLE_PROPERTY,
            size_of::<usize>(),
            CertificateStage::ReadProvider,
        )?;
        let bytes: [u8; size_of::<usize>()] = bytes
            .try_into()
            .map_err(|_| CertificateError::KeyPolicyUnavailable)?;
        let provider = NCRYPT_HANDLE(usize::from_ne_bytes(bytes));
        if provider.0 == 0 {
            return Err(CertificateError::KeyPolicyUnavailable);
        }
        // NCRYPT_PROVIDER_HANDLE_PROPERTY returns an independently owned
        // provider reference. It is not the certificate's borrowed key handle.
        self.provider = Some(provider);
        let name = wide_string(&property(
            provider,
            NCRYPT_NAME_PROPERTY,
            PROPERTY_BYTES_LIMIT,
            CertificateStage::ReadProvider,
        )?)?;
        if name != "Microsoft Software Key Storage Provider" {
            return Err(CertificateError::ProviderNotAllowed);
        }
        let key = NCRYPT_HANDLE(key.0);
        let group = wide_string(&property(
            key,
            NCRYPT_ALGORITHM_GROUP_PROPERTY,
            PROPERTY_BYTES_LIMIT,
            CertificateStage::ReadKeyPolicy,
        )?)?;
        let exports = dword(key, NCRYPT_EXPORT_POLICY_PROPERTY)?;
        let usage = dword(key, NCRYPT_KEY_USAGE_PROPERTY)?;
        let bits = dword(key, NCRYPT_LENGTH_PROPERTY)?;
        validate_key_policy(
            selector.allowed_provider(),
            &name,
            Some(exports),
            usage & NCRYPT_ALLOW_SIGNING_FLAG != 0,
            group == "RSA",
            bits,
        )?;
        Ok(bits)
    }

    fn sign_and_verify(
        &mut self,
        key: NCRYPT_KEY_HANDLE,
        bits: u32,
        input: &[u8],
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Vec<u8>, CertificateError> {
        check_cancel(cancelled)?;
        let digest = hash(input)?;
        let padding = BCRYPT_PSS_PADDING_INFO {
            pszAlgId: BCRYPT_SHA256_ALGORITHM,
            cbSalt: 32,
        };
        let flags = NCRYPT_PAD_PSS_FLAG | NCRYPT_SILENT_FLAG;
        let mut length = 0;
        // SAFETY: C14: key is live while its retained certificate remains owned.
        // padding points to a live PSS structure with static SHA-256 identifier;
        // digest is exactly 32 bytes. This size-only call transfers no resources.
        unsafe {
            NCryptSignHash(
                key,
                Some(ptr::from_ref(&padding).cast()),
                &digest,
                None,
                &mut length,
                flags,
            )
        }
        .map_err(|error| native_error(CertificateStage::Sign, error))?;
        let expected = bits.div_ceil(8) as usize;
        if !(256..=512).contains(&expected) || length as usize != expected {
            return Err(CertificateError::SignatureInvalid);
        }
        check_cancel(cancelled)?;
        let mut signature = vec![0; expected];
        let mut written = 0;
        // SAFETY: C15: same live key, digest and PSS parameters as the size query.
        // The exact bounded output slice is supplied; a changed output length is
        // rejected. SILENT remains set. No key material or pointer is exported.
        unsafe {
            NCryptSignHash(
                key,
                Some(ptr::from_ref(&padding).cast()),
                &digest,
                Some(&mut signature),
                &mut written,
                flags,
            )
        }
        .map_err(|error| native_error(CertificateStage::Sign, error))?;
        if written as usize != expected {
            return Err(CertificateError::SignatureInvalid);
        }
        check_cancel(cancelled)?;
        let mut public_key = BCRYPT_KEY_HANDLE::default();
        let info = self.certificate_info()?;
        // SAFETY: C16: SubjectPublicKeyInfo is borrowed from the same retained
        // matched CERT_CONTEXT used to acquire/sign. Only its public key is
        // imported. The successful output is immediately owned by self and
        // destroyed on every exit; the reserved pointer is null.
        unsafe {
            CryptImportPublicKeyInfoEx2(
                X509_ASN_ENCODING,
                ptr::from_ref(&info.SubjectPublicKeyInfo),
                CRYPT_IMPORT_PUBLIC_KEY_FLAGS(0),
                None,
                &mut public_key,
            )
        }
        .map_err(|error| native_error(CertificateStage::ImportPublicKey, error))?;
        self.public_key = Some(public_key);
        verify_digest(public_key, &digest, &signature)?;
        self.validate_time()?;
        check_cancel(cancelled)?;
        Ok(signature)
    }

    fn cleanup(&mut self) -> Result<(), CertificateError> {
        let mut first = None;
        let mut record = |stage, error: windows::core::Error| {
            first.get_or_insert(CertificateError::CleanupUnconfirmed {
                stage,
                code: error.code().0,
            });
        };
        if let Some(key) = self.key.take()
            && key.caller_free
            && key.handle.0 != 0
        {
            // SAFETY: C17: successful acquisition transferred ownership only
            // when caller_free is true. take() makes release exactly once.
            // The retained context remains alive. The unexpected CAPI arm
            // only releases a typed rejected result; it never signs/falls back.
            let result = unsafe {
                if key.specification == CERT_NCRYPT_KEY_SPEC {
                    NCryptFreeObject(NCRYPT_HANDLE(key.handle.0))
                } else {
                    CryptReleaseContext(key.handle.0, 0)
                }
            };
            if let Err(error) = result {
                record(CertificateStage::ReleaseKey, error);
            }
        }
        if let Some(provider) = self.provider.take() {
            // SAFETY: C18: the property getter transferred an owned provider
            // reference; take() prevents double free, including Drop after finish.
            if let Err(error) = unsafe { NCryptFreeObject(provider) } {
                record(CertificateStage::ReleaseProvider, error);
            }
        }
        if let Some(public_key) = self.public_key.take() {
            // SAFETY: C19: this is only an owned imported public-key handle.
            // No outstanding operation uses it; take() prevents repeated release.
            let status = unsafe { BCryptDestroyKey(public_key) };
            if status.0 < 0 {
                first.get_or_insert(CertificateError::CleanupUnconfirmed {
                    stage: CertificateStage::ReleasePublicKey,
                    code: status.0,
                });
            }
        }
        #[cfg(test)]
        if let Some(algorithm) = self.vector_algorithm.take() {
            // SAFETY: C24 (test-only): this uniquely owned algorithm handle was
            // opened only for importing a published public vector. Its public
            // key has already been destroyed; no store/private key was opened.
            let status = unsafe { BCryptCloseAlgorithmProvider(algorithm, 0) };
            if status.0 < 0 {
                first.get_or_insert(CertificateError::CleanupUnconfirmed {
                    stage: CertificateStage::ReleasePublicKey,
                    code: status.0,
                });
            }
        }
        for (context, stage) in [
            (self.matched.take(), CertificateStage::ReleaseCertificate),
            (self.cursor.take(), CertificateStage::ReleaseCertificate),
        ] {
            if let Some(context) = context {
                // SAFETY: C20: each entry owns one distinct reference, even if
                // both pointers have the same value after Duplicate. All key
                // operations ended; borrowed key handles were not manually freed.
                // Enumeration-consumed references were already removed by take().
                let result = unsafe { CertFreeCertificateContext(Some(context)) };
                if !result.as_bool() {
                    first.get_or_insert(CertificateError::CleanupUnconfirmed { stage, code: -1 });
                }
            }
        }
        if let Some(store) = self.store.take() {
            // SAFETY: C21: close the unique opened store once, after releasing
            // every locally owned context. No FORCE flag invalidates borrowed
            // state. Close failure is reported/quarantined, never retried.
            if let Err(error) = unsafe { CertCloseStore(Some(store), 0) } {
                first.get_or_insert(CertificateError::CleanupUnconfirmed {
                    stage: CertificateStage::CloseStore,
                    code: error.code().0,
                });
            }
        }
        match first {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

impl Drop for Resources {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}

fn verify_digest(
    public_key: BCRYPT_KEY_HANDLE,
    digest: &[u8; 32],
    signature: &[u8],
) -> Result<(), CertificateError> {
    verify_digest_with_salt(public_key, digest, signature, 32)
}

fn verify_digest_with_salt(
    public_key: BCRYPT_KEY_HANDLE,
    digest: &[u8; 32],
    signature: &[u8],
    salt_bytes: u32,
) -> Result<(), CertificateError> {
    let padding = BCRYPT_PSS_PADDING_INFO {
        pszAlgId: BCRYPT_SHA256_ALGORITHM,
        cbSalt: salt_bytes,
    };
    // SAFETY: C22: public_key is an owned imported public key kept alive by the
    // caller. Exact live digest/signature slices and a live PSS-SHA256 structure
    // are borrowed only for this call. Production fixes salt to 32; only the
    // public-vector negative test supplies 20. No ownership is transferred.
    let status = unsafe {
        BCryptVerifySignature(
            public_key,
            Some(ptr::from_ref(&padding).cast()),
            digest,
            signature,
            BCRYPT_PAD_PSS,
        )
    };
    if status.0 < 0 {
        return Err(CertificateError::SignatureInvalid);
    }
    Ok(())
}

pub(crate) fn sign_ps256(
    selector: &CertificateSelector,
    input: &[u8],
    cancelled: &dyn Fn() -> bool,
) -> WorkOutcome {
    let mut resources = Resources::default();
    let result = (|| {
        check_cancel(cancelled)?;
        resources.open(selector.store())?;
        resources.select(selector, cancelled)?;
        check_cancel(cancelled)?;
        resources.validate_time()?;
        let key = resources.acquire()?;
        check_cancel(cancelled)?;
        let bits = resources.validate_policy(selector, key)?;
        resources.sign_and_verify(key, bits, input, cancelled)
    })();
    match resources.cleanup() {
        Ok(()) => WorkOutcome {
            result,
            cleanup_confirmed: true,
        },
        Err(error) => WorkOutcome {
            result: Err(error),
            cleanup_confirmed: false,
        },
    }
}

pub(crate) fn assertion_nonce() -> Result<[u8; 16], CertificateError> {
    let mut nonce = [0; 16];
    // SAFETY: C23: the system-preferred RNG requires no provider handle. The
    // live output is exactly 16 bytes; no key/store resource or retained pointer
    // is created, and no UI or fallback generator is permitted.
    let status = unsafe { BCryptGenRandom(None, &mut nonce, BCRYPT_USE_SYSTEM_PREFERRED_RNG) };
    status_result(status, CertificateStage::Nonce)?;
    Ok(nonce)
}

#[cfg(test)]
pub(crate) fn verify_public_vector(
    modulus: &[u8],
    exponent: &[u8],
    message: &[u8],
    signature: &[u8],
    salt_bytes: u32,
) -> Result<(), CertificateError> {
    if modulus.len() != 256
        || exponent.len() > 8
        || exponent.is_empty()
        || signature.len() != modulus.len()
    {
        return Err(CertificateError::InvalidInput);
    }
    let mut resources = Resources::default();
    let result = (|| {
        let mut algorithm = BCRYPT_ALG_HANDLE::default();
        // SAFETY: C25 (test-only): both algorithm names are static/null; the
        // output slot is valid. The successful handle is immediately retained
        // for cleanup. This opens a public verification algorithm, not a KSP.
        let status = unsafe {
            BCryptOpenAlgorithmProvider(
                &mut algorithm,
                BCRYPT_RSA_ALGORITHM,
                PCWSTR::null(),
                BCRYPT_OPEN_ALGORITHM_PROVIDER_FLAGS(0),
            )
        };
        status_result(status, CertificateStage::ImportPublicKey)?;
        resources.vector_algorithm = Some(algorithm);
        let mut blob = Vec::new();
        for field in [
            BCRYPT_RSAPUBLIC_MAGIC.0,
            2048,
            exponent.len() as u32,
            modulus.len() as u32,
            0,
            0,
        ] {
            blob.extend_from_slice(&field.to_le_bytes());
        }
        blob.extend_from_slice(exponent);
        blob.extend_from_slice(modulus);
        let mut public_key = BCRYPT_KEY_HANDLE::default();
        // SAFETY: C26 (test-only): the bounded RSA public blob has the six DWORD
        // SDK header fields followed by exact public exponent/modulus lengths.
        // No private factors exist. The owned imported public handle is retained
        // immediately and destroyed before its algorithm handle on every exit.
        let status = unsafe {
            BCryptImportKeyPair(
                algorithm,
                None,
                BCRYPT_RSAPUBLIC_BLOB,
                &mut public_key,
                &blob,
                0,
            )
        };
        status_result(status, CertificateStage::ImportPublicKey)?;
        resources.public_key = Some(public_key);
        let digest = hash(message)?;
        verify_digest_with_salt(public_key, &digest, signature, salt_bytes)
    })();
    resources.cleanup()?;
    result
}
