// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use fabric_cli_update::{Error, Repository, Target, Trust};
use ring::{signature::Ed25519KeyPair, signature::KeyPair};

pub(super) const VARIANT: &str = "local-native-fixture";

pub(super) fn target() -> Result<Target, Error> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => Ok(Target::WindowsX64),
        ("windows", "aarch64") => Ok(Target::WindowsArm64),
        ("linux", "x86_64") if cfg!(target_env = "gnu") => Ok(Target::LinuxX64),
        ("linux", "aarch64") if cfg!(target_env = "gnu") => Ok(Target::LinuxArm64),
        ("macos", "x86_64") => Ok(Target::MacX64),
        ("macos", "aarch64") => Ok(Target::MacArm64),
        _ => Err(Error::UnsupportedTarget),
    }
}

pub(super) fn key() -> Ed25519KeyPair {
    // Only these separate, feature-gated fixture targets contain a test seed.
    Ed25519KeyPair::from_seed_unchecked(&[23; 32]).expect("synthetic fixture seed")
}

pub(super) fn trust() -> Result<Trust, Error> {
    let mut public = [0; 32];
    public.copy_from_slice(key().public_key().as_ref());
    Trust::new(
        Repository::new("fixture-publisher", "fixture-product")?,
        Some(public),
    )
}
