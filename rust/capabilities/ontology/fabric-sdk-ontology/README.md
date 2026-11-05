<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Public Ontology SDK

This package consumes the existing typed item client and Fabric operation session.
It installs no HTTP engine, credential provider, runtime, private workload route,
or metadata fallback. The caller supplies immutable endpoint/identity selection,
cancellation, operation bounds and executors.

`get_metadata` uses `ItemClient` and `ItemCollection::Ontology`, preserving ordered
metadata and generation evidence. Missing/null generation is unknown, not Gen1;
future integral generations remain intact. Wrong owner, wrong returned identity
and malformed generation fail explicitly.

`get_model` selects the public whole-model API and its explicit Minimized,
Expanded or provider-default form. The source default is Expanded and can fail a
feature gate. There is no downgrade or private alias fallback. Public response
unknown fields remain opaque and preserved.

`replace_model` is an explicit complete-model replacement on an addressed item,
not item creation or compare-and-swap. `ModelReplacement` is a distinct native
request envelope with bounded serialization; an expanded read is not implicitly
authoring input. Envelope/collection shapes are locally checked, but individual
native object schemas and feature gates remain service-authoritative. The SDK
does not advertise a complete local per-object input validator. Its native
16 MiB ceiling does not override stricter host/transport limits: the existing
shared native HTTP adapter has a separately enforced 4 MiB request ceiling.

`get_definition` preserves provider-default multipart source, including part
order, format, payloads, unknown fields and future opaque payload types. Known
InlineBase64 parts are strictly validated under explicit caller-supplied part
and decoded-byte bounds. It neither flattens files nor rewrites JSON/TMDL,
generates OWL, reads rows, restores sensitivity labels, or claims byte-identical
service round-trip. Local extraction/path/no-clobber policy remains host-owned.

Public operations default explicitly to returning real 202 acceptance, retaining
the original owner and operation handle. Opt-in observation uses the existing
bounded GET reconciliation policy without resubmitting a mutation. SDK failures
retain original typed causes, bounded response data and engine uncertainty;
Debug/Display do not implicitly reveal source content.

This package covers public metadata/model/native-definition operations. Generic
item create/update/delete remain in the existing item SDK; native schema, scripts,
bindings and RDF belong to the separate workload SDK. Source existence, injected
offline tests and compiled code do not prove feature rollout or caller access.
