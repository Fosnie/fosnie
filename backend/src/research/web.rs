// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
//! Snapshot deployment web restrictions; a changed restriction requires a new job.
use serde_json::{Value,json};
use crate::{AppState,Result,model_routing::RoutingError,config::runtime::{self,ConfigValueType}};

pub async fn policy(state:&AppState)->Result<Value> {
    let enabled=runtime::get(&state.pg,"integration.web_search.enabled").await?;
    if !enabled.is_some_and(|v|matches!(v.value_type,ConfigValueType::Bool) && v.value=="true") {
        return Err(RoutingError::Forbidden.into());
    }
    let mut value=json!({"max_fetches":4});
    for (key,field) in [("web_search.allowlist","domain_allowlist"),("web_search.blocklist","domain_blocklist"),
        ("web_search.allowlist_only","allowlist_only"),("web_search.robots_policy","robots_policy")] {
        if let Some(entry)=runtime::get(&state.pg,key).await? {
            if entry.value.len()>8192 {return Err(RoutingError::Invalid("web policy exceeds limit").into());}
            value[field]=if field=="allowlist_only" {
                if !matches!(entry.value_type,ConfigValueType::Bool) {return Err(RoutingError::Invalid("web policy type").into());}
                match entry.value.as_str() {"true"=>json!(true),"false"=>json!(false),_=>return Err(RoutingError::Invalid("web allowlist mode").into())}
            } else {
                if !matches!(entry.value_type,ConfigValueType::String) || (field=="robots_policy" && !matches!(entry.value.as_str(),"respect"|"user_triggered")) {
                    return Err(RoutingError::Invalid("web policy value").into());
                }
                json!(entry.value)
            };
        }
    }
    Ok(value)
}

pub fn validate_sources(value:&Value,documents:usize,policy:&Value)->Result<()> {
    let invalid=||RoutingError::Invalid("invalid research web sources");
    let sources=value["web_sources"].as_array().filter(|v|!v.is_empty() && v.len()<=4).ok_or_else(invalid)?;
    let citations=value["citations"].as_array().filter(|v|!v.is_empty() && v.len()<=sources.len()).ok_or_else(invalid)?;
    let mut urls=std::collections::BTreeSet::new();
    for (index,row) in sources.iter().enumerate() {
        let raw=row["url"].as_str().filter(|s|s.len()<=4096).ok_or_else(invalid)?;
        let url=reqwest::Url::parse(raw).map_err(|_|invalid())?;
        let host=url.host_str().ok_or_else(invalid)?;
        let matches=|key:&str| policy[key].as_str().unwrap_or("").split(',').map(|s|s.trim().trim_start_matches('.').to_ascii_lowercase())
            .any(|s|!s.is_empty() && (host==s || host.ends_with(&format!(".{s}"))));
        if !matches!(url.scheme(),"http"|"https") || !url.username().is_empty() || url.password().is_some()
            || !urls.insert(raw) || row["domain"]!=host || row["snippet_only"]!=false
            || row["reference"]!=format!("D{}",documents+index+1) || matches("domain_blocklist")
            || ((policy["allowlist_only"]==true || policy["domain_allowlist"].as_str().is_some_and(|s|!s.is_empty())) && !matches("domain_allowlist")) {
            return Err(invalid().into());
        }
        for (key,limit) in [("quote_text",800),("title",512)] {
            if !(key=="title" && row[key].is_null()) && !row[key].as_str().is_some_and(|s|s.len()<=limit && !s.contains('\0')) {return Err(invalid().into());}
        }
        if !row["published_date"].is_null() {return Err(invalid().into());}
        time::OffsetDateTime::parse(row["fetched_at"].as_str().ok_or_else(invalid)?,&time::format_description::well_known::Rfc3339).map_err(|_|invalid())?;
    }
    let mut seen=std::collections::BTreeSet::new();
    for citation in citations {
        if !sources.contains(citation) || !seen.insert(citation["reference"].as_str()) {return Err(invalid().into());}
    }
    Ok(())
}
