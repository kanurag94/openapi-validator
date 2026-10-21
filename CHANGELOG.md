# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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

[Unreleased]: https://github.com/kanurag94/openapi-validator/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/kanurag94/openapi-validator/releases/tag/v0.1.0
