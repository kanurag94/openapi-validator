# openapi-validator

[![CI](https://github.com/kanurag94/openapi-validator/actions/workflows/ci.yml/badge.svg)](https://github.com/kanurag94/openapi-validator/actions/workflows/ci.yml)

A Rust library that validates HTTP requests and responses against an OpenAPI 3.0, 3.1 or 3.2 document, or a Swagger 2.0 one, at run time. It is built to run inside an API gateway on every call: it never panics, its time and memory are bounded for any input, and one loaded document can be shared by all threads.

## Features

- Supports OpenAPI 3.0.x, 3.1.x and 3.2.x documents (through 3.1.2 and 3.2.1) and Swagger 2.0 documents, in JSON or YAML 1.2, in one document or several.
- Routes requests by server path, path template and method, including 3.2's `query` and `additionalOperations`.
- Checks path, query, header and cookie parameters in every style and explode combination, and 3.2 `querystring` parameters.
- Checks security requirements for credential presence, with a hook for your own verification.
- Validates JSON, form, multipart, XML and text bodies, compressed or not.
- Validates 3.2 sequential bodies (JSON Lines, NDJSON, JSON text sequences, Server-Sent Events) and positional multipart bodies.
- Validates response status, headers and bodies.
- Validates webhook and callback requests and responses.
- Handles each version's schema rules, readOnly/writeOnly, discriminators and exact numbers.
- Runs ECMA-262 patterns in linear time, and lookaround and backreferences on a bounded engine.
- Warns about documents that break a MUST of the specification, or refuses them on request.
- Provides a C ABI and a Go wrapper.

## Installation

The crate is not yet published on crates.io. Add it as a Git dependency:

```toml
[dependencies]
openapi-validator = { git = "https://github.com/kanurag94/openapi-validator", tag = "v0.1.0" }
```

## Usage

```rust
use openapi_validator::{Options, Request, Spec};

let spec = Spec::load(document_bytes, Options::default())?;

let report = spec.validate_request(&Request {
    method: "POST",
    path: "/v1/pets/7",
    query: "notify=true",
    headers: &[("Content-Type", "application/json")],
    body: br#"{"name": "rex"}"#,
});

for problem in report.problems() {
    println!("{} {}: {}", problem.kind, problem.location, problem.message);
}
```

Load the document once and share the `Spec`; it is `Send + Sync`. Pass the request as received, with the path still percent-encoded and header lines in arrival order. Use `validate_response` to check a response against the operation its request matches.

A description spread over several documents loads with `Spec::load_documents`, which takes every document with its URI; nothing is fetched. Webhooks and callbacks are named rather than routed: use `validate_webhook_request` with the webhook's name, or `validate_callback_request` with the operation's `operationId` and the callback's name, and their `_response` counterparts.

Each problem has a kind, a location such as `query.limit` or `body/items/3`, and a one-sentence message. A report keeps at most 20 problems or 8 KiB of text and counts the rest as "and N more". `Options` controls these limits, body checking, unknown-parameter handling, server path matching and the choices listed in [`SPEC_DECISIONS.md`](SPEC_DECISIONS.md). The defaults follow the specification text; `Options::gateway_profile()` switches on stricter choices for gateways.

### C and Go

The `openapi-validator-ffi` crate exposes a C ABI. The header, [`ffi/include/openapi_validator.h`](ffi/include/openapi_validator.h), documents memory ownership and threading. Every entry point catches panics and returns an error code.

A Go wrapper using cgo is in [`ffi/go`](ffi/go):

```sh
cargo build --release -p openapi-validator-ffi
cd ffi/go && go test ./...
```

## Safety and limits

- Schema verdicts come from the [`jsonschema`](https://crates.io/crates/jsonschema) crate, compiled lazily per operation. Its error listing is exponential on recursive schemas, so problems are located by a separate, bounded pass that never changes the verdict.
- Patterns run on the linear-time `regex` crate. Patterns that need lookaround or backreferences run on `fancy-regex` with a step limit per match, and a match that hits the limit is reported, never accepted. The `patterns: LinearOnly` option refuses them at load instead.
- Numbers are compared as exact decimals, never as floats.
- JSON with duplicate keys, invalid UTF-8 or lone surrogates is refused, to prevent parser differentials.
- Nesting and in-place schema chains are bounded at load, so the worst accepted documents validate on a 256 KiB stack in release builds.
- Compressed bodies are decoded up to `max_body_bytes`, so a compression bomb cannot exhaust memory.
- The library never reads files or the network. External `$ref`s must be supplied to `Spec::load_documents`.

Where the specifications leave a choice open, the library picks the behaviour that never refuses a valid call. Each choice is listed in [`SPEC_DECISIONS.md`](SPEC_DECISIONS.md).

## Not supported

- Fetching external documents; supply them to `Spec::load_documents`.
- Credential verification beyond presence; use `security_hook`.
- Links, and evaluating callback runtime expressions; the caller names the callback.
- Incremental validation of streams; sequential bodies are validated whole.
- Custom metaschema vocabularies in `$schema`.

## Performance

Measured with `cargo bench` on an Apple M-series machine, using the synthetic document in `benches/support/synthetic.rs`.

| Measure | Target | Result |
|---|---|---|
| Build, 1.6 MiB, 1,200 paths | < 200 ms, < 50 MB | 73 ms |
| Build, 2.4 MiB, 2,000 paths | | 115 ms, 43 MiB |
| GET with three parameters | | 1.7 µs |
| POST with a valid 4-level JSON body | | 5.0 µs |
| POST with an invalid 4-level JSON body | | 15 µs |
| 16-level recursive `anyOf`, invalid | < 1 ms | 9.2 µs |
| Pattern `^(a+)+$` on 50,000 characters | linear | 67 µs |

A valid body is checked faster than parsing it with `serde_json` and checking it with `jsonschema` alone (5.6 µs for the same body), because `multipleOf` uses the library's exact decimals instead of the engine's rational arithmetic.

Where correctness and speed conflict, correctness wins. For example, `enum`, `const` and `uniqueItems` use JSON Schema equality instead of the engine's faster built-ins, and every repeated query value is checked, with repeated header lines joined into one value.

## Testing

| Suite | Result |
|---|---|
| Unit and integration tests | 191 passing |
| Conformance cases in `tests/cases/` | 1,075 of 1,075 |
| JSON-Schema-Test-Suite, draft 2020-12 | 1,301 of 1,302 (99.9%) |
| JSON-Schema-Test-Suite, optional formats | 890 of 890 |
| Property tests | 3 properties, 1,000 cases each |
| Fuzzing | 2.5 hours, no crashes |

The JSON-Schema-Test-Suite failure needs a custom metaschema vocabulary.

```sh
cargo test                                               # all tests
CASE=parameters cargo test --test conformance            # a subset of cases
JSON_SCHEMA_TEST_SUITE=/path/to/suite cargo test --release --test json_schema_suite
cargo bench
cd fuzz && cargo +nightly fuzz run load
```

## Minimum supported Rust version

Rust 1.88.

## License

Licensed under the [Apache License, Version 2.0](LICENSE).
