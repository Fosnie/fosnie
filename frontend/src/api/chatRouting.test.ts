// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
import { describe,it,expect } from "vitest";
import { routedChatFrame,routingSurfaceSupported,type ChatRoutingCapabilities } from "./chatRouting";
describe("remote routing compatibility",()=>{
  const caps:ChatRoutingCapabilities={schema_version:1,enabled:true,frame:"chat.routed.send",selection_modes:["default","manual","auto"]};
  it("requires explicit desktop credential support while preserving browser and old-server behavior",()=>{
    expect(routingSurfaceSupported(undefined,false)).toBe(false);
    expect(routingSurfaceSupported(caps,false)).toBe(true);
    expect(routingSurfaceSupported(caps,true)).toBe(false);
    expect(routingSurfaceSupported({...caps,desktop:true},true)).toBe(false);
    expect(routingSurfaceSupported({...caps,desktop:true,desktop_credential:"original_key_ticket_v1"},true)).toBe(true);
    expect(routingSurfaceSupported({...caps,schema_version:2,desktop:true,desktop_credential:"original_key_ticket_v1"},true)).toBe(false);
  });
  it("preserves explicit Auto intent without inventing device identity or a legacy provider",()=>{
    const frame=routedChatFrame({selection:{mode:"auto",objective:"quality",policy_id:"p"},options:{local_only:true,effort:"xhigh"}},"question","c","a","project");
    expect(frame).toEqual({type:"chat.routed.send",request:{chat_id:"c",agent_id:"a",project_id:"project",content:"question",model_selection:{mode:"auto",objective:"quality",policy_id:"p"},options:{local_only:true,effort:"xhigh"}}});
  });
});
