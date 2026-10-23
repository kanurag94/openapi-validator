# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- `Options::strict_mode`, also in the C ABI, to refuse messages on readings the specifications leave open. The gateway profile turns it on.
- A lint warning for `example` and `examples` together on a parameter, media type or header, which the specifications make mutually exclusive.
- A lint warning for a 3.1 or 3.2 pattern with an escape, such as `\-` outside a class, that ECMA-262's Unicode mode does not allow.

### Changed

- A schema that many operations reach through `$ref` is compiled once and shared, instead of once per operation: Stripe's description peaks at 114 MB instead of 3.5 GB, and its first calls take 29 ms instead of 3 s.
- A request or response body without `Content-Type`, or of a media type its `content` does not describe, is no longer reported; it is passed without a schema check. Strict mode keeps the old behaviour.
- In OpenAPI 3.0, a `requestBody` on OPTIONS or TRACE is ignored, as on GET, HEAD and DELETE (3.0.4 §4.7.10.1).

### Fixed

- A number with an exponent beyond the 64-bit range, in a document or a body, no longer overflows while it is refused.
- A `multipart/form-data` part whose `Content-Disposition` type is not `form-data` is reported as malformed (RFC 7578 §4.2).
- An invalid percent-escape in an undeclared query parameter is reported (RFC 3986 §2.1).
- A path parameter whose name the template repeats is checked at every place, not only the first.
- `date-time` refuses a leap second that does not end a month in UTC (RFC 3339 §5.7).
- `email` reads address literals by RFC 5321 §4.1.3: at most six IPv6 groups beside `::`, and IPv4 numbers with leading zeros.
- OpenAPI 3.0 patterns match UTF-16 code units, as ECMA-262 does without the `u` flag, so `^.$` no longer matches a character outside the Basic Multilingual Plane.

## [0.2.0] - 2026-10-22

### Added

- OpenAPI 3.2 documents: the `query` method, `additionalOperations`, `$self`, `components.mediaTypes`, `in: querystring` and `style: cookie` parameters, sequential media types with `itemSchema`, positional multipart with `prefixEncoding` and `itemEncoding`, discriminator `defaultMapping`, and XML `nodeType`.
- `validate_webhook_request`, `validate_webhook_response`, `validate_callback_request` and `validate_callback_response` for operations the API calls, also in the C ABI and Go.
- `Spec::load_documents` for descriptions spread over several documents, also in the C ABI and Go.
- XML bodies are checked against their schemas through the XML Object.
- Bodies with `Content-Encoding` gzip, deflate or br are decoded before they are checked.
- Multipart parts are checked against their Encoding Objects' content types and headers.
- Every JSON Schema 2020-12 format, and OpenAPI's byte, float and double.
- Lint warnings for documents that break a MUST, listed by `Spec::warnings()`, and `Options::strict_document` to refuse them.
- `Options::gateway_profile()`, which switches on the stricter choices the defaults no longer make.
- Swagger 2.0 documents, read as the OpenAPI 3.0 they convert to.
- OpenAPI 3.2.1 and 3.1.2: form-data read as ordered named parts when its schema is an array, XML nulls in 3.1, and their document rules.
- `ReadWritePresence`, to refuse readOnly properties in requests and writeOnly ones in responses (in the gateway profile).
- Exploded map query parameters, which take every pair no other parameter names.
- Lint for 3.1 and 3.2 schemas against the JSON Schema 2020-12 meta-schema, unique tag names, 3.2 tag parents, repeated template expressions and server URLs with a query.
- Reports for repeated `Authorization` and `Content-Type` lines, and for content in responses to HEAD and in 204 and 304 responses.

### Changed

- Defaults follow the specification text: formats are annotations in 3.1 and 3.2, discriminators are annotations, 3.0 integers are mathematical, and readOnly/writeOnly relax `required` in 3.0 only. Each earlier choice is an option.
- Lookaround and backreference patterns run on a bounded engine instead of refusing the document; `PatternEngine::LinearOnly` keeps the old behaviour.
- YAML `<<` keys merge mappings by default; `Options::yaml_merge_keys` turns this off.
- A server variable without `enum` matches any value.
- `Set-Cookie` response headers are read one value per line, and never split on commas.
- A header sent on several lines is one comma-joined value (RFC 9110 §5.3), so a scalar sent twice is refused.
- Templates that differ only in parameter names load as one path with a warning instead of refusing the document.
- OpenAPI 3.0 patterns follow ECMA-262 Annex B (no `u` flag), so `\A` is the letter A; 3.1 and 3.2 keep the `u` flag.
- OpenAPI 3.0 request bodies on GET, HEAD and DELETE are ignored, as 3.0.4 says.
- In 3.1 and later, an Encoding Object's `style` and `explode` apply to multipart parts.
- `Version` has a `V32` variant and is `#[non_exhaustive]`.
- C ABI: `oav_options` has a new last field, `read_write_presence`; earlier fields keep their offsets. Callers that build `oav_options` themselves rather than through `oav_options_default` must set it.
- `MatchedOperation::method` is an `additionalOperations` key, as written, for those operations.

## [0.1.0] - 2026-10-21

### Added

- `Spec::load` for OpenAPI 3.0.x and 3.1.x documents in JSON or YAML 1.2.
- `validate_request` and `validate_response`, returning a bounded report of problems.
- Routing by server path, path template and method, with HEAD served by GET.
- Parameter checks for every style and explode combination, including `content` parameters.
- Credential presence checks for `apiKey`, `http`, `oauth2` and `openIdConnect` schemes, with a hook.
- Body checks for JSON, form, multipart and text, and well-formedness checks for XML.
- Response checks for status, headers and bodies.
- Schema support for OpenAPI 3.0 and 3.1, readOnly/writeOnly, discriminators, exact numbers and linear-time patterns.
- Load-time limits that bound validation time and stack use.
- A C ABI crate, `openapi-validator-ffi`, and a Go wrapper.
- A conformance suite of 949 cases, a JSON-Schema-Test-Suite runner, property tests, fuzz targets and benchmarks.

[Unreleased]: https://github.com/kanurag94/openapi-validator/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/kanurag94/openapi-validator/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/kanurag94/openapi-validator/releases/tag/v0.1.0
