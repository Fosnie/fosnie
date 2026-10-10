// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
//! Operator-reviewed held-out evidence. No prompt store or online exploration.
use super::*;
use serde::{Deserialize,Serialize};
use sha2::{Digest,Sha256};
use uuid::Uuid;

#[derive(Debug,Clone,Copy,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="snake_case")]
pub enum EvidenceSource { Live, Synthetic }

#[derive(Debug,Clone,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvaluationEvidence {
    pub id:Uuid,
    pub source:EvidenceSource,
    pub dataset_sha256:String,
    pub families_sha256:String,
    pub profile_sha256:String,
    pub evaluator_version:String,
    pub purpose:CallPurpose,
    pub workload:WorkloadRequirements,
    /// One independently held-out dialogue family per observation. Unknowns
    /// and failures count in total but never in passed. No train/calibration rows.
    pub families:u32,
    pub passed:u32,
    pub reviewed:bool,
}

pub fn fingerprint(profile:&ModelProfile)->String {
    let caps=&profile.capabilities;
    // Arrays and ordered sets avoid dependency feature changes to JSON map order.
    let mut value=serde_json::json!(["fosnie-routing-profile-v1",profile.provider_id,profile.provider_revision,profile.adapter,
        profile.model,[caps.tools,caps.vision,caps.structured_output,caps.context_tokens,caps.max_output_tokens,caps.efforts],
        profile.tier,profile.task_tiers.iter().collect::<Vec<_>>()]);
    // Preserve old fingerprints byte-for-byte when no preset is configured.
    if let Some(effort)=&profile.auto_effort {value.as_array_mut().unwrap().push(serde_json::json!(["auto_effort",effort]));}
    if !profile.task_exclusions.is_empty() {
        value.as_array_mut().unwrap().push(serde_json::json!(["task_exclusions",profile.task_exclusions]));
    }
    if let Some(efforts)=&caps.structured_output_efforts {
        value.as_array_mut().unwrap().push(serde_json::json!(["structured_output_efforts",efforts]));
    }
    hex::encode(Sha256::digest(value.to_string().as_bytes()))
}
pub fn validate(profile:&ModelProfile)->Result<(),RoutingError> {
    if profile.quality_evidence.len()>64 {return Err(RoutingError::Invalid("too many evaluation records"));}
    let expected=fingerprint(profile);let mut ids=std::collections::BTreeSet::new();
    for row in &profile.quality_evidence {
        if !ids.insert(row.id) || row.families==0 || row.families>1_000_000 || row.passed>row.families
            || row.profile_sha256!=expected || [&row.dataset_sha256,&row.families_sha256].iter()
                .any(|digest|digest.len()!=64 || !digest.bytes().all(|b|b.is_ascii_hexdigit()))
            || row.evaluator_version.is_empty() || row.evaluator_version.len()>128
            || row.workload.task==TaskKind::Unknown || row.workload.uncertain
            || row.workload.input_tokens==0 || row.workload.output_tokens==0
            || row.workload.language.as_ref().is_some_and(|v|v.len()>32)
            || row.workload.effort.as_ref().is_some_and(|v|v.len()>24) {
            return Err(RoutingError::Invalid("invalid or stale evaluation evidence"));
        }
    }
    Ok(())
}
/// Wilson's 95% lower bound describes these observations, not a calibrated
/// probability of the next answer. Fixed minimum sample/quality floors avoid
/// choosing a cheap model on a single lucky result.
pub fn lower_bound(passed:u32,total:u32)->u32 {
    if total==0 || passed>total {return 0;}
    let n=f64::from(total);let p=f64::from(passed)/n;let z=1.96_f64;
    (((p+z*z/(2.0*n)-z*((p*(1.0-p)+z*z/(4.0*n))/n).sqrt())/(1.0+z*z/n))*10000.0).floor() as u32
}
pub fn nearest<'a>(profile:&'a ModelProfile,req:&WorkloadRequirements,purpose:CallPurpose)->Option<(&'a EvaluationEvidence,u32)> {
    if req.uncertain || req.task==TaskKind::Unknown {return None;}
    let digest=fingerprint(profile);
    profile.quality_evidence.iter().filter_map(|row| {
        let w=&row.workload;
        if row.source!=EvidenceSource::Live || !row.reviewed || row.families<20 || row.passed>row.families
            || row.profile_sha256!=digest || row.purpose!=purpose || w.task!=req.task || w.minimum_tier!=req.minimum_tier || w.uncertain
            || w.tools!=req.tools || w.vision!=req.vision || w.structured_output!=req.structured_output
            || w.fresh_information!=req.fresh_information || w.language!=req.language || w.effort!=req.effort {
            return None;
        }
        // Bounded nearest-neighbour retrieval using public workload features.
        // Never extrapolate beyond a factor of two in either token dimension.
        let distance=|a:u32,b:u32|->Option<u64> {
            if a==0 || b==0 || u64::from(a.max(b))>u64::from(a.min(b))*2 {return None;}
            Some(u64::from(a.abs_diff(b))*10000/u64::from(a.max(b)))
        };
        Some((distance(w.input_tokens,req.input_tokens)?+distance(w.output_tokens,req.output_tokens)?,row))
    }).min_by_key(|(distance,row)|(*distance,lower_bound(row.passed,row.families),row.id))
        .map(|(_,row)|(row,lower_bound(row.passed,row.families)))
}

/// Candidate-specific effort is checked by `nearest` against that candidate's
/// resolved preset or explicit request. Cohort comparability concerns the same
/// public workload and held-out families, not identical provider effort names.
pub(super) fn comparable(a:&EvaluationEvidence,b:&EvaluationEvidence)->bool {
    let without_effort=|w:&WorkloadRequirements|WorkloadRequirements{effort:None,..w.clone()};
    a.dataset_sha256==b.dataset_sha256 && a.families_sha256==b.families_sha256
        && a.evaluator_version==b.evaluator_version && a.purpose==b.purpose && a.families==b.families
        && without_effort(&a.workload)==without_effort(&b.workload)
}

#[derive(Debug,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceBatch {
    pub schema_version:u32,pub source:EvidenceSource,pub dataset_sha256:String,pub profile_sha256:String,
    pub evaluator_version:String,pub purpose:CallPurpose,pub workload:WorkloadRequirements,
    pub families:Vec<HeldOutFamily>,pub observations:Vec<Observation>,
}
#[derive(Debug,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HeldOutFamily {pub id:String,pub split:EvaluationSplit}
#[derive(Debug,Deserialize)]
#[serde(rename_all="snake_case")]
pub enum EvaluationSplit { Test }
#[derive(Debug,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Observation {pub family_id:String,pub outcome:Outcome}
#[derive(Debug,Deserialize,PartialEq,Eq)]
#[serde(rename_all="snake_case")]
pub enum Outcome { Pass, Fail, Unknown }

/// Offline aggregation of an independently reviewed test manifest. Every
/// declared family counts, including absent/failed/unjudgeable observations.
/// This verifies structure, not the honesty of an operator's live provenance.
pub fn aggregate(profile:&ModelProfile,batch:EvidenceBatch)->Result<EvaluationEvidence,RoutingError> {
    let invalid=||RoutingError::Invalid("invalid held-out evaluation batch");
    if batch.schema_version!=1 || batch.profile_sha256!=fingerprint(profile)
        || batch.families.is_empty() || batch.families.len()>20_000 || batch.observations.len()>20_000 {return Err(invalid());}
    let families:std::collections::BTreeSet<_>=batch.families.iter().map(|f|f.id.as_str()).collect();
    if families.len()!=batch.families.len() || families.iter().any(|id|id.is_empty()||id.len()>128||id.chars().any(char::is_control)) {return Err(invalid());}
    let mut observed=std::collections::BTreeSet::new();let mut passed=0;
    for row in batch.observations {
        if !families.contains(row.family_id.as_str()) || !observed.insert(row.family_id) {return Err(invalid());}
        if row.outcome==Outcome::Pass {passed+=1;}
    }
    let record=EvaluationEvidence{id:Uuid::now_v7(),source:batch.source,dataset_sha256:batch.dataset_sha256,
        families_sha256:hex::encode(Sha256::digest(serde_json::to_vec(&families).map_err(|_|invalid())?)),
        profile_sha256:batch.profile_sha256,evaluator_version:batch.evaluator_version,purpose:batch.purpose,
        workload:batch.workload,families:families.len() as u32,passed,reviewed:false};
    let mut checked=profile.clone();checked.quality_evidence=vec![record.clone()];validate(&checked)?;
    Ok(record)
}
