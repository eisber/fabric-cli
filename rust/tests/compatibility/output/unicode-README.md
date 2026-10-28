<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Unicode JSON contracts

`unicode-json-cases.json` verifies `encode_unicode` and explicit encoder options.
Both use the same scalar/container engine as ordinary ASCII-escaped output.

Fixed cases retain BMP and astral values/keys, separators, control characters,
quotes, slashes, nested ordering, empty values, numeric kinds and every supported
layout. Comparisons use complete UTF-8 bytes, not only semantic JSON equality.
Each observation also checks the unchanged ASCII-escaped policy.

Negative controls reject substituted escaping, key sorting, omitted cases,
newline changes and CRLF normalization. Surrogate-containing output must fail
strict UTF-8 conversion rather than silently substitute characters.

From `rust/`:

```text
cargo run --offline --locked -q -p fabric-core-json --example unicode_json
python -B tests\compatibility\output\unicode_reference.py --observed <artifact>
python -B -m unittest discover -s tests\compatibility\output -p test_unicode_reference.py
```

Save example output to a caller-owned UTF-8 artifact and supply that path.
The reference never rewrites expectations or performs application file writes,
authentication or network requests. Obtain the applicable execution grant on
shared hosts.
