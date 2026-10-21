//! Property tests: a value generated to fit a schema must validate, and
//! no body, however odd, makes validation panic or exceed its bounds.

mod common;

use openapi_validator::{Options, Request, Spec};
use proptest::prelude::*;
use serde_json::{json, Map, Value};

fn leaf() -> impl Strategy<Value = Value> {
    prop_oneof![
        (-1000i64..1000, 0i64..50, prop::option::of(prop::sample::select(vec![1i64, 2, 5, 10]))).prop_map(|(lo, span, step)| {
            let mut s = json!({"type": "integer", "minimum": lo, "maximum": lo + span * step.unwrap_or(1)});
            if let Some(st) = step {
                s["multipleOf"] = json!(st);
            }
            s
        }),
        (-100i64..100, 1i64..100).prop_map(|(lo, span)| json!({"type": "number", "minimum": lo, "exclusiveMaximum": lo + span})),
        (0usize..5, 0usize..10, prop::option::of(prop::sample::select(vec![
            "^[a-z]+$", "^\\d{3}-\\d{4}$", "^[A-Z][a-z0-9_]*$", "^(foo|bar)\\.json$", "^\\w+@\\w+\\.com$", "^[\\w.-]+$",
        ])), prop::option::of(prop::sample::select(vec!["date", "date-time", "uuid", "email", "uri", "ipv4", "hostname"])))
            .prop_map(|(min, extra, pat, fmt)| {
                let mut s = json!({"type": "string"});
                if let Some(p) = pat {
                    s["pattern"] = json!(p);
                } else if let Some(f) = fmt {
                    s["format"] = json!(f);
                } else {
                    s["minLength"] = json!(min);
                    s["maxLength"] = json!(min + extra);
                }
                s
            }),
        Just(json!({"type": "boolean"})),
        prop::collection::vec(prop_oneof![Just(json!("a")), Just(json!(1)), Just(json!({"k": [1, 2]})), Just(json!(null))], 1..4)
            .prop_map(|e| json!({"enum": e})),
    ]
}

fn schema() -> impl Strategy<Value = Value> {
    leaf().prop_recursive(4, 32, 4, |inner| {
        prop_oneof![
            (inner.clone(), 0usize..3, 0usize..3, any::<bool>()).prop_map(|(items, min, extra, unique)| {
                let mut s = json!({"type": "array", "items": items, "minItems": min, "maxItems": min + extra});
                if unique {
                    s["uniqueItems"] = json!(true);
                }
                s
            }),
            (prop::collection::vec(inner.clone(), 1..4), any::<u8>(), any::<bool>()).prop_map(|(props, req, closed)| {
                let mut p = Map::new();
                let mut required = Vec::new();
                for (i, s) in props.into_iter().enumerate() {
                    let name = format!("p{i}");
                    if req & (1 << i) != 0 {
                        required.push(json!(name.clone()));
                    }
                    p.insert(name, s);
                }
                json!({"type": "object", "properties": p, "required": required, "additionalProperties": !closed})
            }),
            inner.clone().prop_map(|s| json!({"allOf": [s]})),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| json!({"anyOf": [a, b]})),
        ]
    })
}

fn spec_for(version: &str, schema: &Value) -> Option<(Spec, Value)> {
    let doc = json!({
        "openapi": version,
        "info": {"title": "p", "version": "1"},
        "paths": {"/p": {"post": {
            "requestBody": {"required": true, "content": {"application/json": {"schema": schema}}},
            "responses": {"200": {"description": "ok"}}
        }}}
    });
    let spec = Spec::load(doc.to_string().as_bytes(), Options::default()).ok()?;
    Some((spec, doc))
}

fn post(spec: &Spec, body: &[u8]) -> openapi_validator::Report {
    spec.validate_request(&Request {
        method: "POST",
        path: "/p",
        headers: &[("Content-Type", "application/json")],
        body,
        ..Request::default()
    })
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 1000, ..ProptestConfig::default() })]

    #[test]
    fn generated_values_validate(s in schema(), v31 in any::<bool>(), salt in 0u64..5) {
        let version = if v31 { "3.1.0" } else { "3.0.3" };
        let (spec, doc) = spec_for(version, &s).expect("generated schemas load");
        let g = common::Gen { root: &doc, v30: !v31, dir: common::Dir::Request, salt };
        let body_schema = &doc["paths"]["/p"]["post"]["requestBody"]["content"]["application/json"]["schema"];
        if let Some(value) = g.value(body_schema, 0) {
            let r = post(&spec, value.to_string().as_bytes());
            prop_assert!(r.is_valid(), "schema {s}\nvalue {value}\n{r}");
        }
    }

    #[test]
    fn arbitrary_bodies_never_panic(s in schema(), body in prop::collection::vec(any::<u8>(), 0..256)) {
        if let Some((spec, _)) = spec_for("3.1.0", &s) {
            let r = post(&spec, &body);
            prop_assert!(r.problems().len() <= spec.options().max_problems);
        }
    }

    #[test]
    fn arbitrary_json_values_get_bounded_reports(s in schema(), v in arb_json()) {
        if let Some((spec, _)) = spec_for("3.1.0", &s) {
            let r = post(&spec, v.to_string().as_bytes());
            prop_assert!(r.problems().len() <= spec.options().max_problems);
            let bytes: usize = r.problems().iter().map(|p| p.location.len() + p.message.len()).sum();
            prop_assert!(bytes <= spec.options().max_report_bytes);
        }
    }
}

fn arb_json() -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        any::<i64>().prop_map(|n| json!(n)),
        (-1e6f64..1e6).prop_map(|f| json!(f)),
        "[a-zA-Z0-9 é😀]{0,8}".prop_map(Value::String),
    ];
    leaf.prop_recursive(4, 40, 5, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..5).prop_map(Value::Array),
            prop::collection::btree_map("p[0-3]", inner, 0..5)
                .prop_map(|m| Value::Object(m.into_iter().collect())),
        ]
    })
}
