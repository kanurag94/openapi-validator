//! A bound on in-place schema nesting.
//!
//! Applicators that apply to the same value (`$ref`, `allOf`, `anyOf`,
//! `oneOf`, `not`, `if`/`then`/`else`, `dependentSchemas`) nest validator
//! calls without consuming any of the value, so their chain length (through
//! references) is what decides how deep validation recurses for one value.
//! The document nesting limit bounds chains written inline; this bounds
//! chains built from references. The walk is iterative, so measuring a long
//! chain cannot itself overflow the stack. A cycle adds nothing: it is
//! recursion without progress, which the schema engine detects.
//!
//! The same walk counts in-place work: how many schema nodes one value can
//! be checked against before any of it is consumed. Schemas that reach the
//! same target through several applicators (`allOf` and `anyOf` both
//! pointing at the next link of a chain) multiply that count, and the
//! schema engine does not memoise, so a 30-link chain would cost 2^30.

use std::collections::HashMap;

use serde_json::Value;

use crate::doc;

/// Longest allowed chain of in-place applicators.
pub(crate) const MAX_IN_PLACE: usize = 64;
/// Validator frames one call may nest: body depth times in-place depth.
/// Measured at well under 512 KiB of stack in release builds.
pub(crate) const FRAME_BUDGET: usize = 2048;
/// Largest allowed in-place work for one value (schema nodes visited).
pub(crate) const MAX_IN_PLACE_WORK: u64 = 100_000;

/// What the walk measured.
pub(crate) struct InPlace {
    pub depth: usize,
    pub work: u64,
}

const IN_PLACE_ARRAYS: &[&str] = &["allOf", "anyOf", "oneOf"];
const IN_PLACE_SINGLE: &[&str] = &["not", "if", "then", "else"];
const CHILD_SINGLE: &[&str] = &[
    "items",
    "additionalProperties",
    "propertyNames",
    "contains",
    "unevaluatedProperties",
    "unevaluatedItems",
];
const CHILD_MAPS: &[&str] = &["properties", "patternProperties", "$defs", "definitions"];

/// The schemas applied in place, and those applied to child values.
fn edges<'a>(root: &'a Value, s: &'a Value) -> (Vec<&'a Value>, Vec<&'a Value>) {
    let mut same = Vec::new();
    let mut child = Vec::new();
    let Some(m) = s.as_object() else {
        return (same, child);
    };
    if let Some(t) = m
        .get("$ref")
        .and_then(Value::as_str)
        .and_then(|r| doc::lookup(root, r))
    {
        same.push(t);
    }
    for k in IN_PLACE_ARRAYS {
        same.extend(m.get(*k).and_then(Value::as_array).into_iter().flatten());
    }
    for k in IN_PLACE_SINGLE {
        same.extend(m.get(*k));
    }
    if let Some(d) = m.get("dependentSchemas").and_then(Value::as_object) {
        same.extend(d.values());
    }
    for k in CHILD_SINGLE {
        child.extend(m.get(*k));
    }
    child.extend(
        m.get("prefixItems")
            .and_then(Value::as_array)
            .into_iter()
            .flatten(),
    );
    for k in CHILD_MAPS {
        child.extend(
            m.get(*k)
                .and_then(Value::as_object)
                .into_iter()
                .flat_map(|o| o.values()),
        );
    }
    (same, child)
}

/// The longest in-place chain, and the largest in-place work, of any
/// schema reachable from `starts`.
pub(crate) fn measure(root: &Value, starts: &[&Value]) -> InPlace {
    // (depth, work) per node address; `None` while the node is on the stack
    // (reaching it again is a cycle and adds nothing).
    let mut memo: HashMap<usize, Option<(usize, u64)>> = HashMap::new();
    let mut best = InPlace { depth: 0, work: 0 };
    let mut pending: Vec<&Value> = starts.to_vec();
    while let Some(start) = pending.pop() {
        if memo.contains_key(&addr(start)) {
            continue;
        }
        // Iterative post-order over in-place edges.
        let mut stack: Vec<(&Value, Vec<&Value>, usize, u64)> = Vec::new();
        let (same, child) = edges(root, start);
        pending.extend(child);
        memo.insert(addr(start), None);
        stack.push((start, same, 0, 0));
        while let Some((node, rest, acc_d, acc_w)) = stack.last_mut() {
            if let Some(next) = rest.pop() {
                match memo.get(&addr(next)) {
                    Some(None) => {} // cycle
                    Some(Some((d, w))) => {
                        *acc_d = (*acc_d).max(*d);
                        *acc_w = acc_w.saturating_add(*w);
                    }
                    None => {
                        let (same, child) = edges(root, next);
                        pending.extend(child);
                        memo.insert(addr(next), None);
                        stack.push((next, same, 0, 0));
                    }
                }
                continue;
            }
            let d = *acc_d + 1;
            let w = acc_w.saturating_add(1);
            memo.insert(addr(node), Some((d, w)));
            best.depth = best.depth.max(d);
            best.work = best.work.max(w);
            stack.pop();
            if let Some((_, _, pd, pw)) = stack.last_mut() {
                *pd = (*pd).max(d);
                *pw = pw.saturating_add(w);
            }
        }
    }
    best
}

fn addr(v: &Value) -> usize {
    v as *const Value as usize
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn chains_through_refs_and_cycles() {
        let mut defs = serde_json::Map::new();
        for i in 0..10_000 {
            defs.insert(
                format!("S{i}"),
                json!({"allOf": [{"$ref": format!("#/d/S{}", i + 1)}]}),
            );
        }
        defs.insert("S10000".into(), json!({"type": "integer"}));
        let root = json!({"d": defs});
        let start = root.pointer("/d/S0").unwrap();
        assert_eq!(measure(&root, &[start]).depth, 20_001);
        let cyc = json!({"d": {"A": {"$ref": "#/d/B"}, "B": {"allOf": [{"$ref": "#/d/A"}]}}});
        assert!(measure(&cyc, &[cyc.pointer("/d/A").unwrap()]).depth <= 3);
        let child = json!({"properties": {"a": {"allOf": [{"allOf": [true]}]}}});
        assert_eq!(measure(&child, &[&child]).depth, 3);
    }

    #[test]
    fn fan_out_multiplies_work() {
        let mut defs = serde_json::Map::new();
        for i in 0..25 {
            let next = format!("#/d/S{}", i + 1);
            defs.insert(
                format!("S{i}"),
                json!({"allOf": [{"$ref": next}], "anyOf": [{"$ref": next}, true]}),
            );
        }
        defs.insert("S25".into(), json!({"type": "integer"}));
        let root = json!({"d": defs});
        let m = measure(&root, &[root.pointer("/d/S0").unwrap()]);
        assert!(m.work > MAX_IN_PLACE_WORK, "{}", m.work);
        assert!(m.depth < MAX_IN_PLACE);
    }
}
