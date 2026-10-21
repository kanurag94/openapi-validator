# openapi-validator

[![CI](https://github.com/kanurag94/openapi-validator/actions/workflows/ci.yml/badge.svg)](https://github.com/kanurag94/openapi-validator/actions/workflows/ci.yml)

A Rust library that validates HTTP requests and responses against an OpenAPI 3.0 or 3.1 document at run time. It is built to run inside an API gateway on every call: it never panics, its time and memory are bounded for any input, and one loaded document can be shared by all threads.

## Features

- Supports OpenAPI 3.0.x and 3.1.x documents in JSON or YAML 1.2.
- Routes requests by server path, path template and method.
- Checks path, query, header and cookie parameters in every style and explode combination.
- Checks security requirements for credential presence, with a hook for your own verification.
- Validates JSON, form, multipart and text bodies, and checks XML for well-formedness.
- Validates response status, headers and bodies.
- Handles 3.0 and 3.1 schema differences, readOnly/writeOnly, discriminators and exact numbers.
- Runs ECMA-262 patterns in linear time.
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

Each problem has a kind, a location such as `query.limit` or `body/items/3`, and a one-sentence message. A report keeps at most 20 problems or 8 KiB of text and counts the rest as "and N more". `Options` controls these limits, body checking, unknown-parameter handling, server path matching and format assertion.

### C and Go

The `openapi-validator-ffi` crate exposes a C ABI. The header, [`ffi/include/openapi_validator.h`](ffi/include/openapi_validator.h), documents memory ownership and threading. Every entry point catches panics and returns an error code.

A Go wrapper using cgo is in [`ffi/go`](ffi/go):

```sh
cargo build --release -p openapi-validator-ffi
cd ffi/go && go test ./...
```

## Safety and limits

- Schema verdicts come from the [`jsonschema`](https://crates.io/crates/jsonschema) crate, compiled lazily per operation. Its error listing is exponential on recursive schemas, so problems are located by a separate, bounded pass that never changes the verdict.
- Patterns that need backtracking (lookaround, backreferences) are refused at load, with an error that names the pattern.
- Numbers are compared as exact decimals, never as floats.
- JSON with duplicate keys, invalid UTF-8 or lone surrogates is refused, to prevent parser differentials.
- Nesting and in-place schema chains are bounded at load, so the worst accepted documents validate on a 256 KiB stack in release builds.
- External `$ref`s are refused; the library never reads files or the network.

Where the specifications leave a choice open, the library picks the behaviour that never refuses a valid call. Each choice is listed in [`SPEC_DECISIONS.md`](SPEC_DECISIONS.md).

## Not supported

- Swagger 2.0 and OpenAPI 3.2 documents.
- External references; bundle the document first.
- Checking XML bodies against schemas.
- IDNA rules for `hostname`, and formats other than date, time, date-time, email, uuid, uri, uri-reference, ipv4, ipv6, hostname, int32 and int64.
- Credential verification beyond presence.
- Callbacks, webhooks and links.
- Compressed request or response bodies.

## Performance

Measured with `cargo bench` on an Apple M-series machine, using the synthetic document in `benches/support/synthetic.rs`.

| Measure | Target | Result |
|---|---|---|
| Build, 1.6 MiB, 1,200 paths | < 200 ms, < 50 MB | 61 ms, 26 MiB |
| Build, 2.4 MiB, 2,000 paths | | 97 ms, 39 MiB |
| GET with three parameters | | 1.6 µs |
| POST with a valid 4-level JSON body | | 6.6 µs |
| POST with an invalid 4-level JSON body | | 26 µs |
| 16-level recursive `anyOf`, invalid | < 1 ms | 8.9 µs |
| Pattern `^(a+)+$` on 50,000 characters | linear | 47 µs |

Where correctness and speed conflict, correctness wins. For example, `enum`, `const` and `uniqueItems` use JSON Schema equality instead of the engine's faster built-ins, and every repeated header line and query value is checked.

## Testing

| Suite | Result |
|---|---|
| Unit and integration tests | 147 passing |
| Conformance cases in `tests/cases/` | 949 of 949 |
| JSON-Schema-Test-Suite, draft 2020-12 | 1,257 of 1,302 (96.5%) |
| Property tests | 3 properties, 1,000 cases each |
| Fuzzing | 2.5 hours, no crashes |

Of the JSON-Schema-Test-Suite failures, 44 need remote documents, which are refused by design, and one needs a custom metaschema vocabulary.

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
