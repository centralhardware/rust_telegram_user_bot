//! `raw` without the fields Telegram left at their default.
//!
//! Every MTProto update serializes with its whole flag set spelled out, and
//! almost all of it is `false`: a message carries `"out":false,
//! "mentioned":false,"media_unread":false,"silent":false,"post":false,…`
//! whether or not any of it applies. Across the log that is more than half of
//! `raw` by volume -- 3000 recent rows measure 4.87 MB serialized and 2.27 MB
//! with the defaults dropped.
//!
//! Nothing is lost by dropping them. `raw` is read as a fallback for fields the
//! typed columns do not carry, always through `JSONExtract*`, and ClickHouse
//! returns the type's default for a key that is not there -- exactly the value
//! that was written out. An absent `"out"` reads back as `false` either way.
//!
//! Only unambiguous defaults go: `false`, `null`, `[]` and `{}`. Numbers and
//! strings stay, `0` and `""` included, because for those the difference
//! between "Telegram sent zero" and "Telegram sent nothing" is real -- a
//! `ttl_period` of 0 is not the same as a message without one.

use serde::Serialize;
use serde_json::Value;

/// Serialize `value` to JSON with the defaulted fields left out.
pub fn to_string<T: Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(mut json) => {
            prune(&mut json);
            serde_json::to_string(&json).unwrap_or_default()
        }
        Err(_) => String::new(),
    }
}

/// Drop every defaulted entry, at any depth.
///
/// A key whose value prunes down to nothing -- an object of nothing but
/// `false`s -- goes with it: it says as little as the fields it held.
fn prune(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for v in map.values_mut() {
                prune(v);
            }
            map.retain(|_, v| !is_default(v));
        }
        Value::Array(items) => {
            for v in items.iter_mut() {
                prune(v);
            }
        }
        _ => {}
    }
}

fn is_default(value: &Value) -> bool {
    match value {
        Value::Bool(b) => !b,
        Value::Null => true,
        Value::Array(items) => items.is_empty(),
        Value::Object(map) => map.is_empty(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn compact(value: Value) -> Value {
        serde_json::from_str(&to_string(&value)).unwrap()
    }

    #[test]
    fn drops_false_null_and_empty_containers() {
        assert_eq!(
            compact(json!({
                "out": false,
                "silent": false,
                "post_author": null,
                "entities": [],
                "reactions": {},
                "id": 42,
            })),
            json!({ "id": 42 })
        );
    }

    #[test]
    fn keeps_zero_and_empty_string() {
        let value = json!({ "ttl_period": 0, "message": "" });
        assert_eq!(compact(value.clone()), value);
    }

    #[test]
    fn keeps_true() {
        assert_eq!(compact(json!({ "out": true })), json!({ "out": true }));
    }

    #[test]
    fn prunes_inside_nested_objects_and_arrays() {
        assert_eq!(
            compact(json!({
                "message": { "Message": { "out": false, "id": 7 } },
                "entities": [{ "bold": false, "offset": 3 }],
            })),
            json!({
                "message": { "Message": { "id": 7 } },
                "entities": [{ "offset": 3 }],
            })
        );
    }

    #[test]
    fn drops_an_object_that_prunes_to_nothing() {
        assert_eq!(
            compact(json!({ "flags": { "out": false, "post": false }, "id": 1 })),
            json!({ "id": 1 })
        );
    }
}
