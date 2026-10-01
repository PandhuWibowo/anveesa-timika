//! JSON ↔ protobuf `Struct`, and time formatting.

use std::collections::BTreeMap;

use chrono::{DateTime, SecondsFormat, Utc};
use prost_types::{value::Kind, ListValue, Struct, Value as PValue};
use serde_json::{Map, Number, Value};

pub fn ts(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Millis, true)
}

pub fn ts_opt(t: Option<DateTime<Utc>>) -> Option<String> {
    t.map(ts)
}

pub fn to_pvalue(v: &Value) -> PValue {
    let kind = match v {
        Value::Null => Kind::NullValue(0),
        Value::Bool(b) => Kind::BoolValue(*b),
        Value::Number(n) => Kind::NumberValue(n.as_f64().unwrap_or(0.0)),
        Value::String(s) => Kind::StringValue(s.clone()),
        Value::Array(a) => Kind::ListValue(ListValue { values: a.iter().map(to_pvalue).collect() }),
        Value::Object(o) => Kind::StructValue(to_struct(o)),
    };
    PValue { kind: Some(kind) }
}

pub fn to_struct(m: &Map<String, Value>) -> Struct {
    Struct { fields: m.iter().map(|(k, v)| (k.clone(), to_pvalue(v))).collect::<BTreeMap<_, _>>() }
}

/// A JSON value that should be an object, as a `Struct` (non-objects → empty).
pub fn value_struct(v: &Value) -> Struct {
    match v {
        Value::Object(o) => to_struct(o),
        _ => Struct::default(),
    }
}

pub fn from_pvalue(v: PValue) -> Value {
    match v.kind {
        None | Some(Kind::NullValue(_)) => Value::Null,
        Some(Kind::BoolValue(b)) => Value::Bool(b),
        Some(Kind::NumberValue(n)) => {
            // Keep integers integers (JSON round-trip friendliness).
            if n.fract() == 0.0 && n.abs() < 9.0e15 {
                Value::Number(Number::from(n as i64))
            } else {
                Number::from_f64(n).map(Value::Number).unwrap_or(Value::Null)
            }
        }
        Some(Kind::StringValue(s)) => Value::String(s),
        Some(Kind::ListValue(l)) => Value::Array(l.values.into_iter().map(from_pvalue).collect()),
        Some(Kind::StructValue(s)) => Value::Object(from_struct(s)),
    }
}

pub fn from_struct(s: Struct) -> Map<String, Value> {
    s.fields.into_iter().map(|(k, v)| (k, from_pvalue(v))).collect()
}

pub fn str_opt(v: &Value) -> Option<String> {
    v.as_str().map(String::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_roundtrip() {
        let v = serde_json::json!({ "a": 1, "b": "x", "c": [true, null, 2.5], "d": { "e": "f" } });
        let back = Value::Object(from_struct(to_struct(v.as_object().unwrap())));
        assert_eq!(v, back);
    }
}
