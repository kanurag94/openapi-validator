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

// OpenAPI 3.2.0 §4.14.1, §4.14.5.2: positional encodings.
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
    warns(
        &d("multipart/mixed: {schema: {type: object}, itemEncoding: {}}"),
        "need itemSchema or an array schema",
    );
}
