<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Native release assembly fixtures

These are synthetic developer-tool contracts, not actual artifact, signing,
installer, clean-machine or GA evidence. They do not execute fabricated native
headers or a signing tool. `protocol-v1-fields.json` freezes the existing
`clients/cli/self-updater/src/protocol.rs` field set without changing it.
No source SHA, OS floor, helper digest or package custody is added to that wire
protocol. Those bindings belong to the separate signed assembly manifest.

`scripts/test_assemble_native_release.py` builds synthetic PE/ELF/Mach-O headers,
independently held controller expectations, native CLI and helper build records,
and revision-2 standalone receipts. Public keys and signer references are
explicitly synthetic. There is no private key. The only successful signature
path is mocked at the approved-verifier process boundary and is **not proof
that any signature is valid**. Real signature verification requires separately
approved, digest-pinned OpenSSL 3 and externally supplied signatures.

The tests refuse a checkout working directory. A controller with a fresh
numbered execution grant runs the following from the approved artifact `files`
directory, substituting an absolute checkout path:

```powershell
python -B -c "import sys,unittest; sys.path.insert(0,r'C:\CHECKOUT\rust\scripts'); suite=unittest.defaultTestLoader.discover(r'C:\CHECKOUT\rust\scripts',pattern='test_assemble_native_release.py'); result=unittest.TextTestRunner(verbosity=2).run(suite); sys.exit(not result.wasSuccessful())"
```

Fixture directories are created only beneath that working directory and are
removed using the test-owned `TemporaryDirectory`. No environment mutation,
Cargo, package installation, network, terminal, broker or native candidate
execution is part of this selector. A host that cannot create symbolic links
records a skip; reparse-attribute refusal has a separate synthetic test. Native
filesystem/reparse/ACL behavior still needs independent Windows acceptance.

See [release assembly](../../../authoring-docs/release-assembly.md) for all closed input
records, commands, signature gates, custody limits and publisher inputs.
