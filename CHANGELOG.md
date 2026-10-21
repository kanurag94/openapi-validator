# Changelog

Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). Versioning: [SemVer](https://semver.org/).

## [0.1.0] - 2026-10-21

First release.

### Added

- `Spec::load` for OpenAPI 3.0.x and 3.1.x, JSON or YAML 1.2.
- `validate_request` and `validate_response`, returning a bounded `Report`.
- Routing: servers, path templates, methods, HEAD as GET.
- Parameters: every style and explode combination, `content` parameters, unknown-parameter options.
- Security: presence checks for `apiKey`, `http`, `oauth2`, `openIdConnect`, plus a hook.
- Bodies: JSON, `+json`, form, multipart, text, XML well-formedness.
- Responses: status lookup, headers, bodies.
- Schemas: 3.0 and 3.1 translation, readOnly/writeOnly, discriminator, exact numbers, linear-time patterns, formats.
- Limits: lazy compilation, bounded explanations, load-time depth and fan-out checks.
- C ABI crate `openapi-validator-ffi` and a Go wrapper.
- Tests: 949 conformance cases, JSON-Schema-Test-Suite runner, property tests, fuzz targets, benches, optional corpus test.
