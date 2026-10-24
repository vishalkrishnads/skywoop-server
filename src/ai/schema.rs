use schemars::JsonSchema;
use serde_json::Value;

pub fn strict_schema<T: JsonSchema>() -> Value {
    let mut v = serde_json::to_value(schemars::schema_for!(T)).unwrap();
    fn fix(v: &mut Value) {
        match v {
            Value::Object(obj) => {
                // Azure/OpenAI reject `$ref` siblings (e.g. `default` from `#[serde(default)]`).
                if obj.contains_key("$ref") {
                    let r = obj.get("$ref").cloned();
                    obj.clear();
                    if let Some(r) = r {
                        obj.insert("$ref".into(), r);
                    }
                    return;
                }
                // Defaults are not allowed on structured-output schemas.
                obj.remove("default");
                if let Some(Value::Object(props)) = obj.get("properties") {
                    let keys: Vec<Value> = props.keys().cloned().map(Value::String).collect();
                    obj.insert("required".into(), Value::Array(keys));
                    obj.insert("additionalProperties".into(), Value::Bool(false));
                }
                obj.remove("format");
                for child in obj.values_mut() {
                    fix(child);
                }
            }
            Value::Array(a) => a.iter_mut().for_each(fix),
            _ => {}
        }
    }
    fix(&mut v);
    if let Some(o) = v.as_object_mut() {
        o.remove("$schema");
        o.remove("title");
        o.remove("default");
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use schemars::JsonSchema;
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize, JsonSchema)]
    struct Sample {
        name: String,
        nested: Nested,
        optional: Option<String>,
    }

    #[derive(Serialize, Deserialize, JsonSchema)]
    struct Nested {
        n: i32,
    }

    #[test]
    fn strict_schema_requires_all_props() {
        let v = strict_schema::<Sample>();
        let required = v["required"].as_array().unwrap();
        assert!(required.iter().any(|x| x == "name"));
        assert!(required.iter().any(|x| x == "nested"));
        assert!(required.iter().any(|x| x == "optional"));
        assert_eq!(v["additionalProperties"], false);

        // Nested object types are typically under $defs / definitions via $ref.
        let defs = v
            .get("$defs")
            .or_else(|| v.get("definitions"))
            .and_then(|d| d.as_object());
        if let Some(defs) = defs {
            for def in defs.values() {
                if def.get("properties").is_some() {
                    assert_eq!(def["additionalProperties"], false);
                    assert!(def.get("required").is_some());
                }
            }
        }
    }

    #[derive(Serialize, Deserialize, JsonSchema)]
    struct WithDefaultRef {
        #[serde(default)]
        art_style: Style,
    }

    #[derive(Serialize, Deserialize, JsonSchema, Default)]
    #[serde(rename_all = "snake_case")]
    enum Style {
        #[default]
        Photoreal,
        FlatVector,
    }

    #[test]
    fn strict_schema_strips_default_beside_ref() {
        let v = strict_schema::<WithDefaultRef>();
        let art = &v["properties"]["art_style"];
        assert!(art.get("$ref").is_some(), "expected $ref: {art}");
        assert!(
            art.get("default").is_none(),
            "$ref must not carry default: {art}"
        );
        // No other sibling keywords either.
        assert_eq!(art.as_object().map(|o| o.len()), Some(1));
    }

    #[test]
    fn plan_day_schema_has_clean_refs() {
        use crate::domain::plan::PlanDayOut;
        let v = strict_schema::<PlanDayOut>();
        fn assert_clean(v: &Value) {
            match v {
                Value::Object(o) => {
                    assert!(
                        !(o.contains_key("$ref") && o.len() > 1),
                        "$ref has siblings: {o:?}"
                    );
                    for child in o.values() {
                        assert_clean(child);
                    }
                }
                Value::Array(a) => a.iter().for_each(assert_clean),
                _ => {}
            }
        }
        assert_clean(&v);
    }
}
