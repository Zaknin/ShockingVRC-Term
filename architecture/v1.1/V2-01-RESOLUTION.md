# V2-01 — JSON Schema interfile reference fix

**Status:** FIXED in architecture v1.1. **Source:** user-supplied second static audit dated 2026-10-15 (GO, optional item V2-01).

Previous `schemas/remote-models-v1.schema.json` declared:

```json
"$id": "https://shockingvrc-term.invalid/schema/remote-models-v1"
```

`schemas/remote-messages-v1.schema.json` has relative `$ref` such as `remote-models-v1.schema.json#/$defs/RemotePolicy`, which resolves against its `$id` to a filename ending in `.schema.json`. A validator registering only declared `$id` URIs therefore could not locate the referenced model.

**Minimal correction:** change the model schema `$id` to `https://shockingvrc-term.invalid/schema/remote-models-v1.schema.json`. Do **not** modify DTO definitions, message schemas, REST OpenAPI or on-wire serialization. These `.invalid` identifiers are logical schema names, **not** network endpoints to fetch from.

**Validation:** JSON Schema 2020-12 syntax and cross-document reference resolution using declared `$id` values only; check existing positive message fixtures. Static documentation validation only, no app functionality tests.
