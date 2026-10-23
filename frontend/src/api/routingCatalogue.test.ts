// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
import { describe,it,expect } from "vitest";
import { newProfile,catalogueReady,activeVariants,variantRef,type Variant } from "./routingCatalogue";
import { routedChatFrame } from "./chatRouting";
import { legacyRoutingIntent } from "./routingVNext";
import { workflowSelectionLabel } from "./workflowRouting";
const variant:Variant={id:"00000000-0000-0000-0000-000000000009",revision:3,scope:{kind:"deployment"},provider_id:"00000000-0000-0000-0000-000000000005",model_config_revision:2,label:"Low",behaviour_sha256:"a".repeat(64),behaviour:{adapter:"openai_chat",model_id:"fixture",effort:{kind:"explicit",value:"low"},generation:{max_output_tokens:256}}};
describe("D2 profile selection contracts",()=>{
 it("templates share policy authority without calibration prerequisites",()=>{
  const custom=newProfile("user","selected",variant);const full=newProfile("user","all_available",variant);
  expect(custom.id).toBe(custom.policy.id);expect(custom.config.membership).toEqual({kind:"selected",variant_ids:[variant.id]});
  expect(full.config.membership).toEqual({kind:"all_available"});expect(full.config.reference).toEqual({kind:"configured",variant:variantRef(variant),allow_uncalibrated:true});
  expect(full.policy.rollout).toBe("active");expect(full.config.selector_sha256).toBeNull();
 });
 it("variant and profile selections use the negotiated frame and keep binding updates explicit",()=>{
  const frame=routedChatFrame({selection:{mode:"manual",provider_id:variant.provider_id},options:{local_only:true,effort:null},variant:{id:variant.id,expected_revision:3},required_profile_version:1},"hi",null,null,null);
  expect(frame.type).toBe("chat.routed.v2.send");if(frame.type!=="chat.routed.v2.send")throw new Error();
  expect(frame.request.save_binding).toBe(false);if(!frame.request.routing_intent)throw new Error();expect(frame.request.routing_intent.variant?.expected_revision).toBe(3);
  expect(()=>legacyRoutingIntent(frame.request.routing_intent!)).toThrow();
  const profile=routedChatFrame({selection:{mode:"auto",policy_id:variant.id},options:{local_only:false,effort:null},required_profile_version:2,save_binding:true},"hi",null,null,null);
  expect(profile.type).toBe("chat.routed.v2.send");expect(workflowSelectionLabel({schema_version:2,required_profile_version:2,selection:{mode:"auto",policy_id:variant.id}})).toBe("Auto · balanced");
  const inherited=routedChatFrame({selection:{mode:"default"},options:{local_only:false,effort:null},required_profile_version:1,inherit:true},"hi",null,null,null);
  expect(inherited.request).not.toHaveProperty("options");expect(inherited.request).toHaveProperty("routing_intent",undefined);
 });
 it("absence of advertised support cannot imply v2",()=>{expect(catalogueReady()).toBe(false);expect(catalogueReady({schema_version:2,selection_versions:[1],profile_versions:[1]} as never)).toBe(false);});
 it("old or malformed catalogue replies keep the legacy composer usable without implying v2",()=>{
  for(const response of [{},[],{schema_version:2},{schema_version:2,selection_versions:[2],profile_versions:[2]}]){
   expect(catalogueReady(response as never)).toBe(false);expect(activeVariants(response as never)).toEqual([]);
  }
 });
});
