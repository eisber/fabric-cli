// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! One-shot legacy Keychain protected-store helper, never linked into the SDK.

#![deny(unnameable_test_items)]

use fabric_identity::interactive::macos::{
    PROTECTED_WIRE_LIMIT, ProtectedError, ProtectedReply, ProtectedRequest,
};
use std::io::{Read, Write};

fn main() -> std::process::ExitCode {
    let arguments: Vec<_> = std::env::args_os().skip(1).take(2).collect();
    if arguments.len() == 1 && arguments[0] == "--help" {
        return write_bytes(b"fabric-protected-store-helper: one bounded protected-store JSON request on stdin; no credential arguments.\n");
    }
    if arguments.len() == 1 && arguments[0] == "--version" {
        return write_bytes(b"fabric-protected-store-helper fabric-protected-store-v1\n");
    }
    let reply = if arguments.is_empty() {
        read_request().map_or_else(ProtectedReply::error, |request| native(&request))
    } else {
        ProtectedReply::error(ProtectedError::InvalidInput)
    };
    match reply.encode() {
        Ok(bytes) => write_bytes(&bytes),
        Err(_) => write_bytes(
            br#"{"status":"error","schema":"fabric-protected-store-v1","code":"unavailable"}"#,
        ),
    }
}

fn read_request() -> Result<ProtectedRequest, ProtectedError> {
    let mut bytes = Vec::new();
    std::io::stdin()
        .lock()
        .take((PROTECTED_WIRE_LIMIT + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| ProtectedError::InvalidInput)?;
    ProtectedRequest::decode(&bytes)
}

fn write_bytes(bytes: &[u8]) -> std::process::ExitCode {
    match std::io::stdout().lock().write_all(bytes) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(_) => std::process::ExitCode::FAILURE,
    }
}

#[cfg(not(target_os = "macos"))]
fn native(_: &ProtectedRequest) -> ProtectedReply {
    ProtectedReply::error(ProtectedError::UnsupportedPlatform)
}

#[cfg(target_os = "macos")]
fn native(request: &ProtectedRequest) -> ProtectedReply {
    use fabric_identity::interactive::macos::{PROTECTED_SERVICE, ProtectedBackend};
    use security_framework::{
        item::{ItemClass, ItemSearchOptions},
        os::macos::keychain::{SecKeychain, SecPreferencesDomain},
    };

    struct LegacyStore {
        keychain: SecKeychain,
    }

    impl ProtectedBackend for LegacyStore {
        fn read(&self, locator: &str) -> Result<Vec<u8>, i32> {
            self.keychain
                .find_generic_password(PROTECTED_SERVICE, locator)
                .map(|(password, _)| password.to_owned())
                .map_err(|error| error.code())
        }

        fn upsert(&self, locator: &str, bytes: &[u8]) -> Result<(), i32> {
            match self
                .keychain
                .find_generic_password(PROTECTED_SERVICE, locator)
            {
                Ok((_, mut item)) => item.set_password(bytes),
                Err(error) if error.code() == -25300 => {
                    self.keychain
                        .add_generic_password(PROTECTED_SERVICE, locator, bytes)
                }
                Err(error) => Err(error),
            }
            .map_err(|error| error.code())
        }

        fn delete(&self, locator: &str) -> Result<(), i32> {
            ItemSearchOptions::new()
                .keychains(std::slice::from_ref(&self.keychain))
                .class(ItemClass::generic_password())
                .service(PROTECTED_SERVICE)
                .account(locator)
                .delete()
                .map_err(|error| error.code())
        }
    }

    // This process-global switch is NEVER called in a library. Legacy Keychain
    // operations ignore per-query AuthUI flags; all three operations need it.
    let Ok(_no_ui) = SecKeychain::disable_user_interaction() else {
        return ProtectedReply::error(ProtectedError::Unavailable);
    };
    let keychain = match SecKeychain::default_for_domain(SecPreferencesDomain::User) {
        Ok(keychain) => keychain,
        Err(_) => return ProtectedReply::error(ProtectedError::Unavailable),
    };
    request.execute(&LegacyStore { keychain })
}
