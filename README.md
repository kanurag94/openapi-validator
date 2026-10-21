# openapi-validator

Validates HTTP requests and responses against an OpenAPI 3.0 or 3.1
document at run time, built to sit inside an API gateway and run on every
call: it never panics, its time and memory are bounded for any input, and
one loaded document serves any number of threads.

```rust
use openapi_validator::{Options, Request, Spec};

let spec = Spec::load(document_bytes, Options::default())?;   // once
let report = spec.validate_request(&Request {
    method: "POST",
    path: "/v1/pets/7",                 // as sent, still percent-encoded
    query: "notify=true",
    headers: &[("Content-Type", "application/json"), ("Authorization", "Bearer t0k3n")],
    body: br#"{"name": "rex"}"#,
});
if !report.is_valid() {
    for p in report.problems() {
        println!("{} {}: {}", p.kind, p.location, p.message); // e.g. schema_violation body/name: ...
    }
}
```

`Spec::validate_response(&request, &response)` checks a response against the
operation its request matches. A C ABI (`ffi/`, header
[`ffi/include/openapi_validator.h`](ffi/include/openapi_validator.h)) and a
Go package (`ffi/go`) wrap the same calls.

## What it validates

- **Documents:** OpenAPI 3.0.x and 3.1.x, JSON or YAML 1.2 (`yes`, `no`, `on`
  are strings). External `$ref`s are refused at load with a clear error;
  nothing is ever fetched.
- **Routing:** server paths (whole segments, variables from `enum` or
  `default`, path-item and operation `servers`), path templates (each
  segment decoded on its own, so `%2F` stays inside it; concrete paths
  before templates; most literal characters wins), case-sensitive methods,
  HEAD answered by GET.
- **Parameters** in path, query, header and cookie: every `style` and
  `explode` combination (form, simple, label, matrix, spaceDelimited,
  pipeDelimited, deepObject), `content`-described parameters, path-item
  overrides, repeated header lines, exploded arrays and objects, and
  optional refusal of undeclared query parameters, headers and cookies.
- **Security:** AND within a requirement, OR across requirements, `{}` and
  `security: []`, `apiKey` in query, header or cookie, `http` schemes as
  whole tokens, bearer tokens for `oauth2` and `openIdConnect`; presence
  only, with a hook for your own check.
- **Bodies:** JSON (strict) and `+json`, `application/x-www-form-urlencoded`
  with Encoding Objects, `multipart/form-data`, `text/*`, XML
  well-formedness; media ranges and charsets; required and empty bodies;
  size limits.
- **Responses:** exact status, then `2XX`-style ranges, then `default`;
  headers as literal text or `content`; bodies, with HEAD, 204 and 304
  carrying none; a response to an undeclared operation is a problem.
- **Schemas:** 3.0's Schema Object (`nullable`, boolean exclusive bounds,
  `$ref` siblings ignored, integers written without fraction or exponent)
  and 3.1's JSON Schema 2020-12 (`$ref` siblings applied, `$id`, anchors,
  `unevaluated*`, boolean schemas); readOnly/writeOnly by direction; the
  discriminator enforced; exact numbers; ECMA-262 patterns in linear time;
  asserted formats (date, time, date-time, email, uuid, uri, uri-reference,
  ipv4, ipv6, hostname, int32, int64).

Every problem has a kind (`invalid_parameter`, `schema_violation`, ...), a
location (`query.limit`, `header.x-tag`, `body/items/3`) and a one-sentence
message. Reports stop at `max_problems` (20) or `max_report_bytes` (8 KiB)
and say "and N more".

## How it stays safe

- **Verdicts and explanations are separate.** Schemas are translated to
  JSON Schema 2020-12 and compiled lazily, once per schema root, by the
  [`jsonschema`](https://crates.io/crates/jsonschema) crate, which is asked
  only "valid or not". Its error collection explores every failing branch
  of every `anyOf`/`oneOf`/`allOf` and is exponential on recursive schemas
  (65,536 errors for a 16-level recursive `allOf`), so when a value is
  invalid a separate pass walks schema and value together, memoised and
  step-bounded, asking `jsonschema` for branch verdicts instead of
  descending into every branch. The explanation never changes the verdict.
- **Linear-time patterns.** ECMA-262 patterns are translated for the
  `regex` crate (ASCII `\d\w\b`, ECMA `\s`, `.` without line terminators,
  `\cX`, `\uXXXX`); lookahead, lookbehind and backreferences are refused at
  load with an error naming the pattern.
- **Exact numbers.** `serde_json` with `arbitrary_precision`, compared as
  exact decimals: `int64` bounds, values past 2^53, `multipleOf` and `enum`.
- **Strict JSON.** Duplicate keys, invalid UTF-8 and lone surrogate escapes
  are refused (parser differentials an attacker can exploit when the gateway
  and the service read the same body differently); one leading BOM is
  accepted.
- **Bounded recursion and work.** Documents nest at most 128 levels;
  chains of in-place applicators (`$ref`, `allOf`, `anyOf`, ...) at most 64
  deep and 100,000 subschemas wide, measured at load by an iterative walk;
  bodies at most 128 levels, lowered per document so body depth times
  schema depth stays within a fixed budget. The worst documents the loader
  accepts validate on a **256 KiB** stack in release builds
  (`examples/stack_probe.rs`).
- **No panics across the API.** `Spec::load`, `validate_request` and
  `validate_response` catch panics and report them; every C entry point
  does the same and returns an error code.

## Behaviour choices

Each is listed with its reason and section in
[`SPEC_DECISIONS.md`](SPEC_DECISIONS.md); the rule is *never refuse a valid
call*, and an option where reasonable people differ. The main ones:

| Choice | Section |
|---|---|
| Path segments are percent-decoded one at a time; `%2F` never splits a segment | RFC 3986 §2.2; OAS §4.8.2.2 |
| Concrete paths beat templates; then most literal characters; same-shape templates are a load error | OAS §4.8.8.2 |
| Methods are case-sensitive; HEAD without `head` is checked as GET | RFC 9110 §9.1, §9.3.2 |
| Server paths stripped as whole segments; with or without them by default (`server_paths`); operation `servers` replace the path item's | OAS §4.8.5, §4.8.10.1 |
| Header parameters named Accept, Content-Type or Authorization are ignored | OAS §4.8.12.2 |
| Text becomes a number or boolean only where the schema allows it; a value is valid if any reading is | OAS §4.8.12 (parsing unspecified) |
| Query strings split only on `&`; `+` is a space (`query_plus_as_space`); bad escapes are reported; no pair limit | RFC 3986 §3.4 |
| `list=` is an empty array; exploded objects take only their declared properties | OAS §4.8.12.4 |
| Every line of a repeated header is checked | RFC 9110 §5.3 |
| Undeclared-header mode treats Accept, Content-Type, Authorization, Cookie, the security schemes' headers and transport headers as declared (configurable) | — |
| Security is presence only; `http` schemes match a whole, case-insensitive token and need a credential | OAS §4.8.27; RFC 9110 §11.4; RFC 6750 §2.1 |
| 3.0 `nullable` needs `type` beside it, respects `enum`, and never rewrites `allOf`/`oneOf` | OAS 3.0.4 §4.8.24.1 |
| 3.0 ignores keywords beside `$ref`; 3.1 applies them | OAS 3.0.4 §4.8.23; JSON Schema 2020-12 §8.2.3.1 |
| Integers: lexical in 3.0, mathematical in 3.1 | Wright draft 00; 2020-12 §6.1.1 |
| readOnly/writeOnly drop out of `required` per direction, through `$ref`; sending them is allowed | OAS §4.8.24.4 |
| A discriminator value must be mapped and selects the branch to check | OAS §4.8.25 |
| Count keywords written as `2.0` or `1.79e308` are counts; negative ones are ignored with a warning | 2020-12 §6.2–6.4 |
| Unknown formats are annotations | 2020-12 §7.2 |
| An empty response body where content is declared is a problem, except HEAD, 204 and 304 | RFC 9110 §15.3.5, §15.4.5 |
| Response header values are literal text in simple style, never re-read as JSON | OAS §4.8.21 |
| XML bodies are checked for well-formedness only | OAS §4.8.26 |

## Not supported

- Swagger 2.0 and OpenAPI 3.2 documents.
- External references (other files or URLs); bundle the document first.
  References to the JSON Schema metaschemas are allowed (they are built in).
- Checking XML bodies against schemas (the XML Object mapping).
- IDNA rules for `hostname` (A-labels are checked as letters, digits and
  hyphens only) and the formats not listed above (`duration`, `iri`,
  `uri-template`, `regex`, ...), which are annotations.
- Credential verification beyond presence (use `security_hook`).
- Callbacks, webhooks and links (never validated).
- `Content-Encoding` (compressed bodies are not decoded).
- `readOnly`/`writeOnly` in a `required` list of one `allOf` member when the
  property is declared in a different, `$ref`-ed member.
- Precise problem locations inside `$dynamicRef` targets and `$id`-scoped
  references (the verdict is exact; the problem is reported at the
  enclosing schema).

## Correctness over speed

Where the two pulled apart, correctness won:

- Exact decimal comparison instead of `f64`.
- Every repeated header line, every query value and every multipart part is
  checked, not just the first.
- `enum`, `const` and `uniqueItems` use JSON Schema equality (numbers by
  value, objects regardless of key order) instead of the engine's faster
  built-ins.
- Parameters that admit several types are tried under each reading.
- An invalid value costs a second, explanation pass (still bounded).

## Benchmarks

Apple M-series laptop, release build, `cargo bench` and
`cargo run --release --example build_cost`. The synthetic document
(`benches/support/synthetic.rs`) has 2,000 path templates, each with GET
(query, path and header parameters, a `$ref` response) and POST (a body
through a 4-deep `$ref` chain), and 800 component schemas.

| Measure | Target | Result |
|---|---|---|
| Build, 1.6 MiB / 1,200 paths | < 200 ms, < 50 MB | **61 ms**, 26 MiB retained |
| Build, 2.4 MiB / 2,000 paths | | **97 ms**, 39 MiB retained (72 MiB peak) |
| First call to an operation (compiles its schemas) | | 1.7 ms |
| Warm call: GET with three parameters | | 1.6 µs |
| Warm call: POST, 4-level JSON body, valid | | 6.6 µs |
| Warm call: same body, invalid (with explanation) | | 26 µs |
| 16-level recursive `anyOf` body, valid / invalid | < 1 ms | **4.8 µs / 8.9 µs** |
| Backtracking-prone pattern `^(a+)+$` on 50,000 characters | linear | 47 µs |
| `uniqueItems` over 10,000 items | | 1.8 ms |
| 127-level nested JSON body | | 4.9 µs |
| 5,000 query pairs | | 0.8 ms |

Retained memory grows about 16 MiB per MiB of document, almost all of it
the translated schemas held by the engine.

## Tests

| Suite | Result |
|---|---|
| Unit and integration tests (`cargo test`) | 146 test functions, all passing |
| Conformance suite, `tests/cases/` (one YAML case per file, written from the specifications) | 949 of 949 |
| JSON-Schema-Test-Suite draft 2020-12, through the 3.1 path | 1,257 of 1,302 (96.5%); of the 45 failures, 44 need remote documents (refused by design) and 1 needs a custom metaschema vocabulary. Optional format tests: every asserted format passes except IDNA rules for `hostname` |
| Property tests (proptest): generated values validate, arbitrary bodies stay bounded | 3 properties × 1,000 cases |
| Fuzzing (cargo-fuzz: `load`, `validate_request`, `json`, `yaml`, `pattern`) | 2.5 hours, about 65 million executions, no crash; nightly in CI |
| Go cgo test (`ffi/go`) under `cgocheck2` and the race detector | passing |

The conformance suite runs with `cargo test --test conformance`; one case
with `CASE=<substring> cargo test --test conformance`. The
JSON-Schema-Test-Suite runs when `JSON_SCHEMA_TEST_SUITE` names a checkout.
`tests/corpus.rs` runs over a local directory of documents named by
`OAS_CORPUS` and is skipped otherwise.

## Building

```sh
cargo test                                   # library, conformance, properties
cargo bench                                  # criterion benches
cargo build --release -p openapi-validator-ffi   # libopenapi_validator_ffi.{a,so,dylib}
(cd ffi/go && go test ./...)                 # Go wrapper
(cd fuzz && cargo +nightly fuzz run load)    # fuzzing
```

Minimum supported Rust version: 1.88.

## License

Apache-2.0.
