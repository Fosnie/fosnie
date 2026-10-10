import { describe,it,expect } from "vitest";
import { routingFailure,failureDescription } from "./routingFailure";
describe("Safe routing failure data",()=>{
  it("uses stable descriptions, never vendor message or credential text",()=>{
    const value=routingFailure({schema_version:1,code:"provider_quota_exhausted",source:"provider",before_output:true,can_retry:false,message:"private-secret"})!;
    expect(failureDescription(value)).toContain("quota");expect(failureDescription(value)).not.toContain("private-secret");
  });
  it("refuses unknown versions, types, codes and prototype keys",()=>{
    const base={schema_version:1,code:"provider_timeout",source:"provider",before_output:true,can_retry:true};
    for(const patch of [{schema_version:2},{code:"toString"},{code:"billing_guess"},{source:"unknown"},{before_output:1},{can_retry:"yes"}])
      expect(routingFailure({...base,...patch})).toBeUndefined();
  });
  it("preserves interruption without making it eligible for an automatic replay",()=>{
    const value=routingFailure({schema_version:1,code:"provider_unavailable",source:"provider",before_output:false,can_retry:false})!;
    expect(value.before_output).toBe(false);expect(value.can_retry).toBe(false);
  });
  it("accepts confirmed bounded cooldown and a persisted deadline",()=>{
    const base={schema_version:1,code:"provider_rate_limited",source:"provider",before_output:true,can_retry:true};
    expect(routingFailure({...base,retry_after_seconds:30,retry_at_ms:1900000000000})?.retry_at_ms).toBe(1900000000000);
    for(const retry_after_seconds of [0,-1,86401,Infinity,NaN,"30"])expect(routingFailure({...base,retry_after_seconds})).toBeUndefined();
  });
});
