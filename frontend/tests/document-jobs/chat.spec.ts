import { test,expect,type Page } from "@playwright/test";
const url="/tests/document-jobs/chat.html";
async function fixture(page:Page) {
  page.on("pageerror",e=>console.error("Chat fixture page error:",e.message));
  const state={enabled:true,old:false,desktop:true,capsFailed:false,catalogFailed:false,savedFailed:false,workspace:false,deviceRequests:[] as {authorization:string|undefined;cookie:string|undefined}[],legacy:[] as string[],
    saved:{schema_version:1,selection:{mode:"auto",objective:"economy"} as any,options:{local_only:true,effort:"high" as string|null},adopted:true,supported:true},
    messages:[] as any[]};
  await page.route("**/api/**",async route=>{
    const req=route.request();const path=new URL(req.url()).pathname;
    if(!path.startsWith("/api/"))return route.continue();
    const json=(body:unknown,status=200)=>route.fulfill({status,json:body});
    state.deviceRequests.push({authorization:req.headers().authorization,cookie:req.headers().cookie});
    if(path==="/api/auth/config")return json({mode:"local",local_enabled:true});
    if(path==="/api/whoami")return json({user_id:"owner",role:"user",llm_configured:true,capabilities:{voice:true,voice_live:true,reasoning:{mode:"none"}}});
    if(path==="/api/agents")return json([{id:"agent-one",name:"General Assistant",modes:["general"],params:{},tools:[],can_manage:false}]);
    if(path==="/api/chats")return json([{id:"chat-one",title:"Routed conversation",agent_id:"agent-one",created_at:new Date().toISOString()}]);
    if(path==="/api/routing/browser-chat")return state.old||state.capsFailed ? json({},state.old?404:503) : json({schema_version:1,enabled:state.enabled,frame:"chat.routed.send",desktop:state.desktop,desktop_credential:state.desktop ? "original_key_ticket_v1" : undefined,selection_modes:["default","manual","auto"]});
    if(path==="/api/routing/regenerate")return json({schema_version:1,enabled:true,preserves_original:true,desktop:state.desktop});
    if(path==="/api/routing/catalog")return state.catalogFailed ? json({},503) : json([
      {id:"local-one",label:"Local model",model:"local",enabled:true,provider_revision:1,profile_revision:1,
        profile:{provider_id:"local-one",provider_revision:1,revision:1,model:"local",capabilities:{efforts:["high"]}}},
      {id:"cloud-one",label:"Cloud model",model:"cloud",enabled:true,provider_revision:1,profile_revision:1,
        profile:{provider_id:"cloud-one",provider_revision:1,revision:1,model:"cloud",capabilities:{efforts:["xhigh"]}}},
      {id:"stale-one",label:"Stale model",model:"old",enabled:true,provider_revision:2,profile_revision:1,
        profile:{provider_id:"stale-one",provider_revision:1,revision:1,model:"old"}},
    ]);
    if(path==="/api/routing/policies")return json([{id:"policy-one",label:"Balanced policy",revision:1,scope:"deployment",is_default:true,
      policy:{id:"policy-one",revision:1,strategy:"hybrid",rollout:"shadow",default_provider_id:"cloud-one",classifier_provider_id:"local-one"}}]);
    if(path.endsWith("/routing")&&path.startsWith("/api/chats/"))return state.savedFailed ? json({},403) : json({...state.saved,supported:!state.workspace && !path.includes("branch-one")});
    if(path.endsWith("/messages"))return json(path.includes("branch-one") ? [{id:"branch-answer",role:"assistant",content:"Branch answer",streaming:false}] : state.messages);
    if(path.endsWith("/workspace"))return json(state.workspace ? {id:"workspace-one",label:"Local folder",path:"C:/synthetic",device_id:"device-one"} : null);
    if(path==="/api/me/llm-providers"){state.legacy.push(path);return json({providers:[],active_id:null});}
    if(path.endsWith("/llm-provider")){state.legacy.push(path);return json({});}
    return json([]);
  });
  return state;
}
async function frames(page:Page){return page.evaluate(()=> (window as any).chatTest.frames);}
async function emit(page:Page,frame:any){await page.evaluate(f=>(window as any).chatTest.emit(f),frame);}
async function send(page:Page,text="Route this question") {
  await page.locator("textarea.comp-in").fill(text);await page.locator("textarea.comp-in").press("Enter");
}
test("Production composer sends explicit Auto, acknowledges planning and reloads actual model",async({page})=>{
  const s=await fixture(page);await page.goto(url);
  await expect(page.getByLabel("Chat model",{exact:true})).toHaveValue("default");
  await page.getByLabel("Chat model",{exact:true}).selectOption("auto");
  await page.getByLabel("Routing objective").selectOption("economy");
  await page.getByLabel("Local inference only").check();await page.getByLabel("Routing effort").selectOption("high");
  await expect(page.getByLabel("Chat model",{exact:true}).locator("option")).toHaveCount(4);
  await page.screenshot({path:"test-results/chat-routing-composer.png",fullPage:true});
  await send(page);await expect.poll(()=>frames(page)).toHaveLength(1);
  const sent=(await frames(page))[0];expect(sent.type).toBe("chat.routed.send");expect(sent.request.model_selection).toEqual({mode:"auto",objective:"economy"});
  expect(sent.request.options).toEqual({local_only:true,effort:"high"});expect(sent).not.toHaveProperty("reasoning");expect(sent).not.toHaveProperty("llm_provider_id");
  await emit(page,{type:"chat.created",chat_id:"chat-one"});await emit(page,{type:"chat.routing_started",chat_id:"chat-one",turn_id:"planning-turn"});
  await page.getByTitle("Stop generating").click();expect((await frames(page)).at(-1)).toEqual({type:"chat.cancel",turn_id:"planning-turn"});
  await emit(page,{type:"chat.started",chat_id:"chat-one",turn_id:"planning-turn",message_id:"answer-one"});
  await emit(page,{type:"chat.token",turn_id:"planning-turn",delta:"Routed answer"});
  s.messages=[{id:"user-one",role:"user",content:"Route this question",streaming:false},
    {id:"answer-one",role:"assistant",content:"Routed answer",streaming:false,routing:{schema_version:1,selection_mode:"auto",local_only:true,
      actual:{model:"local-real",effort:"high"},actual_unavailable:false,reason_codes:["single_eligible_model"],status:"completed"}}];
  await emit(page,{type:"chat.completed",chat_id:"chat-one",turn_id:"planning-turn",message_id:"answer-one"});
  await expect(page.getByLabel("Actual model")).toContainText("local-real · high");
  await page.reload();await expect(page.getByLabel("Chat model",{exact:true})).toHaveValue("auto");
  await expect(page.getByLabel("Local inference only")).toBeChecked();await expect(page.getByLabel("Actual model")).toContainText("Only eligible model");
  await page.screenshot({path:"test-results/chat-routing-actual.png",fullPage:true});
  await expect(page.getByTitle("Read aloud",{exact:true})).toHaveCount(0);
  await page.getByTitle("Regenerate",{exact:true}).click();expect((await frames(page)).at(-1)).toEqual({type:"chat.routed.regenerate",chat_id:"chat-one",from_message_id:"answer-one"});
  await expect(page.getByText("Routed answer",{exact:true})).toBeVisible();
  await emit(page,{type:"chat.created",chat_id:"branch-one"});
  await expect(page.getByText("Branch answer",{exact:true})).toBeVisible();await expect(page.getByText("Routed answer",{exact:true})).toHaveCount(0);
  expect(s.legacy).toEqual([]);
});
test("Saved Auto is preserved with the gate off, configuration failures and unavailable model",async({page})=>{
  const s=await fixture(page);s.enabled=false;await page.goto(url+"#/c/chat-one");
  await expect(page.getByLabel("Chat model",{exact:true})).toHaveValue("auto");await send(page);
  await expect(page.getByText("Browser model routing is disabled. Your saved choice is preserved.")).toBeVisible();expect(await frames(page)).toEqual([]);
  s.enabled=true;s.catalogFailed=true;await page.reload();await send(page);
  await expect(page.getByText("Routing configuration is unavailable. Retry before sending.")).toBeVisible();expect(await frames(page)).toEqual([]);
  s.catalogFailed=false;s.saved.selection={mode:"manual",provider_id:"gone"};await page.reload();
  await expect(page.getByLabel("Chat model",{exact:true})).toHaveValue("gone");await send(page);expect(await frames(page)).toEqual([]);
  await expect(page.getByText("The selected model is unavailable or has no current routing profile.")).toBeVisible();expect(s.legacy).toEqual([]);
});
test("Old server continues on legacy frame, while saved-selection read failure blocks sending",async({page})=>{
  const s=await fixture(page);s.old=true;await page.goto(url);await send(page);
  await expect.poll(()=>frames(page)).toHaveLength(1);expect((await frames(page))[0].type).toBe("chat.send");
  s.old=false;s.savedFailed=true;await page.goto(url+"#/c/chat-one");await page.reload();await send(page);
  await expect(page.getByText("Saved model selection is unavailable. Retry before sending.")).toBeVisible();
  expect((await frames(page)).filter((f:any)=>f.type==="chat.routed.send")).toEqual([]);
  s.savedFailed=false;s.capsFailed=true;await page.reload();await send(page);
  await expect(page.getByText("Model routing discovery is unavailable. Retry before sending.")).toBeVisible();expect(await frames(page)).toEqual([]);
});

test("Paired desktop uses remote Auto and reloads actual metadata; regeneration keeps intent",async({page})=>{
  const s=await fixture(page);await page.goto(url+"?desktop=1");
  await expect(page.getByLabel("Chat model",{exact:true})).toHaveValue("default");
  await page.getByLabel("Chat model",{exact:true}).selectOption("auto");await page.getByLabel("Local inference only").check();
  await page.getByLabel("Routing effort").selectOption("high");await send(page,"Remote desktop question");
  await expect.poll(()=>frames(page)).toHaveLength(1);const sent=(await frames(page))[0];
  expect(sent.type).toBe("chat.routed.send");expect(sent.request.model_selection.mode).toBe("auto");
  expect(sent.request.options).toEqual({local_only:true,effort:"high"});expect(sent.request.device_id).toBeUndefined();expect(sent.request.workspace_id).toBeUndefined();
  expect(s.deviceRequests.some(r=>r.authorization==="Bearer synthetic-device-token")).toBe(true);
  expect(s.deviceRequests.filter(r=>r.authorization).every(r=>r.cookie===undefined)).toBe(true);
  s.messages=[{id:"desktop-answer",role:"assistant",content:"Desktop routed answer",streaming:false,
    routing:{schema_version:1,selection_mode:"auto",actual:{model:"desktop-actual",effort:"high"},actual_unavailable:false,reason_codes:["rule_match"],status:"completed",local_only:true}}];
  await page.goto(url+"?desktop=1#/c/chat-one");await page.reload();
  await expect(page.getByLabel("Chat model",{exact:true})).toHaveValue("auto");await expect(page.getByText("desktop-actual",{exact:false})).toBeVisible();
  await expect(page.getByLabel("Local inference only")).toBeChecked();await expect(page.getByLabel("Routing effort")).toHaveValue("high");
  await page.screenshot({path:"test-results/chat-routing-desktop.png",fullPage:true});
  await page.getByTitle("Regenerate",{exact:true}).click();await expect.poll(()=>frames(page)).toHaveLength(1);
  expect((await frames(page))[0]).toEqual({type:"chat.routed.regenerate",chat_id:"chat-one",from_message_id:"desktop-answer"});
  expect(s.legacy).toEqual([]);
});

test("Desktop capability absence and local folders preserve routed intent; old server keeps legacy",async({page})=>{
  const s=await fixture(page);s.old=true;await page.goto(url+"?desktop=1");await send(page);
  await expect.poll(()=>frames(page)).toHaveLength(1);expect((await frames(page))[0].type).toBe("chat.send");
  s.old=false;s.desktop=false;await page.goto(url+"?desktop=1#/c/chat-one");await page.reload();await send(page);
  await expect(page.getByText("This server does not support desktop model routing. Your saved choice is preserved.")).toBeVisible();expect(await frames(page)).toEqual([]);
  s.desktop=true;s.workspace=true;await page.reload();await send(page);
  await expect(page.getByText("Model routing supports general remote conversations. This conversation requires a different execution path.")).toBeVisible();expect(await frames(page)).toEqual([]);
});

test("Typed routing failure preserves partial output, supports explicit retry and reload",async({page})=>{
  const s=await fixture(page);await page.goto(url);await send(page,"A retained question");
  await emit(page,{type:"chat.created",chat_id:"chat-one"});
  await emit(page,{type:"chat.routing_started",chat_id:"chat-one",turn_id:"failure-turn"});
  await emit(page,{type:"chat.started",chat_id:"chat-one",turn_id:"failure-turn",message_id:"failure-answer"});
  const failure={schema_version:1,code:"provider_rate_limited",source:"provider",before_output:true,can_retry:true};
  await emit(page,{type:"chat.routing_failure",turn_id:"another-turn",chat_id:"other-chat",message_id:"failure-answer",failure});
  await expect(page.getByRole("alert")).toHaveCount(0);
  await emit(page,{type:"chat.routing_failure",turn_id:"failure-turn",chat_id:"chat-one",message_id:"failure-answer",failure});
  await emit(page,{type:"chat.error",turn_id:"failure-turn",chat_id:"chat-one",message:"private-vendor-body"});
  await expect(page.getByRole("alert")).toContainText("rate limited");
  await expect(page.getByText("private-vendor-body")).toHaveCount(0);
  const before=(await frames(page)).length;await page.getByRole("button",{name:"Prepare retry",exact:true}).click();
  await expect(page.locator("textarea.comp-in")).toHaveValue("A retained question");expect((await frames(page)).length).toBe(before);
  s.messages=[{id:"user-one",role:"user",content:"A retained question",streaming:false},
    {id:"failure-answer",role:"assistant",content:"Partial output",streaming:false,routing:{schema_version:1,selection_mode:"auto",actual:null,
      actual_unavailable:false,reason_codes:[],status:"interrupted",failure:{...failure,before_output:false,can_retry:false}}}];
  await page.reload();await expect(page.getByRole("alert")).toContainText("received output is retained");
  await expect(page.getByText("Partial output",{exact:true})).toBeVisible();await expect(page.getByRole("button",{name:"Prepare retry"})).toHaveCount(0);
});
