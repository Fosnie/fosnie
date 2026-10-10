// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
//! Portable bounded JSON contracts. Only flat required scalar objects are
//! supported; unknown keywords fail before dispatch rather than being dropped.
use serde::de::{MapAccess, Visitor};
use serde::Deserialize;
use serde_json::Value;
use super::{AdapterKind, RoutingError};

fn invalid() -> RoutingError { RoutingError::Invalid("unsupported or invalid output schema") }
fn typed(value: &Value, kind: &str) -> bool {
    match kind {
        "string" => value.is_string(), "boolean" => value.is_boolean(),
        "number" => value.as_f64().is_some_and(f64::is_finite),
        "integer" => value.is_i64() || value.is_u64() || value.as_f64().is_some_and(|n|n.is_finite() && n.fract()==0.0),
        _ => false,
    }
}
// JSON numeric enums compare values, not lexical integer/decimal spelling.
// Converting both operands to f64 would incorrectly equate neighbouring large
// integers, so preserve integer operands before comparing a decimal operand.
fn scalar_equal(left:&Value,right:&Value)->bool {
    if !left.is_number() || !right.is_number() {return left==right;}
    fn integral(v:&Value)->Option<i128> {v.as_i64().map(i128::from).or_else(||v.as_u64().map(i128::from))}
    match (integral(left),integral(right)) {
        (Some(a),Some(b))=>a==b,
        (None,None)=>left.as_f64()==right.as_f64(),
        (Some(integer),None)|(None,Some(integer))=>{
            let decimal=if integral(left).is_none(){left}else{right};
            decimal.as_f64().is_some_and(|n|n.is_finite() && n.fract()==0.0
                && n>=i64::MIN as f64 && n<18446744073709551616.0 && n as i128==integer)
        }
    }
}
pub fn supported(adapter: AdapterKind) -> bool {
    matches!(adapter, AdapterKind::OpenaiChat | AdapterKind::OpenaiResponses | AdapterKind::Gemini)
}
pub fn validate(schema: &Value) -> Result<(), RoutingError> {
    let root=schema.as_object().ok_or_else(invalid)?;
    if schema.to_string().len()>8192 || root.keys().any(|k|!matches!(k.as_str(),"type"|"properties"|"required"|"additionalProperties"|"description"))
        || root.get("type").and_then(Value::as_str)!=Some("object") || root.get("additionalProperties")!=Some(&Value::Bool(false)) {return Err(invalid());}
    let props=root.get("properties").and_then(Value::as_object).ok_or_else(invalid)?;
    let required=root.get("required").and_then(Value::as_array).ok_or_else(invalid)?;
    let keys:std::collections::BTreeSet<_>=required.iter().filter_map(Value::as_str).collect();
    if props.is_empty() || props.len()>64 || required.len()!=props.len() || keys.len()!=props.len() || props.keys().any(|k|!keys.contains(k.as_str())) {return Err(invalid());}
    for node in std::iter::once(schema).chain(props.values()) {
        let node=node.as_object().ok_or_else(invalid)?;
        if node.get("description").is_some_and(|v|v.as_str().is_none_or(|s|s.len()>256)) {return Err(invalid());}
    }
    for (key,node) in props {
        if key.is_empty() || key.len()>80 || !key.bytes().all(|c|c.is_ascii_alphanumeric() || c==b'_' || c==b'-') {return Err(invalid());}
        let node=node.as_object().ok_or_else(invalid)?;
        let kind=node.get("type").and_then(Value::as_str).ok_or_else(invalid)?;
        if !matches!(kind,"string"|"boolean"|"number"|"integer") || node.keys().any(|k|!matches!(k.as_str(),"type"|"enum"|"minimum"|"maximum"|"description")) {return Err(invalid());}
        for bound in ["minimum","maximum"] {if node.get(bound).is_some_and(|n|!matches!(kind,"number"|"integer") || !typed(n,"number")) {return Err(invalid());}}
        if let (Some(min),Some(max))=(node.get("minimum"),node.get("maximum")) {if min.as_f64()>max.as_f64() {return Err(invalid());}}
        if let Some(values)=node.get("enum") {
            let values=values.as_array().ok_or_else(invalid)?;
            if values.is_empty() || values.len()>32 || values.iter().enumerate().any(|(i,v)|!typed(v,kind) || values[..i].iter().any(|prior|scalar_equal(prior,v))) {return Err(invalid());}
        }
    }
    Ok(())
}
pub fn overhead(schema: Option<&Value>) -> Result<usize, RoutingError> {
    match schema {Some(s)=>{validate(s)?;Ok(s.to_string().len()+256)},None=>Ok(0)}
}
struct UniqueObject(serde_json::Map<String,Value>);
impl<'de> Deserialize<'de> for UniqueObject {
    fn deserialize<D:serde::Deserializer<'de>>(d:D)->Result<Self,D::Error> {
        struct ObjectVisitor;
        impl<'de> Visitor<'de> for ObjectVisitor {
            type Value=UniqueObject;
            fn expecting(&self,f:&mut std::fmt::Formatter)->std::fmt::Result {f.write_str("an object with unique fields")}
            fn visit_map<M:MapAccess<'de>>(self,mut map:M)->Result<Self::Value,M::Error> {
                let mut object=serde_json::Map::new();
                while let Some((key,value))=map.next_entry::<String,Value>()? {
                    if object.insert(key,value).is_some() {return Err(serde::de::Error::custom("duplicate field"));}
                }
                Ok(UniqueObject(object))
            }
        }
        d.deserialize_map(ObjectVisitor)
    }
}
pub fn validate_output(schema:&Value,text:&str)->Result<(),RoutingError> {
    validate(schema)?;
    let failure=||RoutingError::Invalid("answer does not satisfy output schema");
    if text.len()>64000 {return Err(failure());}
    let UniqueObject(value)=serde_json::from_str(text).map_err(|_|failure())?;
    let props=schema["properties"].as_object().ok_or_else(failure)?;
    if value.len()!=props.len() {return Err(failure());}
    for (key,node) in props {
        let item=value.get(key).ok_or_else(failure)?;
        if !typed(item,node["type"].as_str().unwrap_or(""))
            || node.get("enum").is_some_and(|e|!e.as_array().unwrap().iter().any(|allowed|scalar_equal(allowed,item)))
            || node.get("minimum").is_some_and(|min|item.as_f64()<min.as_f64())
            || node.get("maximum").is_some_and(|max|item.as_f64()>max.as_f64()) {return Err(failure());}
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    fn contract()->Value {serde_json::json!({"type":"object","additionalProperties":false,"properties":{"count":{"type":"integer","minimum":1},"ok":{"type":"boolean"}},"required":["count","ok"]})}
    #[test] fn strict_contract_rejects_fences_duplicates_extra_wrong_types_and_unsupported_keywords() {
        let schema=contract();validate_output(&schema,r#"{"count":2,"ok":true}"#).unwrap();
        for text in [r#"{"count":true,"ok":true}"#,r#"{"count":0,"ok":true}"#,r#"{"count":2,"count":3,"ok":true}"#,r#"{"count":2,"ok":true,"other":1}"#,"```json\n{}\n```",r#"{"count":2}"#] {assert!(validate_output(&schema,text).is_err());}
        let mut bad=schema.clone();bad["$ref"]=Value::String("https://external.test/schema".into());assert!(validate(&bad).is_err());
        bad=schema.clone();bad["properties"]["count"]["pattern"]=Value::String(".*".into());assert!(validate(&bad).is_err());
        assert_eq!(overhead(Some(&schema)).unwrap(),schema.to_string().len()+256);
        let mut numeric=contract();numeric["properties"]["count"]["enum"]=serde_json::json!([2]);
        validate_output(&numeric,r#"{"count":2.0,"ok":true}"#).unwrap();
        numeric["properties"]["count"]["enum"]=serde_json::json!([2,2.0]);assert!(validate(&numeric).is_err());
        assert!(!scalar_equal(&serde_json::json!(9007199254740993_u64),&serde_json::json!(9007199254740992.0)));
    }
}
