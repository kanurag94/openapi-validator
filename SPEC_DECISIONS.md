# Specification decisions

This document lists where OpenAPI, JSON Schema or the HTTP RFCs are silent, say SHOULD, or leave a choice open, and what this library does in each case.

The guiding rule is to never refuse a valid call. Where reasonable people differ, the behaviour is an option. Section numbers refer to OpenAPI 3.1.1 unless stated otherwise. OpenAPI 3.0.4 numbers the same objects one level lower: the Parameter Object is §4.8.12 in 3.1.1 and §4.7.12 in 3.0.4.

The defaults follow the specification text. `Options::gateway_profile()` switches on stricter, gateway-oriented choices that go beyond it; each is noted below.

## Routing

- **Trailing slashes.** The specification is silent. A path ending in `/` that matches no template is retried without the slash.
- **Path variables.** A variable matches one or more characters within a single segment. Segments are percent-decoded one at a time, so `%2F` never creates a new segment (RFC 3986 §2.2).
- **Several variables in one segment.** Each variable takes the shortest run that lets the next literal follow, and a final literal must end the segment.
- **Ties between templates.** §4.8.8.2 only says concrete paths beat templated ones. Among templates, the one with the most literal characters wins, then the one whose first differing segment is literal.
- **Server paths.** The option `server_paths` decides whether the request path may (`either`, the default), must (`required`) or must not (`ignored`) start with a server path. Server paths are stripped as whole segments only.
- **Operation servers.** Following §4.8.10.1, the server path used must serve the matched operation itself, not just a sibling operation on the same path item.
- **Server variables without `enum`.** The value is open, so any segment matches; `default` is only the value used when none is supplied (§4.8.6.1). A missing `default` is a load warning.
- **Relative server URLs.** A relative URL such as `v1` is read as `/v1`, because there is no document URL at run time.
- **Method case.** Methods are case-sensitive (RFC 9110 §9.1): `GET` matches the `get` operation and `get` does not.
- **HEAD requests.** Without a `head` operation, HEAD is checked as GET (RFC 9110 §9.3.2). A response to HEAD must not carry content.
- **Unexpected request bodies.** A body sent to an operation without a `requestBody` is not reported.

## Parameters

- **Typed values.** §4.8.12 defines serialization, not parsing. Text becomes a number only if it is a JSON number literal, and a boolean only if it is `true` or `false`. When a schema allows several types, the value is valid if any reading is valid.
- **Plus signs in queries.** RFC 3986 gives `+` no meaning, but browsers send it for a space. It is read as a space by default; the option `query_plus_as_space` turns this off.
- **Query separators.** Query strings are split on `&` only, so `;` stays inside values (RFC 3986 §3.4). An invalid percent-escape is reported, and there is no limit on the number of pairs.
- **Exploded objects.** Only the schema's declared properties are taken from the query, as in the §4.8.12.4 style table. An object with none of them present is absent.
- **Nested values in exploded objects.** RFC 6570 leaves these undefined. Repeated pairs form an array, and an object may be sent as JSON text or as `name[key]=value` pairs.
- **Nested deepObject keys.** §4.8.12.4 defines one level. Deeper keys such as `a[b][c]=v` nest, and `a[]=v` appends to an array.
- **Empty arrays.** `list=` is an empty array in both explode modes.
- **Repeated parameters.** A non-exploded array sent twice has its items combined. A primitive sent twice has each value checked.
- **Header lists.** Items in list-valued headers are trimmed, so `a, b` is accepted (RFC 9110 §5.6.1).
- **Repeated header lines.** Every line is checked on its own (RFC 9110 §5.3).
- **Cookie values.** Values are percent-decoded when the escapes are valid and taken as written otherwise (RFC 6265).
- **`allowEmptyValue`.** The field is deprecated and changes nothing; the schema decides whether an empty value is valid.
- **Documents that break a parameter MUST.** These load with a warning instead of being refused. With both `schema` and `content`, `schema` is used. With several `content` entries, the first is used. With duplicate parameters, the last is used. A path parameter with `required: false` is treated as required.
- **Undeclared headers.** When `allow_unknown_headers` is false, `Accept`, `Content-Type`, `Authorization`, `Cookie` and the security schemes' header names always count as declared, as do the names in `ignored_headers` (by default `Host`, `Content-Length`, `Transfer-Encoding`, `Connection` and `User-Agent`).
- **Content parameters without a schema.** These are unconstrained and not parsed.

## Security

- **Scope.** Only the presence of credentials is checked (§4.8.27). The `security_hook` option can verify them.
- **HTTP schemes.** The `Authorization` header must start with the scheme as a whole, case-insensitive token, followed by whitespace and a non-empty credential (RFC 9110 §11.4).
- **Repeated single-value headers.** `Authorization` and `Content-Type` sent on more than one line are reported, because they take one value and a sender MUST NOT repeat them (RFC 9110 §5.3, §8.3, §11.6.2).
- **OAuth 2 and OpenID Connect.** These need `Authorization: Bearer <token>` (RFC 6750 §2.1).
- **Mutual TLS and unknown scheme types.** These are never refused, because they are not visible in the HTTP message.
- **Undefined scheme names.** A requirement naming an undefined scheme is a load error (§4.8.30.1).
- **Incomplete API key schemes.** An `apiKey` scheme without `in` or `name` loads with a warning and is not checked.

## Bodies

- **Missing Content-Type.** If the operation declares exactly one media type that is not a range, it is assumed (RFC 9110 §8.3). Otherwise the problem is reported.
- **Media type matching.** The most specific key wins: `type/subtype`, then `type/*`, then `*/*`. Parameters such as `charset` do not prevent a match.
- **Parsing.** Bodies are parsed by their actual media type: JSON (including `+json`, RFC 6839), form, multipart, XML and `text/*`. Other media types are not parsed.
- **XML.** Only well-formedness is checked (§4.8.26). Document type definitions are allowed, and nesting is bounded.
- **Text charsets.** UTF-8, US-ASCII and ISO-8859-1 are supported; other charsets are reported as malformed.
- **Undeclared form fields.** They are kept as properties of the body, so `additionalProperties` decides whether they are allowed.
- **Reserved characters in form values.** Unencoded reserved characters are accepted even when `allowReserved` is false, since the specification only says they SHOULD be encoded.
- **Multipart parts.** Following §4.8.15.1, object properties default to JSON and arrays take their items' type. File parts are read as text, and line breaks without a carriage return are accepted.
- **Empty optional bodies.** These are accepted whatever the `Content-Type` (§4.8.13).
- **Size.** Bodies larger than `max_body_bytes` (16 MiB by default) are reported and not parsed.

## Responses

- **Status lookup.** The exact code is tried first, then a range such as `2XX` in any case, then `default` (§4.8.16.2).
- **Empty bodies.** An empty body where content is declared is reported, except for HEAD requests and 204 and 304 responses (RFC 9110 §15.3.5, §15.4.5).
- **Content where none is allowed.** A response to HEAD, or a 204 or 304 response, that carries content is reported (RFC 9110 §9.3.2, §15.3.5, §15.4.5).
- **Undeclared bodies.** A body where no content is declared is not reported.
- **Header values.** Values are literal text in simple style and are never re-read as JSON (§4.8.21). Each repeated line is checked.
- **Content-Type headers.** A declared `Content-Type` response header is ignored (§4.8.17.1).

## Schemas

- **3.0 `nullable`.** It applies only when `type` is in the same schema object, `enum` must still list `null`, and it is never pushed into `allOf` or `oneOf` (3.0.4 §4.7.24.1).
- **Unknown keywords in 3.0.** Keywords that are not part of the 3.0 Schema Object, such as `const` or `if`, are ignored.
- **Integers.** Any number with a zero fraction is an integer, so `1.0` and `1e2` are integers, in 3.0 (3.0.4 §4.4) and 3.1 (JSON Schema 2020-12 §6.1.1). The option `integers: LexicalIn30` applies the older 3.0.3 reading, where an integer is written without a fraction or exponent (gateway profile).
- **Count keywords written as numbers.** Values such as `2.0` are counts. Beyond the 64-bit range, a maximum is unbounded and a minimum is unreachable. Negative or fractional counts are dropped with a warning.
- **Formats.** Every JSON Schema 2020-12 format (§7.3) and OpenAPI's int32, int64, float, double and byte are implemented. In 3.1 they are annotations by default, as the format-annotation vocabulary requires (§7.2.1). In 3.0 they are asserted, which 3.0 allows. The option `formats` (`Always`, `Never`) overrides this; the gateway profile asserts them always. Unknown formats are always annotations.
- **Host names.** `hostname` labels contain only letters, digits and hyphens (RFC 1123), and `xn--` labels must decode to valid IDNA2008 labels. `idn-hostname` follows UTS 46 processing (non-transitional, strict) plus the RFC 5892 contextual rules.
- **Patterns.** Patterns are translated from ECMA-262 to the linear-time `regex` crate, keeping ECMA meanings. Lookaround and backreferences are a load error that names the pattern.
- **Discriminator.** By default it is an annotation: it "MUST NOT change the validation outcome" (§4.8.25), so `oneOf` and `anyOf` decide alone and an allOf-style base never searches its children. The option `discriminator: Enforce` requires the value to be a mapped name and checks the payload against that schema alone (gateway profile).
- **Discriminator cycles.** When a base lists a child in `oneOf` or `anyOf` and the child extends the base through `allOf`, the child's reference uses a copy of the base without that `oneOf`/`anyOf`, so the schema does not refer to itself without end.
- **readOnly and writeOnly.** In 3.0, a required readOnly property is required in responses only and a writeOnly one in requests only (3.0.4 §4.7.24.1), including through `$ref` and `allOf`. In 3.1 they are annotations and `required` applies in both directions (§4.8.24.3.2); the option `read_write_required: RelaxAlways` applies the 3.0 rule (gateway profile). Sending them is never refused.
- **Equality.** `enum`, `const` and `uniqueItems` compare numbers by value and ignore the key order of objects (JSON Schema 2020-12 §4.2.2).
- **Fragment references in 3.1.** A `#` reference in a schema without `$id` resolves against the OpenAPI document root.
- **Metaschema references.** References to the JSON Schema metaschemas are allowed, because they are built in and nothing is fetched.

## Parsing and limits

- **JSON.** Duplicate keys, invalid UTF-8 and lone surrogate escapes are refused. RFC 8259 §4 leaves duplicate names to the parser, so parsers disagree on them; I-JSON (RFC 7493 §2.3, §2.1) forbids them, and refusing them closes a parser differential between gateway and service. One leading byte order mark is ignored (RFC 8259 §8.1).
- **Large numbers.** Numbers with an exponent beyond ±4096 or more than 4096 digits are refused, so exact comparison stays bounded.
- **Nesting.** Documents may nest 128 levels. Bodies may nest `max_body_depth` levels (128 by default), lowered per document so that body depth multiplied by schema depth stays within a fixed stack budget.
- **In-place schema chains.** At most 64 nested `$ref`, `allOf`, `anyOf` and similar keywords, and at most 100,000 subschemas, may apply to one value. Larger documents are refused at load, because the schema engine would take exponential time.
- **YAML.** YAML 1.2 core schema rules apply, so `yes` and `on` are strings. Duplicate keys, multiple documents and alias expansion beyond 4 million nodes are refused. `<<` is an ordinary key, and `.inf` and `.nan` become strings.
- **External references.** These are a load error; the library never reads files or the network.
- **Reports.** A report keeps at most `max_problems` (20) problems and `max_report_bytes` (8 KiB) of text, and counts the rest.
- **Failed `anyOf` and `oneOf`.** The problem is reported at the `anyOf` or `oneOf` itself, or inside the one branch whose `type` fits the value.
