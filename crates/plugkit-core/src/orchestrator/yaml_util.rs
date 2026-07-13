use serde_yaml::Value;

pub fn yaml_to_json(v: &Value) -> serde_json::Value {
    match v {
        Value::Null => serde_json::Value::Null,
        Value::Bool(b) => serde_json::Value::Bool(*b),
        Value::Number(n) => serde_json::from_str(&n.to_string()).unwrap_or(serde_json::Value::Null),
        Value::String(s) => serde_json::Value::String(s.clone()),
        Value::Sequence(s) => serde_json::Value::Array(s.iter().map(yaml_to_json).collect()),
        Value::Mapping(m) => {
            let mut o = serde_json::Map::new();
            for (k, val) in m {
                if let Some(ks) = k.as_str() {
                    o.insert(ks.to_string(), yaml_to_json(val));
                }
            }
            serde_json::Value::Object(o)
        }
        Value::Tagged(t) => yaml_to_json(&t.value),
    }
}
