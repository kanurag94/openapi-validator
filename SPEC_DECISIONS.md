# Specification decisions

Where the specs are silent, say SHOULD, or leave a choice open.

- Rule: never refuse a valid call.
- Where reasonable people differ, there is an option.
- Sections are OpenAPI 3.1.1 unless noted; 3.0.4 numbers match.

## Routing

- **Trailing slash** (silent)
  - No match and path ends in `/`: retried without it.
- **Path variables** (§4.8.2.2)
  - Match one or more characters inside one segment.
  - Segments are decoded one at a time; `%2F` never splits a segment (RFC 3986 §2.2).
- **Several variables in one segment** (silent)
  - Each takes the shortest run before the next literal; a final literal must end the segment.
- **Ties between templates** (§4.8.8.2 covers only concrete vs templated)
  - Most literal characters wins (`/f/{id}.json` over `/f/{id}`).
  - Then the template whose first differing segment is literal.
- **Server path in front of the request path** (§4.8.5)
  - Option `server_paths`: `either` (default), `required`, `ignored`.
  - Stripped by whole segments only.
- **Operation and path-item `servers`** (§4.8.9.1, §4.8.10.1)
  - The stripped server path must serve the matched operation itself.
- **Server variable without `enum` or `default`**
  - Substituted as empty, with a load warning.
- **Relative server URL like `v1`**
  - Read as `/v1`.
- **Method case** (RFC 9110 §9.1)
  - Case-sensitive: `GET` matches `get`; `get` does not.
- **HEAD without a `head` operation** (RFC 9110 §9.3.2)
  - Checked as GET; the response body is skipped.
- **Body sent to an operation without `requestBody`** (silent)
  - Not a problem.

## Parameters

- **Text to typed values** (§4.8.12 defines serialization only)
  - Numbers only from JSON number literals (`05` is not a number).
  - Booleans only from `true` / `false`.
  - Several allowed types: valid if any reading is valid.
- **`+` in query strings** (RFC 3986 gives it no meaning)
  - A space by default; option `query_plus_as_space`.
- **Query separators** (RFC 3986 §3.4)
  - Split on `&` only; `;` stays in the value.
  - Bad percent-escapes are `malformed_query`.
  - No limit on pairs.
- **Exploded `form` objects** (§4.8.12.4)
  - Only declared properties are taken; none present means absent.
- **Nested values in exploded objects** (RFC 6570: undefined)
  - Repeated pairs make an array.
  - An object may come as JSON text or `name[key]=value`.
- **`deepObject` beyond one level** (§4.8.12.4 defines one)
  - `a[b][c]=v` nests; `a[]=v` appends.
- **`list=`**
  - An empty array, in both explode modes.
- **Repeats**
  - Non-exploded array sent twice: items concatenated.
  - Primitive sent twice: each value checked.
- **Header lists** (RFC 9110 §5.6.1)
  - Items are trimmed (`a, b` works).
- **Repeated header lines** (RFC 9110 §5.3)
  - Each line is checked on its own.
- **Cookie values** (RFC 6265)
  - Percent-decoded when valid, else taken as written.
- **`allowEmptyValue`** (deprecated)
  - Changes nothing; the schema decides.
- **Broken MUSTs** (§4.8.10.1, §4.8.12.2) — load with a warning:
  - `schema` and `content` both: `schema` is used.
  - Several `content` entries: the first is used.
  - Duplicate parameters: the last is used.
  - Path parameter with `required: false`: treated as required.
- **Undeclared headers** (`allow_unknown_headers: false`)
  - Always declared: Accept, Content-Type, Authorization, Cookie, security header names.
  - Plus `ignored_headers`: Host, Content-Length, Transfer-Encoding, Connection, User-Agent.
- **`content` parameter without a schema**
  - Unconstrained; not parsed.

## Security

- **Scope** (§4.8.27)
  - Presence only; `security_hook` can verify.
- **`http` schemes** (RFC 9110 §11.4)
  - Whole, case-insensitive token, then whitespace, then a non-empty credential.
  - Any `Authorization` line may satisfy it.
- **`oauth2`, `openIdConnect`** (RFC 6750 §2.1)
  - Need `Authorization: Bearer <token>`.
- **`mutualTLS`, unknown types**
  - Never refused (not visible in HTTP).
- **Undefined scheme name** (§4.8.30.2)
  - Load error.
- **`apiKey` missing `in` or `name`**
  - Load warning; not checked.

## Bodies

- **No `Content-Type`** (RFC 9110 §8.3)
  - One declared, non-range type: assumed.
  - Otherwise `unsupported_media_type`.
- **Media type matching** (RFC 9110 §12.5.1)
  - Most specific key wins: `type/sub`, then `type/*`, then `*/*`.
  - Parameters such as `charset` do not block a match.
- **Parsing**
  - By the request's actual type: JSON and `+json` (RFC 6839), form, multipart, XML, `text/*`.
  - Other types are not parsed.
- **XML** (§4.8.26)
  - Well-formedness only; DTDs allowed, nesting bounded.
- **Text charsets**
  - UTF-8 (default), US-ASCII, ISO-8859-1; others are `malformed_body`.
- **Form fields not in the schema**
  - Kept as properties, so `additionalProperties` decides.
- **`allowReserved: false`** (SHOULD encode)
  - Unencoded reserved characters are accepted.
- **Multipart parts** (§4.8.15.1)
  - Objects default to JSON; arrays take their items' type.
  - Files are read as text, lossily.
  - LF-only line breaks accepted.
- **Empty optional body** (§4.8.13)
  - Fine, whatever the `Content-Type`.
- **Size**
  - Over `max_body_bytes` (16 MiB): `body_too_large`, not parsed.

## Responses

- **Status lookup** (§4.8.16.2)
  - Exact code, then `1XX`–`5XX` (any case), then `default`.
- **Empty body with declared content**
  - `missing_body`, except HEAD, 204, 304 (RFC 9110 §15.3.5, §15.4.5).
- **Body with no declared content**
  - Not a problem.
- **Header values** (§4.8.21)
  - Literal text in simple style, never re-read as JSON.
  - Each repeated line checked.
- **`Content-Type` header definition** (§4.8.17.1)
  - Ignored.

## Schemas

- **3.0 `nullable`** (3.0.4 §4.8.24.1)
  - Needs `type` in the same object.
  - `enum` must list `null` for it to pass.
  - Never pushed into `allOf` / `oneOf`.
- **Non-3.0 keywords in 3.0 documents**
  - Ignored (`const`, `if`, `prefixItems`, ...).
- **`integer`**
  - 3.0: written without fraction or exponent (`1.0` fails).
  - 3.1: any number with zero fraction (`1.0` passes; 2020-12 §6.1.1).
- **Count keywords** (2020-12 §6.2–6.4)
  - `2.0`, `1e0` are counts.
  - Past `u64`: maximum unbounded, minimum unreachable.
  - Negative or fractional: dropped with a warning.
- **Formats** (2020-12 §7.2)
  - Asserted: date, time, date-time, email, uuid, uri, uri-reference, ipv4, ipv6, hostname, int32, int64.
  - All others are annotations; option `assert_formats`.
- **`hostname`** (RFC 1123, RFC 5891)
  - Letters, digits, hyphens per label; Punycode is not decoded.
- **Patterns** (2020-12 §6.3.3, ECMA-262)
  - Translated to the linear-time `regex` crate with ECMA meanings.
  - Lookaround and backreferences: load error naming the pattern.
- **Discriminator** (§4.8.25)
  - Property present: value must be mapped; that schema is checked.
  - All branches mapped: the named branch alone decides (option `discriminator_selects_branch`).
  - Property absent: plain `oneOf` / `anyOf`.
- **Discriminator inheritance cycles**
  - A child's `allOf` link to its base uses a copy without the dispatch.
- **readOnly / writeOnly** (§4.8.24.4)
  - Dropped from `required` in the matching direction, through `$ref` and same-schema `allOf`.
  - Sending them is allowed.
- **Equality** (2020-12 §4.2.2)
  - Numbers by value; objects ignore key order.
- **`#` refs in 3.1 schemas without `$id`**
  - Resolve against the OpenAPI document root.
- **JSON Schema metaschema refs**
  - Allowed (built in; nothing fetched).

## Parsing and limits

- **Duplicate JSON keys** (RFC 8259 §4): refused.
- **Invalid UTF-8, lone surrogates** (RFC 8259 §8): refused.
- **Byte order mark**: one leading BOM ignored.
- **Huge numbers**: exponent past ±4096 or over 4096 digits is `malformed_body`.
- **Nesting**
  - Documents: 128 levels.
  - Bodies: `max_body_depth` (128), lowered per document to fit the stack budget.
- **In-place schema chains**
  - At most 64 deep and 100,000 subschemas per value; else a load error.
- **YAML 1.2**
  - Core schema (`yes`, `on` are strings).
  - Refused: duplicate keys, several documents, alias expansion over 4 M nodes.
  - `<<` is an ordinary key; `.inf` / `.nan` become strings.
- **External references**: load error.
- **Reports**: `max_problems` (20), `max_report_bytes` (8 KiB), then "and N more".
- **Failed `anyOf` / `oneOf`**: reported there, or inside the one branch whose `type` fits.
