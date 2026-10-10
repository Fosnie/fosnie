import {describe,it,expect} from "vitest";
import {usdToMicro,microToUsd,profileDraft,importEvidence,newPolicy,emptyRequirements,type ModelEntry} from "./routingSettings";
describe("routing management evidence and exact budget units",()=>{
  it("distinguishes unknown prices from zero and rejects unsafe or fractional micro-USD",()=>{
    expect(usdToMicro("")).toBeNull();expect(usdToMicro("0")).toBe(0);expect(usdToMicro("12.345678")).toBe(12345678);
    expect(microToUsd(0)).toBe("0");expect(microToUsd(10000000)).toBe("10");expect(microToUsd(null)).toBe("");
    for(const invalid of ["-1","NaN","0.0000001","1e2","9007199255"])expect(()=>usdToMicro(invalid)).toThrow();
  });
  it("a new model remains unknown and saving a stale profile cannot carry measured evidence",()=>{
    const entry:ModelEntry={id:"model",label:null,scope:"user",enabled:true,model:"new",provider_revision:2,profile_revision:1,profile:null};
    const blank=profileDraft(entry,false);expect(blank.auto_enabled).toBe(false);expect(blank.quality_status).toBe("unknown");expect(blank.pricing.input_microusd_per_million).toBeNull();
    entry.profile={...blank,provider_revision:1,revision:1,model:"old",auto_enabled:true,auto_effort:"high",locality:"local",locality_trusted:true,quality_status:"verified",evaluation_samples:100,evaluation_version:"eval-v1",capabilities:{...blank.capabilities,tools:"verified"}};
    const stale=profileDraft(entry,true);expect(stale.model).toBe("new");expect(stale.provider_revision).toBe(2);expect(stale.locality_trusted).toBe(false);
    expect(stale.auto_enabled).toBe(false);expect(stale.auto_effort).toBeNull();expect(stale.quality_status).toBe("declared");expect(stale.evaluation_samples).toBe(0);expect(stale.capabilities.tools).toBe("declared");
    expect(entry.profile.quality_status).toBe("verified");
  });
});
describe("P12 evaluation imports",()=>{
  it("keeps opt-ins off and rejects malformed or duplicate imports before rendering",()=>{
    expect(newPolicy("deployment").policy.classifier?.effort).toBeNull();expect(newPolicy("deployment").policy.classifier?.native_schema).toBe(false);expect(newPolicy("deployment").policy.learned_ranking).toBe(false);expect(newPolicy("deployment").policy.quality_escalation).toBe(false);
    const row={id:"fixture",source:"synthetic",families:40,passed:20,reviewed:false,profile_sha256:"a".repeat(64),purpose:"primary",workload:emptyRequirements()};
    expect(importEvidence(JSON.stringify([row]))[0].source).toBe("synthetic");
    for(const bad of [[],[row,row],[{...row,workload:null}],[{...row,passed:41}],[{...row,source:"invented"}]])expect(()=>importEvidence(JSON.stringify(bad))).toThrow();
    expect(()=>importEvidence(" ".repeat(131073))).toThrow();
  });
  it("clears measured records on provider changes and unprivileged profile saves",()=>{
    const entry:ModelEntry={id:"model",label:null,scope:"deployment",enabled:true,model:"same",provider_revision:1,profile_revision:1,profile:null};
    entry.profile=profileDraft(entry,true);entry.profile.quality_evidence=importEvidence(JSON.stringify([{id:"fixture",source:"live",families:40,passed:40,reviewed:true,profile_sha256:"a".repeat(64),purpose:"primary",workload:emptyRequirements()}]));
    expect(profileDraft(entry,true).quality_evidence).toHaveLength(1);expect(profileDraft(entry,false).quality_evidence).toEqual([]);
    entry.provider_revision=2;expect(profileDraft(entry,true).quality_evidence).toEqual([]);expect(entry.profile.quality_evidence).toHaveLength(1);
  });
});

it("task exclusions survive draft cloning without inventing verification",()=>{
  const entry={id:"a",provider_revision:1,profile_revision:1,model:"opaque",scope:"deployment",profile:null} as unknown as ModelEntry;
  const blank=profileDraft(entry,true);expect(blank.task_exclusions).toEqual([]);
  entry.profile={...blank,revision:1,task_exclusions:["reasoning"],quality_status:"declared"};
  const draft=profileDraft(entry,true);expect(draft.task_exclusions).toEqual(["reasoning"]);
  draft.task_exclusions!.push("coding");expect(entry.profile.task_exclusions).toEqual(["reasoning"]);
});


it("JSON effort restrictions preserve explicit empty denial and clone their allowed values",()=>{
  const entry={id:"a",provider_revision:1,profile_revision:1,model:"opaque",scope:"deployment",profile:null} as unknown as ModelEntry;
  entry.profile=profileDraft(entry,true);entry.profile.capabilities.structured_output_efforts=[];
  expect(profileDraft(entry,true).capabilities.structured_output_efforts).toEqual([]);
  entry.profile.capabilities.structured_output_efforts=["high"];
  const draft=profileDraft(entry,true);draft.capabilities.structured_output_efforts!.push("low");
  expect(entry.profile.capabilities.structured_output_efforts).toEqual(["high"]);
});
