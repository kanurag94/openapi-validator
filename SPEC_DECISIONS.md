# Specification decisions

Where OpenAPI (3.0.4, 3.1.1), JSON Schema 2020-12 or the HTTP RFCs are
silent, say SHOULD, or leave a choice open, this library has to pick a
behaviour. The rule used throughout: **pick the behaviour that never
refuses a valid call**, and make it an option when reasonable people
differ. Each entry gives the section, the choice and the reason.

Sections are OpenAPI 3.1.1 unless stated otherwise; the same rules apply
to 3.0.4, whose section numbers match for the objects named here.

## Routing

| # | Topic | Spec | Decision | Why |
|---|---|---|---|---|
| R1 | Trailing slash | §4.8.8 is silent; RFC 3986 treats `/a/` and `/a` as different | When no template matches and the path ends in `/`, it is retried without it | A client adding a slash is common and harmless |
| R2 | What a path variable matches | §4.8.2.2 defines templating, not matching | One or more characters inside one segment, after decoding the segment alone; `%2F` never splits a segment | RFC 3986 §2.2: an encoded reserved character is data |
| R3 | Several variables in one segment | Silent | Each variable takes the shortest run that lets the next literal follow; a final literal must end the segment | Deterministic and linear |
| R4 | Ties between templates | §4.8.8.2 only says concrete beats templated | Most literal characters wins (`/f/{id}.json` over `/f/{id}`), then the template whose first differing segment is literal | The more specific template is the intended one |
| R5 | Server path in front of the request path | §4.8.5 defines URLs, not matching | Option `server_paths`: `either` (default: with or without), `required`, `ignored`. Stripping is by whole segments only | Gateways differ in whether they rewrite paths |
| R6 | Operation and path-item `servers` | §4.8.9.1, §4.8.10.1 | The stripped server path must serve the matched operation itself | Each level replaces the one above |
| R7 | Server variable with neither `enum` nor `default` | `default` is REQUIRED | Substituted as empty, with a load warning | The document still loads |
| R8 | Relative server URL without `/` (`v1`) | §4.8.5: relative to the document | Read as `/v1` | There is no document URL at run time |
| R9 | Method case | RFC 9110 §9.1: case-sensitive | `GET` matches `get`; `get` does not match | Following the RFC |
| R10 | HEAD without a `head` operation | RFC 9110 §9.3.2: HEAD is GET without content | Checked as the GET operation; the response body is not checked | Servers answer HEAD with GET's handler |
| R11 | A body sent to an operation with no `requestBody` | Silent | Not a problem | An undescribed body is not proof of an invalid call |

## Parameters

| # | Topic | Spec | Decision | Why |
|---|---|---|---|---|
| P1 | Turning text into typed values | §4.8.12 describes serialization, not parsing | Numbers only from JSON number literals (`05` is not a number), booleans only from `true`/`false`; when a schema allows several types, the value is valid if **any** reading is valid | Never refuses a value some reading accepts |
| P2 | `+` in query strings | RFC 3986 gives it no meaning; HTML forms and WHATWG URL read it as space | Space by default; option `query_plus_as_space` | Matches what browsers and most clients send |
| P3 | Query pair separators | RFC 3986 §3.4 | Only `&`; `;` stays inside the value; no limit on pairs; a bad percent-escape is a `malformed_query` problem | Old `;` splitting is a known parser differential |
| P4 | Exploded `form` objects | §4.8.12.4 table: properties become top-level pairs | Only the schema's declared properties are taken; an object with none present is absent | Other pairs belong to other parameters |
| P5 | Nested values in exploded objects | RFC 6570 §3.2.8: undefined | Repeated pairs make an array; an object may arrive as JSON text or `name[key]=value` | Accepts the common client encodings |
| P6 | `deepObject` beyond one level | §4.8.12.4 defines one level | `a[b][c]=v` nests, `a[]=v` appends to an array; values below the first level are typed by their text | Common client behaviour |
| P7 | `list=` | Silent | An empty array, in both explode modes | The only way to send an empty array |
| P8 | Repeated non-exploded array (`a=1,2&a=3`) | Silent | Items are concatenated | Never refuses |
| P9 | Repeated primitive (`n=1&n=2`) | Silent | Every value is checked | A bad repeat is still bad |
| P10 | Header list values | RFC 9110 §5.6.1 allows optional whitespace | Array items and object entries are trimmed | `a, b` is the usual spelling |
| P11 | Repeated header lines | RFC 9110 §5.3 allows combining | Every line is checked on its own | A bad second line must not hide |
| P12 | Cookie values | RFC 6265 leaves values opaque; OpenAPI `form` style percent-encodes | Percent-decoded when the escapes are valid, else taken as written | Never refuses |
| P13 | `allowEmptyValue` | Deprecated (§4.8.12.2) | Does not refuse or allow anything by itself; the empty value meets the schema | The schema already decides |
| P14 | `schema` and `content` both | MUST NOT | Load warning; `schema` is used | Refusing the document refuses every call |
| P15 | `content` with several entries | MUST have one | Load warning; the first entry is used | Same |
| P16 | Duplicate parameters in one list | MUST NOT | Load warning; the last one is used | Same |
| P17 | Path parameter with `required: false` | MUST be true | Load warning; treated as required | Same |
| P18 | Undeclared headers when `allow_unknown_headers` is false | Silent | `Accept`, `Content-Type`, `Authorization`, `Cookie`, the operation's security header names and the `ignored_headers` list (default `Host`, `Content-Length`, `Transfer-Encoding`, `Connection`, `User-Agent`) count as declared | Transport headers are not part of the API |
| P19 | A `content` parameter without a schema | Silent | Unconstrained, not even parsed | Nothing to check against |

## Security

| # | Topic | Spec | Decision | Why |
|---|---|---|---|---|
| S1 | What is checked | §4.8.27 describes schemes, not verification | Presence only; `Options::security_hook` can verify | Verification belongs to the gateway's auth layer |
| S2 | `http` schemes | RFC 9110 §11.4 | The auth-scheme must be a whole, case-insensitive token followed by whitespace and a non-empty credential; any `Authorization` line may satisfy it | `Bearerx abc` must not pass for `bearer` |
| S3 | `oauth2`, `openIdConnect` | RFC 6750 §2.1 | `Authorization: Bearer <token>` | The only form a gateway can see |
| S4 | `mutualTLS` and unknown types | Not visible in HTTP | Never refused (the hook can decide) | Cannot be checked from the message |
| S5 | Undefined scheme name in a requirement | §4.8.30.2 MUST correspond | Load error | The requirement cannot be understood |
| S6 | `apiKey` without `in` or `name` | REQUIRED fields | Load warning; not checked | Cannot be checked |

## Bodies

| # | Topic | Spec | Decision | Why |
|---|---|---|---|---|
| B1 | No `Content-Type` | RFC 9110 §8.3 lets the recipient assume | With one declared, non-range media type, that one is assumed; otherwise `unsupported_media_type` | Never refuses the unambiguous case |
| B2 | Media type parameters | §4.8.14 keys may carry parameters | `charset` and other parameters do not stop a match; the most specific key wins (`type/sub` > `type/*` > `*/*`) | RFC 9110 §12.5.1 ranking |
| B3 | How a body is parsed | Silent | By the request's actual media type: JSON for `application/json` and `+json` (RFC 6839), form, multipart, XML, `text/*`; others are not parsed | The bytes are in the sender's format |
| B4 | XML | §4.8.26 XML Object maps schemas to XML | Well-formedness only (any DTD allowed, nesting bounded); no schema check | The XML mapping is large and rarely relied on; documented as not supported |
| B5 | Text charsets | RFC 9110 §8.3.2 | UTF-8 (default), US-ASCII, ISO-8859-1; others are `malformed_body` | The common ones, decoded exactly |
| B6 | Form fields not in the schema | Silent | They are properties of the form object, so `additionalProperties` decides | The body is one object |
| B7 | `allowReserved: false` with unencoded reserved characters | SHOULD encode | Accepted, as are encoded ones | Never refuses |
| B8 | Multipart part types | §4.8.15.1 defaults | Objects JSON; arrays take their items' type (one part per item); files are text read lossily; LF-only line breaks accepted | Lenient on what clients send |
| B9 | Empty optional body | §4.8.13 | Fine, whatever the `Content-Type` | An empty body is no body |
| B10 | Size | Silent | Over `max_body_bytes` (16 MiB) is `body_too_large` and not parsed | Bounded memory |

## Responses

| # | Topic | Spec | Decision | Why |
|---|---|---|---|---|
| Q1 | Status lookup | §4.8.16.2 | Exact code, then `1XX`–`5XX` (case-insensitive `X`), then `default` | Spec order |
| Q2 | Empty body where content is declared | Silent | `missing_body`, except for HEAD, 204 and 304 (RFC 9110 §15.3.5, §15.4.5) | Those never carry content |
| Q3 | A body where no content is declared | Silent | Not a problem | Error responses are often undocumented |
| Q4 | Header values | §4.8.21 `simple` style | Literal text, never re-read as JSON; each repeated line checked | `"abc"` with quotes is a 5-character string |
| Q5 | `Content-Type` header definition | §4.8.17.1 SHALL be ignored | Ignored | Spec |

## Schemas

| # | Topic | Spec | Decision | Why |
|---|---|---|---|---|
| J1 | 3.0 `nullable` with `enum` | 3.0.4 §4.8.24.1: other keywords keep their effect | `null` must be listed in `enum` to be allowed; `nullable` without `type` does nothing; never pushed into `allOf`/`oneOf` | The 3.0.3 clarification |
| J2 | Keywords outside the 3.0 Schema Object | Unknown keywords constrain nothing | Ignored in 3.0 documents | `const` in a 3.0 document is not a keyword there |
| J3 | `integer` | 3.0 uses Wright draft 00; 2020-12 §6.1.1 | 3.0: the JSON integer production (`1.0`, `1e2` are not integers); 3.1: any number with a zero fraction | Each version's own definition |
| J4 | Count keywords as floats | Must be non-negative integers | `2.0`, `1e0` are counts; past `u64` a maximum is unbounded and a minimum unreachable; negative or fractional counts are dropped with a load warning | Never refuses the document |
| J5 | Formats | 2020-12 §7.2: format is an annotation unless asserted; OpenAPI lists its own | Asserted: date, time, date-time (RFC 3339), email (RFC 5321), uuid, uri, uri-reference (RFC 3986), ipv4, ipv6, hostname (RFC 1123), int32, int64. Everything else, including `byte`, `binary`, `password`, `float`, `double`, is an annotation. Option `assert_formats` | The listed formats are the ones APIs rely on |
| J6 | `hostname` and A-labels | RFC 5891 | Letters, digits and hyphens per label (RFC 1123); `--` in positions 3–4 only for `xn--`; Punycode contents are not decoded or checked | Never refuses a valid name; IDNA is out of scope |
| J7 | Patterns | ECMA-262 (2020-12 §6.3.3) | Translated to the linear-time `regex` crate with ECMA meanings (ASCII `\d\w\b`, ECMA `\s`, `.` without line terminators, `\cX`, `\uXXXX`); lookaround and backreferences are refused at load naming the pattern | Required: bounded time |
| J8 | Discriminator | §4.8.25: `mapping` and implicit names | When the payload has the property, its value must be a mapped name and the payload is checked against that schema; when every `oneOf`/`anyOf` branch is mapped, that branch alone decides (option `discriminator_selects_branch`); without the property the plain `oneOf`/`anyOf` applies | Enforced as asked, without refusing payloads that match two loose branches |
| J9 | Discriminator inheritance cycles | Silent | A child's `allOf` reference to its base points at a copy of the base without the dispatch | The common `Pet`/`Dog` shape otherwise loops |
| J10 | `readOnly` / `writeOnly` | SHOULD NOT be sent | Dropped from `required` in the direction concerned, following `$ref` and `allOf` members of the same schema; sending them is allowed | SHOULD NOT is not MUST NOT |
| J11 | Equality in `enum`, `const`, `uniqueItems` | 2020-12 §4.2.2 | Numbers by value (`1` equals `1.0`), objects regardless of member order | Spec |
| J12 | `#` references inside 3.1 schemas without `$id` | §4.8.24: the document is the resource | Resolve against the OpenAPI document root | Spec |
| J13 | References to the JSON Schema metaschemas | Would be remote | Allowed; the engine has them built in | No retrieval happens |

## Parsing and limits

| # | Topic | Spec | Decision | Why |
|---|---|---|---|---|
| L1 | Duplicate JSON keys | RFC 8259 §4: SHOULD be unique | Refused | Parsers disagree on which value wins (parser differential) |
| L2 | Invalid UTF-8 and lone surrogate escapes | RFC 8259 §8.1, §8.2 | Refused | Same |
| L3 | Byte order mark | RFC 8259 §8.1 MAY ignore | One leading BOM is ignored | Common from some tools |
| L4 | Huge numbers | Unbounded in JSON | Exponents beyond ±4096 or more than 4096 digits are `malformed_body` | Exact comparison must stay bounded |
| L5 | Nesting | Unbounded | Documents 128 levels; bodies `max_body_depth` (128), lowered per document so body depth × schema in-place depth stays within a fixed frame budget | Never overflows the stack |
| L6 | In-place schema chains | Unbounded | At most 64 nested `$ref`/`allOf`/`anyOf`/… applied to one value, and at most 100,000 subschemas reachable that way (fan-out), else a load error | The schema engine does not memoise and would cost 2^n |
| L7 | YAML | OpenAPI 3.1.1 §4.3: YAML 1.2 | Core schema (`yes`, `no`, `on` are strings); duplicate keys, several documents and alias expansion over 4 M nodes are refused; `<<` merge keys are ordinary keys; `.inf`/`.nan` become strings | YAML 1.2, bounded |
| L8 | External references | Allowed by the spec | Refused at load | Validation never reads files or the network; bundle the document first |
| L9 | Report size | Silent | `max_problems` (20) and `max_report_bytes` (8 KiB); the rest is counted as "and N more" within the explanation budget | Bounded output |
| L10 | Where a failed `anyOf`/`oneOf` is reported | Silent | At the `anyOf`/`oneOf` location, or inside the one branch whose `type` fits the value | Precise without exploring every branch |
