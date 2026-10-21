# openapi-validator

Validates HTTP requests and responses against an OpenAPI 3.0 or 3.1 document, at run time, inside an API gateway.

- Never panics; time and memory are bounded for any input.
- Load once, share across threads (`Spec` is `Send + Sync`).
- Rust API, C ABI (`ffi/`) and Go wrapper (`ffi/go`).

## Usage

```rust
use openapi_validator::{Options, Request, Spec};

let spec = Spec::load(document_bytes, Options::default())?;

let report = spec.validate_request(&Request {
    method: "POST",
    path: "/v1/pets/7",                // raw, percent-encoded
    query: "notify=true",
    headers: &[("Content-Type", "application/json")],
    body: br#"{"name": "rex"}"#,
});

for p in report.problems() {
    println!("{} {}: {}", p.kind, p.location, p.message);
}
```

- `validate_response(&request, &response)` checks a response for the matched operation.
- Each problem has a kind, a location (`query.limit`, `header.x-tag`, `body/items/3`) and a message.
- Reports stop at 20 problems or 8 KiB and end with "and N more".
- C header: [`ffi/include/openapi_validator.h`](ffi/include/openapi_validator.h) (documents memory ownership).

## What it checks

- **Documents**
  - OpenAPI 3.0.x and 3.1.x, JSON or YAML 1.2.
  - External `$ref`s are refused at load; nothing is fetched.
- **Routing**
  - Server paths stripped as whole segments; variables from `enum` or `default`.
  - Path-item and operation `servers` override the document's.
  - Path segments decoded one at a time (`%2F` stays in its segment).
  - Concrete paths beat templates; then most literal characters wins.
  - Methods are case-sensitive; HEAD falls back to GET.
- **Parameters**
  - Path, query, header, cookie.
  - Every style and explode combination, including deepObject.
  - `content`-described parameters.
  - Repeated header lines each checked.
  - Optional refusal of undeclared query parameters, headers and cookies.
- **Security**
  - AND within a requirement, OR across requirements; `{}` and `security: []`.
  - `apiKey`, `http`, `oauth2`, `openIdConnect`: presence checks.
  - A hook for your own verification.
- **Bodies**
  - JSON (strict), `+json`, form-urlencoded, multipart/form-data, `text/*`.
  - XML: well-formedness only.
  - Media ranges, charsets, required and empty bodies, size limit.
- **Responses**
  - Status: exact, then `2XX`, then `default`.
  - Headers and bodies; HEAD, 204 and 304 carry no body.
- **Schemas**
  - 3.0: `nullable`, boolean exclusive bounds, `$ref` siblings ignored, lexical integers.
  - 3.1: JSON Schema 2020-12.
  - readOnly/writeOnly per direction; discriminator enforced.
  - Exact numbers; ECMA-262 patterns in linear time.
  - Formats: date, time, date-time, email, uuid, uri, uri-reference, ipv4, ipv6, hostname, int32, int64.

## Design

- **Verdict and explanation are separate.**
  - Schemas are translated to 2020-12 and compiled lazily per operation by `jsonschema`.
  - `jsonschema` only answers "valid or not"; its error listing is exponential on recursive schemas.
  - Invalid values get a second, bounded pass that locates the problems.
- **Linear-time patterns.** Lookahead and backreferences are refused at load, naming the pattern.
- **Exact numbers.** Decimal comparison, never `f64`.
- **Strict JSON.** Duplicate keys, invalid UTF-8 and lone surrogates are refused (parser differentials).
- **Bounded depth and work.**
  - Documents: 128 levels.
  - In-place schema chains (`$ref`, `allOf`, ...): 64 deep, 100,000 wide.
  - Bodies: 128 levels, lowered per document to stay within a stack budget.
  - Worst accepted documents validate on a 256 KiB stack (release).
- **No panics across the API.** Rust and C entry points catch panics and report them.

## Behaviour choices

Rule: never refuse a valid call. Full list with spec sections: [`SPEC_DECISIONS.md`](SPEC_DECISIONS.md).

- Accept, Content-Type and Authorization header parameters are ignored (OAS §4.8.12.2).
- Text becomes a number or boolean only if the schema allows it; any valid reading passes.
- Query strings split on `&` only; `+` is a space (configurable).
- `list=` is an empty array.
- Exploded objects take only their declared properties.
- Security is presence only.
- `nullable` (3.0) needs `type` beside it and respects `enum`.
- Count keywords like `2.0` are counts; negative ones are ignored with a warning.
- Unknown formats are annotations.
- Response header values are literal text, never re-read as JSON.

## Not supported

- Swagger 2.0 and OpenAPI 3.2.
- External references (bundle first).
- XML schema checking.
- IDNA rules for `hostname`; unlisted formats.
- Credential verification beyond presence (use the hook).
- Callbacks, webhooks, links.
- Compressed bodies (`Content-Encoding`).
- readOnly/writeOnly across separate `$ref`-ed `allOf` members.
- Precise locations inside `$dynamicRef` and `$id`-scoped targets (verdict is exact).

## Correctness over speed

- Exact decimals instead of floats.
- Every repeated header, query value and multipart part is checked.
- `enum`, `const`, `uniqueItems` use JSON Schema equality, not the engine's faster built-ins.
- Multi-type parameters are tried under each reading.
- Invalid values pay for a second, explanation pass.

## Benchmarks

Release build, Apple M-series. Synthetic document: `benches/support/synthetic.rs`.

| Measure | Target | Result |
|---|---|---|
| Build, 1.6 MiB, 1,200 paths | < 200 ms, < 50 MB | 61 ms, 26 MiB |
| Build, 2.4 MiB, 2,000 paths | — | 97 ms, 39 MiB |
| First call (compiles) | — | 1.7 ms |
| GET with three parameters | — | 1.6 µs |
| POST, 4-level body, valid | — | 6.6 µs |
| POST, 4-level body, invalid | — | 26 µs |
| Recursive `anyOf`, 16 levels, valid / invalid | < 1 ms | 4.8 / 8.9 µs |
| Pattern `^(a+)+$` on 50 k chars | linear | 47 µs |
| `uniqueItems`, 10 k items | — | 1.8 ms |
| 5,000 query pairs | — | 0.8 ms |

## Tests

| Suite | Result |
|---|---|
| Unit and integration tests | 147 pass |
| Conformance cases (`tests/cases/`) | 949 / 949 |
| JSON-Schema-Test-Suite 2020-12 | 1,257 / 1,302 (96.5%) |
| Property tests | 3 × 1,000 cases |
| Fuzzing | 2.5 h, ~65 M runs, no crash |
| Go cgo test | pass |

- Suite failures: 44 need remote documents (by design), 1 needs a custom vocabulary.
- One conformance case: `CASE=<name> cargo test --test conformance`.
- JSON-Schema-Test-Suite: set `JSON_SCHEMA_TEST_SUITE` to a checkout.
- Local corpus: set `OAS_CORPUS`; skipped otherwise.

## Build

```sh
cargo test
cargo bench
cargo build --release -p openapi-validator-ffi
(cd ffi/go && go test ./...)
(cd fuzz && cargo +nightly fuzz run load)
```

- MSRV: Rust 1.88.
- License: Apache-2.0.
