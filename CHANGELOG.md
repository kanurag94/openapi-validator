# Changelog

All notable changes to this project are documented here. The format
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the
project uses [Semantic Versioning](https://semver.org/).

## [0.1.0] - 2026-10-21

First release.

### Added

- `Spec::load` for OpenAPI 3.0.x and 3.1.x documents in JSON or YAML 1.2,
  with load warnings and clear errors (external references, backtracking
  patterns, duplicate path templates, undefined security schemes, limits).
- `Spec::validate_request` and `Spec::validate_response`, returning a
  bounded `Report` of problems, each with a kind, a location and a message.
- Routing by server path (whole segments, variables, path-item and
  operation `servers`), path template (segment-wise percent-decoding,
  most-specific match) and case-sensitive method, with HEAD served by GET.
- Parameters in path, query, header and cookie, in every style and explode
  combination, including `content`-described parameters and the
  unknown-parameter options.
- Security requirement checks for credential presence (`apiKey`, `http`,
  `oauth2`, `openIdConnect`), with a caller hook.
- Bodies: JSON (strict), `+json`, form-urlencoded with Encoding Objects,
  multipart/form-data, `text/*`, and XML well-formedness; media ranges and
  charsets.
- Responses: status lookup (exact, range, `default`), headers and bodies.
- Schema translation for 3.0 (`nullable`, boolean exclusive bounds, `$ref`
  siblings ignored, lexical integers) and 3.1 (JSON Schema 2020-12),
  readOnly/writeOnly by direction, discriminator enforcement, exact numbers,
  ECMA-262 patterns on a linear-time engine, and asserted formats.
- Bounded cost: lazy per-operation compilation, verdicts from `jsonschema`
  with a memoised, budgeted explanation pass, and load-time limits on
  in-place schema depth and fan-out so validation fits a 256 KiB stack.
- C ABI crate `openapi-validator-ffi` with a documented header, and a Go
  cgo package under `ffi/go`.
- Conformance suite (949 YAML cases), JSON-Schema-Test-Suite runner,
  property tests, cargo-fuzz targets, criterion benches, and an optional
  local corpus test.
