//! Documents that break a MUST of the specification load with warnings,
//! and are refused with `strict_document`.

use openapi_validator::{Options, Spec};

fn warnings(doc: &str) -> Vec<String> {
    Spec::load(doc.as_bytes(), Options::default())
        .unwrap_or_else(|e| panic!("{e}"))
        .warnings()
        .iter()
        .map(|w| w.message.clone())
        .collect()
}

#[track_caller]
fn warns(doc: &str, needle: &str) {
    let w = warnings(doc);
    assert!(
        w.iter().any(|m| m.contains(needle)),
        "expected a warning containing {needle:?}, got {w:?}"
    );
    let strict = Options {
        strict_document: true,
        ..Options::default()
    };
    assert!(
        Spec::load(doc.as_bytes(), strict).is_err(),
        "strict_document refuses it"
    );
}

const HEAD: &str = "openapi: 3.1.0\ninfo: {title: t, version: '1'}\n";

#[test]
fn clean_documents_have_no_warnings() {
    let d = format!(
        "{HEAD}paths:\n  /p/{{id}}:\n    get:\n      operationId: getP\n      parameters:\n        - {{name: id, in: path, required: true, schema: {{type: string}}}}\n      responses: {{'200': {{description: ok}}}}\n"
    );
    assert!(warnings(&d).is_empty(), "{:?}", warnings(&d));
    let strict = Options {
        strict_document: true,
        ..Options::default()
    };
    assert!(Spec::load(d.as_bytes(), strict).is_ok());
}

// OpenAPI 3.1.1 §3.5 and §4.8.12.2.1: template expressions and path
// parameters correspond.
#[test]
fn path_parameters_match_the_template() {
    warns(
        &format!("{HEAD}paths:\n  /p/{{id}}:\n    get:\n      responses: {{'200': {{description: ok}}}}\n"),
        "has no path parameter",
    );
    warns(
        &format!("{HEAD}paths:\n  /p:\n    get:\n      parameters:\n        - {{name: id, in: path, required: true, schema: {{type: string}}}}\n      responses: {{'200': {{description: ok}}}}\n"),
        "does not appear in the path template",
    );
}

// §4.8.12.3: styles per location; §4.8.12.2.1: allowEmptyValue.
#[test]
fn styles_and_allow_empty_value() {
    warns(
        &format!("{HEAD}paths:\n  /p:\n    get:\n      parameters:\n        - {{name: q, in: query, style: matrix, schema: {{type: string}}}}\n      responses: {{'200': {{description: ok}}}}\n"),
        "not defined for query",
    );
    warns(
        &format!("{HEAD}paths:\n  /p:\n    get:\n      parameters:\n        - {{name: h, in: header, allowEmptyValue: true, schema: {{type: string}}}}\n      responses: {{'200': {{description: ok}}}}\n"),
        "only for query",
    );
}

// §4.8.21: Header Object restrictions.
#[test]
fn header_objects() {
    warns(
        &format!("{HEAD}paths:\n  /p:\n    get:\n      responses:\n        '200':\n          description: ok\n          headers:\n            X-A: {{name: X-A, in: header, style: form, schema: {{type: string}}}}\n"),
        "MUST NOT have `name`",
    );
}

// §4.8.14.1: encoding keys and media types.
#[test]
fn encoding_rules() {
    warns(
        &format!("{HEAD}paths:\n  /p:\n    post:\n      requestBody:\n        content:\n          multipart/form-data:\n            schema: {{type: object, properties: {{a: {{type: string}}}}}}\n            encoding: {{b: {{contentType: text/plain}}}}\n      responses: {{'200': {{description: ok}}}}\n"),
        "MUST exist in the schema",
    );
    warns(
        &format!("{HEAD}paths:\n  /p:\n    post:\n      requestBody:\n        content:\n          application/json:\n            schema: {{type: object}}\n            encoding: {{a: {{contentType: text/plain}}}}\n      responses: {{'200': {{description: ok}}}}\n"),
        "SHALL only apply",
    );
}

// §4.8.16: at least one response code; §4.8.10.1: unique operationId.
#[test]
fn responses_and_operation_ids() {
    warns(
        &format!("{HEAD}paths:\n  /p:\n    get:\n      responses: {{}}\n"),
        "at least one response code",
    );
    warns(
        &format!("{HEAD}paths:\n  /a:\n    get: {{operationId: x, responses: {{'200': {{description: ok}}}}}}\n  /b:\n    get: {{operationId: x, responses: {{'200': {{description: ok}}}}}}\n"),
        "MUST be unique",
    );
}

// §4.8.27: required fields of security schemes; 3.0 §4.7.30.1 scopes.
#[test]
fn security_schemes() {
    warns(
        &format!("{HEAD}paths: {{}}\ncomponents:\n  securitySchemes:\n    o: {{type: oauth2}}\n"),
        "REQUIRES `flows`",
    );
    warns(
        "openapi: 3.0.3\ninfo: {title: t, version: '1'}\nsecurity: [{k: [read]}]\npaths: {}\ncomponents:\n  securitySchemes:\n    k: {type: apiKey, in: header, name: X-K}\n",
        "MUST be empty",
    );
}

// §4.8.6.1: a server variable's default is one of its enum values.
#[test]
fn server_variables() {
    warns(
        &format!("{HEAD}servers:\n  - url: /{{v}}\n    variables: {{v: {{default: v3, enum: [v1, v2]}}}}\npaths: {{}}\n"),
        "MUST be one of its `enum`",
    );
}

// OpenAPI 3.0.4 §4.7.24.1: 3.0 Schema Object rules.
#[test]
fn schema_rules_in_30() {
    let d = |schema: &str| {
        format!("openapi: 3.0.3\ninfo: {{title: t, version: '1'}}\npaths: {{}}\ncomponents:\n  schemas:\n    S: {schema}\n")
    };
    warns(&d("{type: array}"), "`items` MUST be present");
    warns(&d("{type: [string, 'null']}"), "MUST be a string");
    warns(
        &d("{type: string, readOnly: true, writeOnly: true}"),
        "both readOnly and writeOnly",
    );
}

const HEAD32: &str = "openapi: 3.2.0\ninfo: {title: t, version: '1'}\n";

// OpenAPI 3.2.0 §4.12.1, §4.12.2.2: querystring parameters.
#[test]
fn querystring_parameters() {
    let op = |params: &str| {
        format!("{HEAD32}paths:\n  /p:\n    get:\n      parameters:\n{params}      responses: {{'200': {{description: ok}}}}\n")
    };
    let qs = "        - {name: q, in: querystring, content: {application/json: {schema: {type: object}}}}\n";
    assert!(warnings(&op(qs)).is_empty(), "{:?}", warnings(&op(qs)));
    warns(
        &op("        - {name: q, in: querystring, schema: {type: string}}\n"),
        "MUST use `content`",
    );
    warns(
        &op("        - {name: q, in: querystring, style: form, content: {text/plain: {}}}\n"),
        "MUST NOT be used with in: querystring",
    );
    warns(&op(&format!("{qs}{qs}")), "MUST NOT appear more than once");
    warns(
        &op(&format!(
            "{qs}        - {{name: a, in: query, schema: {{type: string}}}}\n"
        )),
        "MUST NOT appear with an in: querystring",
    );
}

// OpenAPI 3.2.0 §4.9.1: additionalOperations excludes fixed-field methods.
#[test]
fn additional_operations() {
    let d = |m: &str| {
        format!("{HEAD32}paths:\n  /p:\n    additionalOperations:\n      {m}: {{responses: {{'200': {{description: ok}}}}}}\n")
    };
    assert!(
        warnings(&d("LINK")).is_empty(),
        "{:?}",
        warnings(&d("LINK"))
    );
    warns(&d("POST"), "MUST NOT contain POST");
    warns(&d("QUERY"), "MUST NOT contain QUERY");
}
// OpenAPI 3.2.0 §4.12.3: `style: cookie` is defined for cookies.
#[test]
fn cookie_style() {
    let d = format!("{HEAD32}paths:\n  /p:\n    get:\n      parameters:\n        - {{name: c, in: cookie, style: cookie, schema: {{type: string}}}}\n      responses: {{'200': {{description: ok}}}}\n");
    assert!(warnings(&d).is_empty(), "{:?}", warnings(&d));
    warns(&d.replace("3.2.0", "3.1.0"), "not defined for cookie");
}

// OpenAPI 3.2.1 §4.14.1, §4.14.5.2: positional encodings.
#[test]
fn positional_encodings() {
    let d = |media: &str| {
        format!("{HEAD32}paths:\n  /p:\n    post:\n      requestBody:\n        content:\n          {media}\n      responses: {{'200': {{description: ok}}}}\n")
    };
    let ok =
        d("multipart/mixed: {schema: {type: array}, prefixEncoding: [{contentType: image/png}]}");
    assert!(warnings(&ok).is_empty(), "{:?}", warnings(&ok));
    warns(
        &d("multipart/mixed: {itemSchema: {}, itemEncoding: {}, encoding: {a: {}}}"),
        "MUST NOT be present with prefixEncoding",
    );
    warns(
        &d("application/json: {itemSchema: {}, itemEncoding: {}}"),
        "SHALL only apply to multipart",
    );
    // 3.2.1 §4.14.5.2 ignores entries without an item instead of requiring
    // an array schema.
    let no_items = d("multipart/mixed: {schema: {type: object}, itemEncoding: {}}");
    assert!(warnings(&no_items).is_empty(), "{:?}", warnings(&no_items));
}

// OpenAPI 3.2.0 §4.25.1: an optional discriminating property needs a
// defaultMapping.
#[test]
fn discriminator_default_mapping() {
    let d = |extra: &str, required: &str| {
        format!("{HEAD32}paths: {{}}\ncomponents:\n  schemas:\n    Pet:\n      type: object\n      {required}\n      properties: {{kind: {{type: string}}}}\n      discriminator: {{propertyName: kind{extra}}}\n")
    };
    assert!(warnings(&d("", "required: [kind]")).is_empty());
    assert!(warnings(&d(", defaultMapping: Pet", "required: []")).is_empty());
    warns(&d("", "required: []"), "MUST have a defaultMapping");
    // Required by every branch counts as required.
    let branches = format!("{HEAD32}paths: {{}}\ncomponents:\n  schemas:\n    Pet:\n      oneOf: [{{$ref: '#/components/schemas/A'}}]\n      discriminator: {{propertyName: kind}}\n    A: {{type: object, required: [kind]}}\n");
    assert!(warnings(&branches).is_empty(), "{:?}", warnings(&branches));
}

// OpenAPI 3.2.0 §4.26.1: nodeType replaces attribute and wrapped.
#[test]
fn xml_node_type() {
    let d = |xml: &str| {
        format!("{HEAD32}paths: {{}}\ncomponents:\n  schemas:\n    S:\n      type: object\n      properties:\n        a: {{type: string, xml: {xml}}}\n")
    };
    assert!(warnings(&d("{nodeType: attribute}")).is_empty());
    warns(
        &d("{nodeType: attribute, attribute: true}"),
        "`attribute` MUST NOT be present",
    );
}

// OpenAPI 3.1.1 §4.8.29 and 3.2.0 §4.29: required OAuth flow fields.
#[test]
fn oauth_flows() {
    let d = |head: &str, flow: &str| {
        format!("{head}paths: {{}}\ncomponents:\n  securitySchemes:\n    o:\n      type: oauth2\n      flows:\n        {flow}\n")
    };
    let ok = d(HEAD32, "deviceAuthorization: {deviceAuthorizationUrl: 'https://a/d', tokenUrl: 'https://a/t', scopes: {}}");
    assert!(warnings(&ok).is_empty(), "{:?}", warnings(&ok));
    warns(
        &d(
            HEAD32,
            "deviceAuthorization: {tokenUrl: 'https://a/t', scopes: {}}",
        ),
        "REQUIRES `deviceAuthorizationUrl`",
    );
    warns(
        &d(
            HEAD,
            "authorizationCode: {authorizationUrl: 'https://a/a', scopes: {}}",
        ),
        "REQUIRES `tokenUrl`",
    );
}

// §4.8.1 unique tag names; 3.2.0 §4.22.1 tag parents.
#[test]
fn tags() {
    let d = |tags: &str| format!("{HEAD32}paths: {{}}\ntags: {tags}\n");
    assert!(warnings(&d("[{name: a}, {name: b, parent: a}]")).is_empty());
    warns(&d("[{name: a}, {name: a}]"), "MUST be unique");
    warns(&d("[{name: a, parent: z}]"), "MUST exist");
    warns(&d("[{name: a, parent: b}, {name: b, parent: a}]"), "cycle");
}

// 3.2.0 §4.8.2: a template expression appears once per path.
#[test]
fn template_expression_once() {
    let d = format!("{HEAD32}paths:\n  /p/{{a}}/{{a}}:\n    get:\n      parameters: [{{name: a, in: path, required: true, schema: {{type: string}}}}]\n      responses: {{'200': {{description: ok}}}}\n");
    warns(&d, "more than once");
}

// 3.2.0 §4.21: Header Objects may carry allowReserved.
#[test]
fn header_allow_reserved_in_32() {
    let d = |head: &str| {
        format!("{head}paths:\n  /p:\n    get:\n      responses:\n        '200':\n          description: ok\n          headers:\n            X-A: {{allowReserved: true, schema: {{type: string}}}}\n")
    };
    assert!(warnings(&d(HEAD32)).is_empty());
    warns(&d(HEAD), "MUST NOT be used in a Header Object");
}

// OpenAPI 3.1.1 §4.8.24: Schema Objects are JSON Schema 2020-12 schemas.
#[test]
fn schemas_meet_the_2020_12_meta_schema() {
    let d = |head: &str, schema: &str| {
        format!("{head}paths:\n  /p:\n    post:\n      requestBody: {{content: {{application/json: {{schema: {schema}}}}}}}\n      responses: {{'200': {{description: ok}}}}\ncomponents:\n  schemas:\n    S: {{type: object, properties: {{schema: {{type: string}}}}}}\n")
    };
    assert!(
        warnings(&d(HEAD, "{type: object}")).is_empty(),
        "{:?}",
        warnings(&d(HEAD, "{type: object}"))
    );
    warns(
        &d(HEAD, "{type: foo}"),
        "not a valid JSON Schema 2020-12 schema",
    );
    warns(
        &d(HEAD, "{type: number, exclusiveMaximum: true}"),
        "not a valid JSON Schema 2020-12 schema",
    );
    // A schema naming another dialect is left to that dialect.
    assert!(warnings(&d(HEAD, "{$schema: 'https://example.com/d', type: foo}")).is_empty());
    // 3.0 schemas follow their own rules.
    let v30 = d(
        "openapi: 3.0.3\ninfo: {title: t, version: '1'}\n",
        "{type: number, exclusiveMaximum: true}",
    );
    assert!(warnings(&v30).is_empty(), "{:?}", warnings(&v30));
}

// 3.1.2 §4.8.5.1: a server URL has no query or fragment.
#[test]
fn server_url_without_query() {
    warns(
        &format!("{HEAD}servers: [{{url: 'https://a.example/v1?x=1'}}]\npaths: {{}}\n"),
        "MUST NOT have a query or fragment",
    );
}

// 3.2.1 §4.14.5.1: encoding keys must exist in the data, not the schema.
#[test]
fn encoding_keys_in_32() {
    let d = |head: &str| {
        format!("{head}paths:\n  /p:\n    post:\n      requestBody:\n        content:\n          multipart/form-data:\n            schema: {{type: object, properties: {{a: {{type: string}}}}}}\n            encoding: {{b: {{contentType: text/plain}}}}\n      responses: {{'200': {{description: ok}}}}\n")
    };
    assert!(
        warnings(&d(HEAD32)).is_empty(),
        "{:?}",
        warnings(&d(HEAD32))
    );
    warns(&d(HEAD), "MUST exist in the schema");
}

// OpenAPI 3.0.4 §4.7.24: keywords outside the 3.0 Schema Object are not
// applied, so they are reported, inline schemas included.
#[test]
fn schema_keywords_outside_30() {
    let d = |schema: &str| {
        format!("openapi: 3.0.3\ninfo: {{title: t, version: '1'}}\npaths:\n  /p:\n    post:\n      requestBody: {{content: {{application/json: {{schema: {schema}}}}}}}\n      responses: {{'200': {{description: ok}}}}\n")
    };
    assert!(warnings(&d("{type: object, x-extra: 1}")).is_empty());
    warns(
        &d("{type: string, const: a}"),
        "\"const\" is not an OpenAPI 3.0 Schema Object keyword",
    );
    warns(
        &d("{type: object, properties: {a: {if: {}}}}"),
        "\"if\" is not",
    );
}

// OpenAPI 3.1.1 §4.8.12.4, §4.8.14.1 and §4.8.21: `example` and `examples`
// are mutually exclusive on a parameter, a media type and a header.
#[test]
fn example_and_examples_are_exclusive() {
    let both = "example: x, examples: {e: {value: y}}";
    for v in ["3.0.4", "3.1.0", "3.2.0"] {
        let head = format!("openapi: {v}\ninfo: {{title: t, version: '1'}}\n");
        for doc in [
            format!("{head}paths:\n  /x:\n    get:\n      parameters: [{{name: a, in: query, schema: {{type: string}}, {both}}}]\n      responses: {{'200': {{description: ok}}}}\n"),
            format!("{head}paths:\n  /x:\n    get:\n      parameters: [{{name: a, in: query, content: {{application/json: {{{both}}}}}}}]\n      responses: {{'200': {{description: ok}}}}\n"),
            format!("{head}paths:\n  /x:\n    post:\n      requestBody: {{content: {{application/json: {{schema: {{type: string}}, {both}}}}}}}\n      responses: {{'200': {{description: ok}}}}\n"),
            format!("{head}paths:\n  /x:\n    get:\n      responses: {{'200': {{description: ok, headers: {{X-A: {{schema: {{type: string}}, {both}}}}}}}}}\n"),
        ] {
            warns(&doc, "mutually exclusive");
        }
        let one = format!("{head}paths:\n  /x:\n    get:\n      parameters: [{{name: a, in: query, schema: {{type: string}}, example: x}}]\n      responses: {{'200': {{description: ok}}}}\n");
        assert!(warnings(&one).is_empty(), "{:?}", warnings(&one));
    }
}

// JSON Schema 2020-12 §6.4: a 3.1 pattern SHOULD be valid ECMA-262, read
// with the `u` flag, where `\-` outside a class is not an escape. 3.0 reads
// patterns without the flag, where it is.
#[test]
fn loose_unicode_escapes_in_patterns() {
    let doc = |v: &str| {
        format!("openapi: {v}\ninfo: {{title: t, version: '1'}}\npaths:\n  /x:\n    get:\n      parameters: [{{name: a, in: query, schema: {{type: string, pattern: '^a\\-b$'}}}}]\n      responses: {{'200': {{description: ok}}}}\n")
    };
    warns(&doc("3.1.0"), "Unicode mode");
    assert!(
        warnings(&doc("3.0.4")).is_empty(),
        "{:?}",
        warnings(&doc("3.0.4"))
    );
}
