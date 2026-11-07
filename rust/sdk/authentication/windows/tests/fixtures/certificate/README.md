<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Public PS256 verification vector

`src/certificate_vectors.rs` contains public data from C2SP/Wycheproof,
commit `e0df04e0c033f2d25c5051dd06230336c7822358`,
[`testvectors_v1/rsa_pss_2048_sha256_mgf1_32_test.json`](https://github.com/C2SP/wycheproof/blob/e0df04e0c033f2d25c5051dd06230336c7822358/testvectors_v1/rsa_pss_2048_sha256_mgf1_32_test.json),
first group, test case **3**, marked `valid`.

The source specifies RSA-2048, SHA-256, MGF1 with SHA-256, and **32-byte salt**.
The message is hexadecimal `54657374` (`Test`). The modulus's ASN.1 sign-padding
byte `00` is removed for the unsigned CNG public blob; exponent and signature are
unchanged. This is a reduced/transcribed fixture, not the complete upstream suite.

The test imports **only a public key**, verifies the published signature, rejects
a modified signature, and rejects interpreting this vector with a 20-byte salt.
It performs no certificate-store lookup, private-key import/generation/signing,
device operation, OAuth acquisition or network access. Execution still requires
the controller's explicit native-validation grant and source review.

The vector data is licensed under **Apache-2.0**, reproduced in `LICENSE`.
There is no production key, private factor, certificate or credential fixture.
The repository's test adapter code remains Microsoft-authored MIT code.
