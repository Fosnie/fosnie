import { test,expect,type Page } from "@playwright/test";
import type { ModelEntry,Profile,PolicyEntry,FixedEntry } from "../../src/api/routingSettings";
const url="/tests/document-jobs/routing-settings.html";

async function assertActionSpacing(page:Page) {
  const touching=await page.locator(".routing-settings button").evaluateAll(nodes=>{
    const buttons=nodes.filter(node=>node.checkVisibility() && !node.matches(".legal-tab,[role=option]"))
      .map(node=>({name:node.textContent?.trim(),box:node.getBoundingClientRect()}))
      .filter(({box})=>box.width>0 && box.height>0);
    const errors:string[]=[];
    for(let i=0;i<buttons.length;i++)for(let j=i+1;j<buttons.length;j++) {
      const a=buttons[i],b=buttons[j];
      const overlapY=Math.min(a.box.bottom,b.box.bottom)-Math.max(a.box.top,b.box.top);
      const overlapX=Math.min(a.box.right,b.box.right)-Math.max(a.box.left,b.box.left);
      const gapX=Math.max(a.box.left,b.box.left)-Math.min(a.box.right,b.box.right);
      const gapY=Math.max(a.box.top,b.box.top)-Math.min(a.box.bottom,b.box.bottom);
      if((overlapY>8 && gapX<7.5)||(overlapX>8 && gapY<7.5))errors.push(`${a.name} / ${b.name}`);
    }
    return errors;
  });
  expect(touching,"Visible action buttons must be separated").toEqual([]);
  expect(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth)).toBe(true);
}

for (const surface of ["deployment","personal"] as const) for (const width of [1280,390])
test(`All ${surface} routing editors and card actions are spaced at ${width}px`,async({page})=>{
  const s=await fixture(page);const scope=surface==="personal"?{kind:"user",owner_id:own}:{kind:"deployment"};
  const connection={id:"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",scope:surface==="personal"?"user":"deployment",scope_id:surface==="personal"?own:null,label:"Sample connection",revision:1,auth_revision:1,adapter:"openai_chat",base_url:"https://example.invalid/v1",enabled:true,api_key_set:false,quota_group_id:null,metadata:{locality:"cloud",locality_trusted:false}};
  const model={id:cloud,label:"Sample model",scope,revision:1,provider_revision:1,model_config_revision:1,enabled:true,model:"sample-model",connection:{id:connection.id,auth_revision:1},reasoning_mode:null,is_default:false,adapter:"openai_chat",binding_current:true,capabilities:{efforts:["high"]},metadata:{locality:"cloud"},tariffs:{input_microusd_per_million:1000000}};
  const variant={id:"bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",revision:1,scope,provider_id:cloud,model_config_revision:1,label:"Sample variant",behaviour_sha256:"sample",behaviour:{adapter:"openai_chat",model_id:"sample-model",effort:{kind:"provider_default"},generation:{}}};
  await page.route("**/api/routing/v2/**",route=>{
    const path=new URL(route.request().url()).pathname;
    return route.fulfill({json:path.endsWith("/catalogue")?{schema_version:2,catalogue_revision:1,selection_versions:[2],profile_versions:[2],max_active_variants:256,deployments:[model],variants:[{variant,configuration_current:true,binding_current:true,calibration_state:"declared"}]}
      :path.endsWith("/connections")?[connection]:path.endsWith("/availability")?{models:[]}:[]});
  });
  await page.setViewportSize({width,height:900});await page.goto(`${url}?surface=${surface}`);
  const builder=page.getByRole("region",{name:"Profiles and catalogue"});
  await expect(builder.getByRole("button",{name:"Everyday preset",exact:true})).toBeEnabled();
  await assertActionSpacing(page);
  await builder.getByRole("button",{name:"Everyday preset",exact:true}).click();
  await page.getByRole("button",{name:"Add routing rule",exact:true}).click();
  await page.getByText("Additional conditions",{exact:true}).click();
  await page.getByText("Condition groups",{exact:true}).click();
  await assertActionSpacing(page);await page.getByRole("button",{name:"Close",exact:true}).click();

  await builder.getByRole("button",{name:"Models",exact:true}).click();await assertActionSpacing(page);
  for(const action of ["New model deployment","Edit model","Metadata","Tariffs"]) {
    await builder.getByRole("button",{name:action,exact:true}).click();await assertActionSpacing(page);
    await page.getByRole("button",{name:"Close",exact:true}).click();
  }
  await builder.getByRole("button",{name:"Variants",exact:true}).click();await assertActionSpacing(page);
  for(const action of [`New ${surface==="personal"?"personal":"deployment"} variant`,"Edit"]) {
    await builder.getByRole("button",{name:action,exact:true}).click();await assertActionSpacing(page);
    await page.screenshot({path:`test-results/routing-variant-editor-${surface}-${width}.png`,fullPage:true,animations:"disabled"});
    await page.getByRole("button",{name:"Close",exact:true}).click();
  }
  await builder.getByRole("button",{name:"Connections",exact:true}).click();await assertActionSpacing(page);
  for(const action of ["New connection","Edit"]) {
    await builder.getByRole("button",{name:action,exact:true}).click();await assertActionSpacing(page);
    await page.getByRole("button",{name:"Close",exact:true}).click();
  }
  expect(s.requests.filter(r=>r.method!=="GET")).toHaveLength(0);
});
test("Model roles and legacy policies save without evaluation controls or inference",async({page})=>{
  const s=await fixture(page);await page.goto(url);await page.getByText("Model capabilities, helper services and legacy policies",{exact:true}).click();
  await page.locator(".routing-card").filter({hasText:"Cloud reasoning"}).getByRole("button",{name:"Edit profile",exact:true}).click();
  await page.getByLabel("Model tier",{exact:true}).selectOption("advanced");
  await expect(page.getByLabel("Import evaluation JSON")).toHaveCount(0);
  await page.getByRole("button",{name:"Save",exact:true}).click();await expect(page.getByRole("status")).toContainText("saved");
  expect(s.models[0].profile!.tier).toBe("advanced");
  await page.getByRole("button",{name:"policies",exact:true}).click();await page.getByRole("button",{name:"New deployment policy"}).click();
  await page.getByLabel("Policy name").fill("My rules");await page.getByLabel("Checked default model").selectOption(cloud);
  await expect(page.getByLabel("Rank with reviewed model evaluations")).toHaveCount(0);
  await expect(page.getByLabel("Quality escalation for stateless API")).toHaveCount(0);
  await page.getByRole("button",{name:"Save",exact:true}).click();await expect(page.getByRole("status")).toContainText("saved");
  expect(s.policies.at(-1)!.policy).toMatchObject({learned_ranking:false,quality_escalation:false,rollout:"off"});
  expect(s.requests.filter(r=>r.method==="POST")).toHaveLength(0);
});
const cloud="11111111-1111-4111-8111-111111111111",local="22222222-2222-4222-8222-222222222222",own="33333333-3333-4333-8333-333333333333";
for (const width of [1280,390]) test(`Catalogue import and model actions stay usable at ${width}px`,async({page})=>{
  const s=await fixture(page);
  await page.setViewportSize({width,height:900});
  await page.route("**/api/routing/v2/**",route=>{
    const path=new URL(route.request().url()).pathname;
    return route.fulfill({json:path.endsWith("/catalogue")
      ? {schema_version:2,catalogue_revision:1,selection_versions:[2],profile_versions:[2],max_active_variants:256,deployments:[],variants:[]}
      : path.endsWith("/availability") ? {models:[]} : []});
  });
  await page.goto(url);
  const importControl=page.locator(".routing-import");
  await expect(importControl).toHaveText("Import profile");
  await expect(importControl.locator("input")).toHaveAccessibleName("Import profile");
  await page.screenshot({path:`test-results/routing-import-${width}.png`,fullPage:true,animations:"disabled"});
  const payload={schema_version:2,label:"Imported profile",policy:{...s.policies[0].policy,default_provider_id:null,classifier_provider_id:null},config:{schema_version:2,membership:{kind:"all_available"},max_active_variants:256,quality:{assurance:"declared",max_relative_loss_bp:0,minimum_absolute_quality_bp:null},reference:{kind:"automatic",default_variant:null,allow_uncalibrated_default:true},fallback_variant_ids:[],groups:[],selector_sha256:null,verifier_sha256:null}};
  for(let attempt=0;attempt<2;attempt++) {
    const choosing=page.waitForEvent("filechooser");await importControl.click();
    await (await choosing).setFiles({name:"routing-profile.json",mimeType:"application/json",buffer:Buffer.from(JSON.stringify(payload))});
    await expect(page.getByRole("heading",{name:"Import profile · map source IDs",exact:true})).toBeVisible();
    await assertActionSpacing(page);
    await page.getByRole("button",{name:"Close",exact:true}).click();
  }
  await page.getByRole("button",{name:"Models",exact:true}).click();
  const refresh=await page.getByRole("button",{name:"Refresh reported availability",exact:true}).boundingBox();
  const create=await page.getByRole("button",{name:"New model deployment",exact:true}).boundingBox();
  expect(refresh).not.toBeNull();expect(create).not.toBeNull();
  const gap=create!.y>=refresh!.y+refresh!.height-1 ? create!.y-refresh!.y-refresh!.height : create!.x-refresh!.x-refresh!.width;
  expect(gap).toBeGreaterThanOrEqual(8);
  expect(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth)).toBe(true);
  await page.screenshot({path:`test-results/routing-model-actions-${width}.png`,fullPage:true,animations:"disabled"});
});
const constraints={allowed_providers:null,local_only:false,max_calls:null,max_output_tokens:null,max_total_tokens:null,max_duration_ms:null,max_cost_microusd:null};
function profile(id:string,model:string):Profile {
  return {provider_id:id,provider_revision:1,revision:5,model,adapter:"openai_chat",locality:"cloud",locality_trusted:false,auto_enabled:true,
    capabilities:{tools:"declared",vision:"unknown",structured_output:"declared",context_tokens:32000,max_output_tokens:8192,efforts:["high"]},
    tier:"balanced",task_tiers:{},quality_status:"declared",evaluation_samples:0,evaluation_version:null,latency_ms:null,
    pricing:{input_microusd_per_million:1000000,output_microusd_per_million:2000000,source:null,checked_at:null}};
}
async function fixture(page:Page) {
  page.on("pageerror",e=>console.error("Routing settings page error:",e.message));
  const models:ModelEntry[]=[{id:cloud,label:"Cloud reasoning",model:"cloud-api-model",scope:"deployment",enabled:true,provider_revision:1,profile_revision:5,profile:profile(cloud,"cloud-api-model")},
    {id:local,label:"Local small",model:"local-api-model",scope:"deployment",enabled:true,provider_revision:2,profile_revision:null,profile:null},
    {id:own,label:"Personal model",model:"personal-api-model",scope:"user",enabled:true,provider_revision:1,profile_revision:5,profile:profile(own,"personal-api-model")}];
  const policies:PolicyEntry[]=[{id:"44444444-4444-4444-8444-444444444444",label:"Shared hybrid",scope:"deployment",revision:10,is_default:true,
    policy:{id:"44444444-4444-4444-8444-444444444444",revision:10,rollout:"off",strategy:"hybrid",objective:"economy",constraints:{...constraints},rules:[],default_provider_id:cloud,classifier_provider_id:cloud,utility_assignments:{},technical_fallback:false}}];
  const fixed:FixedEntry[]=[{id:"55555555-5555-4555-8555-555555555555",role:"embed",scope:"deployment",enabled:true,model:"indexed-embedding",provider_revision:1,active_index_revision:7,desired_index_revision:null,profile_revision:null,profile:null},
    {id:"55555555-5555-4555-8555-555555555555",role:"embed_desired",scope:"deployment",enabled:true,model:"next-embedding",provider_revision:1,active_index_revision:null,desired_index_revision:8,profile_revision:null,profile:null}];
  const s={manager:true,byok:true,failed:false,conflict:false,enabled:false,aliasConflict:false,apiEnabled:false,
    alias:{schema_version:1,name:"fosnie/auto",enabled:false,revision:7,objective:"balanced",status:"disabled",agent_field:"fosnie_agent_id"},
    models,policies,fixed,requests:[] as {path:string;method:string;body:any}[]};
  await page.route("**/api/**",async route=>{
    const request=route.request();const path=new URL(request.url()).pathname;if(!path.startsWith("/api/"))return route.continue();
    const method=request.method();const body=method==="GET" ? null : request.postDataJSON();s.requests.push({path,method,body});
    const json=(value:unknown,status=200)=>route.fulfill({status,json:value});
    if(path==="/api/auth/config")return json({mode:"local",local_enabled:true});
    if(path==="/api/routing/management")return s.failed ? json({},503) : json({schema_version:1,deployment_manage:s.manager,user_byok_enabled:s.byok,personal_policies:true});
    if(path==="/api/routing/catalog")return json(s.models.filter(m=>s.byok || m.scope==="deployment"));
    if(path==="/api/routing/policies")return json(s.policies);
    if(path==="/api/routing/fixed-catalog")return json(s.fixed);
    if(path==="/api/routing/public-api") {if(method==="PUT")s.apiEnabled=body.enabled;return json({schema_version:1,enabled:s.apiEnabled});}
    if(path==="/api/routing/public-api-alias") {
      if(method==="PUT") {if(s.aliasConflict)return json({},409);s.alias={...s.alias,...body,revision:s.alias.revision+1,status:body.enabled ? s.apiEnabled ? "ready" : "routing_disabled" : "disabled"};}
      return json({...s.alias,status:s.alias.enabled ? s.apiEnabled ? "ready" : "routing_disabled" : "disabled"});
    }
    if(path==="/api/routing/browser-chat" || path==="/api/routing/regenerate") {if(method==="PUT")s.enabled=body.enabled;return json({schema_version:1,enabled:s.enabled});}
    if(path.startsWith("/api/routing/profiles/")) {
      if(path.endsWith("/probe"))return json({compatible:true,quality_evaluated:false,inference_calls:1,usage:{prompt_tokens:10,completion_tokens:1}});
      const entry=s.models.find(m=>path.endsWith(m.id))!;
      if(s.conflict){s.conflict=false;entry.profile_revision=6;entry.profile={...entry.profile!,revision:6};return json({},409);}
      entry.profile={...body.profile,revision:20};entry.profile_revision=20;return json(entry.profile);
    }
    if(path.startsWith("/api/routing/policies/")) {
      const saved={id:body.policy.id,label:body.label,scope:body.scope,revision:21,is_default:body.is_default,policy:{...body.policy,revision:21}};
      const index=s.policies.findIndex(p=>p.id===saved.id);if(index>=0)s.policies[index]=saved;else s.policies.push(saved);return json(saved.policy);
    }
    if(path.startsWith("/api/routing/fixed-profiles/")) {
      const entry=s.fixed.find(f=>f.id===body.profile.provider_id && f.role===body.profile.role)!;
      entry.profile={...body.profile,revision:22};entry.profile_revision=22;return json(entry.profile);
    }
    if(path==="/api/routing/preview")return json({dry_run:true,inference_calls:0,classifier_required:false,rollout:"off",execution_target:{model:"cloud-api-model",effort:null},
      decision:{proposed:null,actual:null,policy_revision:10,reason_codes:["routing_off"],estimated_cost_microusd:null},requirements:body.requirements,constraints});
    if(path==="/api/routing/classifier-probe")return json({ok:true,judge_model:"cloud-api-model",requirements:{task:"current_fact",minimum_tier:"economy",fresh_information:true,tools:true,vision:false,structured_output:false,uncertain:false},usage:{calls:1,tokens:400,cost_microusd:123,unknown_cost_calls:0,exhausted:false},answer_calls:0,quality_evaluated:false});
    return json([]);
  });return s;
}
test("Management loads without paid calls or gate changes; profiles retain drafts on conflict",async({page})=>{
  const s=await fixture(page);await page.goto(url);await page.getByText("Model capabilities, helper services and legacy policies",{exact:true}).click();await expect(page.getByText("Cloud reasoning",{exact:true})).toBeVisible();
  expect(s.requests.filter(r=>r.method!=="GET")).toHaveLength(0);await expect(page.getByText("Personal model",{exact:true})).toHaveCount(0);
  await page.locator(".routing-card").filter({hasText:"Local small"}).getByRole("button",{name:"Edit profile",exact:true}).click();
  await page.getByLabel("Context tokens",{exact:true}).fill("32000");await page.getByLabel("Maximum output tokens",{exact:true}).fill("4096");
  await page.getByLabel("Quality evidence",{exact:true}).selectOption("declared");await page.getByLabel("Locality",{exact:true}).selectOption("local");
  await page.getByLabel("Administrator attests locality",{exact:true}).check();await page.getByLabel("Eligible for Auto").check();
  await page.getByLabel("Supported efforts").pressSequentially("high, xhigh");await page.getByLabel("Auto effort preset",{exact:true}).selectOption("xhigh");await page.getByLabel("Input price (USD / million tokens)").fill("0");
  await page.getByRole("button",{name:"Save",exact:true}).click();await expect(page.getByRole("status")).toContainText("saved");
  const saved=s.requests.find(r=>r.method==="PUT" && r.path.endsWith(local))!.body;
  expect(saved.expected_revision).toBe(0);expect(saved.profile.provider_revision).toBe(2);expect(saved.profile.model).toBe("local-api-model");
  expect(saved.profile.auto_effort).toBe("xhigh");expect(saved.profile.capabilities.efforts).toEqual(["high","xhigh"]);expect(saved.profile.pricing.input_microusd_per_million).toBe(0);expect(saved.profile.pricing.output_microusd_per_million).toBeNull();
  expect(saved.profile.capabilities.tools).toBe("unknown");expect(saved.profile.quality_status).toBe("declared");
  await page.locator(".routing-card").filter({hasText:"Cloud reasoning"}).getByRole("button",{name:"Edit profile",exact:true}).click();
  await page.getByLabel("Model tier",{exact:true}).selectOption("advanced");s.conflict=true;await page.getByRole("button",{name:"Save",exact:true}).click();
  await expect(page.getByRole("alert")).toContainText("draft is preserved");await expect(page.getByLabel("Model tier",{exact:true})).toHaveValue("advanced");
  await page.getByRole("button",{name:"Reload latest (discard draft)",exact:true}).click();await expect(page.getByLabel("Model tier",{exact:true})).toHaveValue("balanced");
  await page.screenshot({path:"test-results/routing-settings-profile.png",fullPage:true});
  await page.getByRole("button",{name:"Save",exact:true}).click();await expect(page.getByRole("status")).toContainText("saved");
  expect(s.requests.filter(r=>r.path.endsWith(cloud) && r.method==="PUT").at(-1)!.body.expected_revision).toBe(6);
  await page.getByText("Routing rollout gates",{exact:true}).click();await page.getByRole("button",{name:"Enable browser routed chat",exact:true}).click();
  expect(s.requests.filter(r=>r.path==="/api/routing/browser-chat" && r.method==="PUT")).toHaveLength(1);
  expect(s.requests.filter(r=>r.method==="POST")).toHaveLength(0);
});
test("Policy rules, separate embedding stages and explicit bounded previews use saved revisions",async({page})=>{
  const s=await fixture(page);await page.goto(url);await page.getByText("Model capabilities, helper services and legacy policies",{exact:true}).click();await page.getByRole("button",{name:"policies",exact:true}).click();await page.getByRole("button",{name:"New deployment policy"}).click();
  await page.getByLabel("Policy name").fill("Weather routing");await page.getByLabel("Strategy",{exact:true}).selectOption("hybrid");await page.getByLabel("Rollout",{exact:true}).selectOption("shadow");
  await page.getByLabel("Checked default model").selectOption(cloud);await page.getByLabel("Judge model",{exact:true}).selectOption(cloud);await page.getByLabel("Run cost limit (USD)").fill("0.01");
  await page.getByLabel("Restrict provider pool").check();await page.getByLabel("Cloud reasoning",{exact:true}).check();await page.getByLabel("Local small",{exact:true}).check();
  await page.getByRole("button",{name:"Add rule",exact:true}).click();await page.getByLabel("Priority",{exact:true}).fill("20");await page.getByLabel("Rule action").selectOption("select");
  await page.getByLabel("Rule model").selectOption(local);await page.getByLabel("Languages",{exact:true}).pressSequentially("en, ru");await page.getByLabel("current fact",{exact:true}).check();
  await page.getByRole("button",{name:"Save",exact:true}).click();await expect(page.getByRole("status")).toContainText("saved");
  const body=s.requests.find(r=>r.path.startsWith("/api/routing/policies/") && r.method==="PUT")!.body;
  expect(body.expected_revision).toBe(0);expect(body.scope).toBe("deployment");expect(body.policy.classifier_provider_id).toBe(cloud);
  expect(body.policy.constraints.allowed_providers).toEqual([cloud,local]);expect(body.policy.constraints.max_cost_microusd).toBe(10000);
  expect(body.policy.rules[0]).toMatchObject({priority:20,conditions:{languages:["en","ru"],tasks:["current_fact"]},action:{kind:"select",provider_id:local}});
  await page.getByRole("button",{name:"fixed services",exact:true}).click();await page.locator(".routing-card").filter({hasText:"embed desired"}).getByRole("button",{name:"Edit fixed profile"}).click();
  await expect(page.getByLabel("Fixed service model")).toHaveValue("next-embedding");await page.getByLabel("Fixed service locality",{exact:true}).selectOption("local");await page.getByLabel("Administrator attests fixed service locality").check();
  await page.getByRole("button",{name:"Save",exact:true}).click();await expect(page.getByRole("status")).toContainText("saved");
  const fixed=s.requests.find(r=>r.path.startsWith("/api/routing/fixed-profiles/"))!.body;
  expect(fixed.profile).toMatchObject({role:"embed_desired",desired_index_revision:8,active_index_revision:null,model:"next-embedding",locality_trusted:true});
  await page.getByRole("button",{name:"preview",exact:true}).click();await page.getByLabel("Preview saved policy").click();
  await expect(page.getByRole("listbox")).toHaveClass(/dd-menu/);
  await page.screenshot({path:"test-results/routing-policy-menu.png",fullPage:true,animations:"disabled"});
  await page.getByRole("option",{name:"Shared hybrid · deployment · revision 10",exact:true}).click();
  await page.getByRole("button",{name:"Preview without inference"}).click();await expect(page.getByRole("status")).toContainText("Inference calls: 0");
  expect(s.requests.filter(r=>r.path==="/api/routing/classifier-probe")).toHaveLength(0);
  await page.getByLabel("Judge test conversation").fill("Weather in Glasgow?");await page.getByLabel("Judge cost cap (USD)").fill("0.001");
  await page.getByRole("button",{name:"Run one judge call",exact:true}).click();await expect(page.getByText("Answer calls: 0.",{exact:false})).toBeVisible();
  const judge=s.requests.find(r=>r.path==="/api/routing/classifier-probe")!.body;expect(judge.max_cost_microusd).toBe(1000);expect(judge.expected_policy_revision).toBe(10);
  await page.screenshot({path:"test-results/routing-settings-preview.png",fullPage:true});
  await page.getByRole("button",{name:"models",exact:true}).click();await page.locator(".routing-card").filter({hasText:"Cloud reasoning"}).getByRole("button",{name:"Test compatibility"}).click();
  await page.getByLabel("Probe cost cap (USD)").fill("0.001");await page.getByRole("button",{name:"Run one synthetic model call"}).click();await expect(page.getByRole("status")).toContainText("Quality was not evaluated");
  expect(s.requests.filter(r=>r.path.endsWith("/probe"))).toHaveLength(1);
});
test("Personal policies remain available with BYOK off; shared writes and failed discovery are blocked",async({page})=>{
  const s=await fixture(page);s.manager=false;s.byok=false;s.failed=true;await page.goto(`${url}?surface=personal`);
  await expect(page.getByRole("alert")).toContainText("unavailable");expect(s.requests.filter(r=>r.path.endsWith("/catalog"))).toHaveLength(0);
  s.failed=false;await page.getByRole("button",{name:"Retry routing management"}).click();
  await page.getByText("Model capabilities, helper services and legacy policies",{exact:true}).click();await expect(page.getByText("Cloud reasoning",{exact:true})).toBeVisible();
  await expect(page.getByRole("button",{name:"Edit profile",exact:true})).toHaveCount(0);await expect(page.getByText("Personal model",{exact:true})).toHaveCount(0);
  await page.getByRole("button",{name:"policies",exact:true}).click();await expect(page.getByRole("button",{name:"Edit policy",exact:true})).toHaveCount(0);
  await page.getByRole("button",{name:"New personal policy"}).click();await page.getByLabel("Policy name").fill("My cloud policy");await page.getByLabel("Checked default model").selectOption(cloud);
  await page.getByRole("button",{name:"Save",exact:true}).click();await expect(page.getByRole("status")).toContainText("saved");
  expect(s.requests.find(r=>r.path.startsWith("/api/routing/policies/") && r.method==="PUT")!.body.scope).toBe("user");
  expect(s.requests.filter(r=>r.method==="POST")).toHaveLength(0);
  await page.setViewportSize({width:390,height:844});await page.screenshot({path:"test-results/routing-settings-personal-mobile.png",fullPage:true});
  expect(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth)).toBe(true);
});
test("API alias activation is explicit, collision conflicts preserve the draft and gate stays separate",async({page})=>{
  const s=await fixture(page);await page.goto(url);await page.getByText("Model capabilities, helper services and legacy policies",{exact:true}).click();await page.getByText("API Auto alias",{exact:true}).click();
  expect(s.requests.filter(r=>r.method!=="GET")).toHaveLength(0);
  await page.getByLabel("API alias objective",{exact:true}).click();
  await expect(page.getByRole("listbox")).toHaveClass(/dd-menu/);
  await page.screenshot({path:"test-results/routing-alias-menu.png",fullPage:true,animations:"disabled"});
  await page.getByRole("option",{name:"quality",exact:true}).click();
  await page.getByLabel("API Auto alias name",{exact:true}).fill("fosnie-route/auto");await page.getByLabel("Enable API Auto alias",{exact:true}).check();
  s.aliasConflict=true;await page.getByRole("button",{name:"Save API alias",exact:true}).click();
  await expect(page.getByRole("alert")).toContainText("draft is preserved");await expect(page.getByLabel("API Auto alias name",{exact:true})).toHaveValue("fosnie-route/auto");
  s.aliasConflict=false;await page.getByRole("button",{name:"Save API alias",exact:true}).click();await expect(page.getByText("Alias is configured, but API routing is disabled.",{exact:false})).toBeVisible();
  expect(s.requests.filter(r=>r.path==="/api/routing/public-api-alias" && r.method==="PUT")[0].body.expected_revision).toBe(7);
  expect(s.requests.filter(r=>r.path==="/api/routing/public-api-alias" && r.method==="PUT")[0].body.objective).toBe("quality");
  expect(s.requests.filter(r=>r.path==="/api/routing/public-api" && r.method==="PUT")).toHaveLength(0);
  await page.getByRole("button",{name:"Enable API routing",exact:true}).click();await expect(page.getByText("Saved status: ready.",{exact:false})).toBeVisible();
  expect(s.requests.filter(r=>r.method==="POST")).toHaveLength(0);
  await page.screenshot({path:"test-results/routing-api-alias.png",fullPage:true});
});

test("Nested rules, main model pool and judge limits survive save and reload",async({page})=>{
  const s=await fixture(page);await page.goto(url);await page.getByText("Model capabilities, helper services and legacy policies",{exact:true}).click();await page.getByRole("button",{name:"policies",exact:true}).click();
  await page.getByRole("button",{name:"New deployment policy"}).click();await page.getByLabel("Policy name").fill("Local executor with cloud judge");
  await page.getByLabel("Strategy",{exact:true}).selectOption("hybrid");await page.getByLabel("Checked default model").selectOption(local);
  await page.getByLabel("Judge model",{exact:true}).selectOption(cloud);await page.getByText("Judge limits",{exact:true}).click();
  await page.getByLabel("Judge effort",{exact:true}).selectOption("high");await page.getByLabel("Require native JSON schema from judge",{exact:true}).check();
  await page.getByLabel("Judge input budget",{exact:true}).fill("8192");await page.getByLabel("Judge output limit",{exact:true}).fill("256");
  await page.getByLabel("Local judge timeout (ms)", {exact:true}).fill("6000");await page.getByLabel("Cloud judge timeout (ms)", {exact:true}).fill("2000");
  await page.getByLabel("Technical fallback before output",{exact:true}).check();await page.getByRole("button",{name:"Add rule",exact:true}).click();
  await page.getByLabel("Rule action",{exact:true}).selectOption("restrict_pool");await page.getByLabel("Local small",{exact:true}).check();
  await page.getByLabel("fresh information condition",{exact:true}).selectOption("yes");
  await page.getByText("Condition groups",{exact:true}).click();await page.getByRole("button",{name:"Add all condition",exact:true}).click();
  const all=page.getByRole("group",{name:"ALL condition 1",exact:true});await all.getByLabel("tools condition",{exact:true}).selectOption("yes");
  await page.getByRole("button",{name:"Add any condition",exact:true}).first().click();
  const any=page.getByRole("group",{name:"ANY condition 1",exact:true});await any.getByLabel("Languages",{exact:true}).fill("ru, en-GB");
  await any.getByLabel("current fact",{exact:true}).check();await page.getByRole("button",{name:"Save",exact:true}).click();
  await expect(page.getByRole("status")).toContainText("saved");
  const body=s.requests.filter(r=>r.method==="PUT" && r.path.startsWith("/api/routing/policies/")).at(-1)!.body;
  expect(body.policy.classifier).toEqual({native_schema:true,effort:"high",min_confidence:0.75,max_input_tokens:8192,max_output_tokens:256,local_timeout_ms:6000,cloud_timeout_ms:2000});
  expect(body.policy.technical_fallback).toBe(true);expect(body.policy.classifier_provider_id).toBe(cloud);
  expect(body.policy.rules[0]).toMatchObject({action:{kind:"restrict_pool",allowed_providers:[local]},conditions:{fresh_information:true,
    all:[{tools:true}],any:[{languages:["ru","en-GB"],tasks:["current_fact"]}]}});
  expect(body.policy.rollout).toBe("off");expect(s.requests.filter(r=>r.method==="POST")).toHaveLength(0);
  await page.locator(".routing-card").filter({hasText:"Local executor with cloud judge"}).getByRole("button",{name:"Edit policy",exact:true}).click();
  await page.getByText("Judge limits",{exact:true}).click();await expect(page.getByLabel("Judge input budget",{exact:true})).toHaveValue("8192");await expect(page.getByLabel("Judge effort",{exact:true})).toHaveValue("high");await expect(page.getByLabel("Require native JSON schema from judge",{exact:true})).toBeChecked();
  await expect(page.getByRole("group",{name:"ALL condition 1",exact:true}).getByLabel("tools condition",{exact:true})).toHaveValue("yes");
  await expect(page.getByRole("group",{name:"ANY condition 1",exact:true}).getByLabel("Languages",{exact:true})).toHaveValue("ru, en-GB");
  await page.setViewportSize({width:390,height:844});expect(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth)).toBe(true);
  await page.screenshot({path:"test-results/routing-nested-rules-mobile.png",fullPage:true});
});
