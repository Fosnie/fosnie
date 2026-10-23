import {test,expect,type Page} from "@playwright/test";
import type {Catalogue,RoutingProfile,Variant} from "../../src/api/routingCatalogue";
const provider="81111111-1111-4111-8111-111111111111";
const low="82222222-2222-4222-8222-222222222222", high="83333333-3333-4333-8333-333333333333";
const variant=(id:string,effort:string):Variant=>({id,revision:3,scope:{kind:"deployment"},provider_id:provider,model_config_revision:2,label:`Fixture ${effort}`,
 behaviour_sha256:"a".repeat(64),behaviour:{adapter:"openai_chat",model_id:"fixture-model",effort:{kind:"explicit",value:effort},generation:{max_output_tokens:512}}});
async function fixture(page:Page){
 const catalogue:Catalogue={schema_version:2,catalogue_revision:9,selection_versions:[1,2],profile_versions:[1,2],max_active_variants:256,
 deployments:[{id:provider,label:"Fixture model",scope:{kind:"deployment"},revision:2,provider_revision:2,model_config_revision:2,enabled:true,model:"fixture-model",connection:null,
 reasoning_mode:null,is_default:false,adapter:"openai_chat",binding_current:true,capabilities:{efforts:["low","high"]},metadata:{},tariffs:{}}],
 variants:[low,high].map((id,i)=>({variant:variant(id,i?"high":"low"),configuration_current:true,binding_current:true,calibration_state:"configured"}))};
 const state={profiles:[] as RoutingProfile[],conflict:false,saves:[] as any[],variantSaves:[] as any[],requests:[] as string[]};
 await page.route("**/api/**",async route=>{
  const req=route.request(),path=new URL(req.url()).pathname;
  if(!path.startsWith("/api/"))return route.continue();
  state.requests.push(req.method()+" "+path);
  const reply=(json:unknown,status=200)=>route.fulfill({json,status});
  if(path==="/api/auth/config")return reply({mode:"local",local_enabled:true});
  if(path==="/api/routing/management")return reply({schema_version:1,deployment_manage:false,user_byok_enabled:false,personal_policies:true});
  if(path==="/api/routing/v2/catalogue")return reply(catalogue);
  if(path==="/api/routing/v2/connections")return reply([]);
  if(path==="/api/routing/v2/profiles")return reply(state.profiles);
  if(path==="/api/routing/v2/availability")return reply({schema_version:2,models:[],generation_calls:0});
  if(path.startsWith("/api/routing/v2/variants/") && req.method()==="PUT"){
   state.variantSaves.push(req.postDataJSON());return reply({});
  }
  if(path.startsWith("/api/routing/v2/profiles/") && req.method()==="PUT"){
   const body=req.postDataJSON();state.saves.push(body);if(state.conflict)return reply({message:"changed revision"},409);
   const id=path.split("/").at(-1)!;const saved={...body,id,revision:7,policy:{...body.policy,revision:7},calibration_state:"configured"} as RoutingProfile;
   state.profiles=state.profiles.filter(p=>p.id!==id).concat(saved);return reply(saved);
  }
  if(path==="/api/routing/catalog" || path==="/api/routing/policies" || path==="/api/routing/fixed-catalog")return reply([]);
  return reply([]);
 });return state;
}
test("ordinary users without BYOK build personal complexity rules and preserve conflicted drafts",async({page})=>{
 const s=await fixture(page);await page.goto("/tests/document-jobs/routing-settings.html?surface=personal");
 const panel=page.getByRole("region",{name:"Profiles and catalogue"});
 await expect(panel.getByRole("button",{name:"Everyday preset"})).toBeVisible();
 await expect(panel.getByRole("button",{name:"Thorough preset"})).toBeVisible();
 await panel.getByRole("button",{name:"Custom Auto template",exact:true}).click();
 await page.getByLabel("Profile name",{exact:true}).fill("My evening and work models");
 await page.getByRole("button",{name:"Add routing rule",exact:true}).click();
 const group=page.getByRole("group",{name:"Task group",exact:true});
 await group.getByLabel("Name",{exact:true}).fill("Premium routine answers");
 await page.getByRole("combobox",{name:"Assignment",exact:true}).selectOption("pin");
 await page.getByRole("combobox",{name:"Assigned variant",exact:true}).selectOption(high);
 await group.getByLabel("Match task complexity",{exact:true}).check();
 await group.getByRole("group",{name:"Difficulty levels"}).getByLabel("standard",{exact:true}).uncheck();
 await group.getByRole("group",{name:"Difficulty levels"}).getByLabel("routine",{exact:true}).check();
 s.conflict=true;await page.getByRole("button",{name:"Save profile",exact:true}).click();
 await expect(page.getByRole("alert")).toContainText("draft is preserved");
 await expect(page.getByLabel("Profile name",{exact:true})).toHaveValue("My evening and work models");
 s.conflict=false;await page.getByRole("button",{name:"Save profile",exact:true}).click();
 await expect(panel.getByRole("status")).toContainText("saved");
 const saved=s.saves.at(-1);expect(saved.scope).toBe("user");expect(saved.policy.strategy).toBe("hybrid");
 expect(saved.config.groups[0]).toMatchObject({assignment:{kind:"pin",variant:{id:high,revision:3}},difficulty:{levels:["routine"]}});
 expect(saved.config.selector_sha256).toBeNull();expect(saved.policy.learned_ranking).toBe(false);
 expect(s.requests.some(r=>/profiling|quality|generate|classify/.test(r))).toBe(false);
 await panel.getByRole("button",{name:"Connections",exact:true}).click();
 await expect(panel.getByRole("button",{name:"New connection"})).toHaveCount(0);
 await panel.getByRole("button",{name:"Variants",exact:true}).click();
 await panel.getByRole("button",{name:"New personal variant",exact:true}).click();
 const variantEditor=page.getByRole("form",{name:"variant editor"});
 await variantEditor.getByLabel("Name",{exact:true}).fill("My high effort");
 await variantEditor.getByLabel("Effort",{exact:true}).selectOption("high");
 await variantEditor.getByRole("button",{name:"Save",exact:true}).click();
 await expect(panel.getByRole("status")).toContainText("saved");
 expect(s.variantSaves.at(-1)).toMatchObject({scope:"user",provider_id:provider,effort:{kind:"explicit",value:"high"}});
});
test("Full Auto and presets save without calibration and fit desktop and mobile",async({page})=>{
 const s=await fixture(page);await page.goto("/tests/document-jobs/routing-settings.html?surface=personal");
 const panel=page.getByRole("region",{name:"Profiles and catalogue"});
 await panel.getByRole("button",{name:"Thorough preset",exact:true}).click();
 await expect(page.getByLabel("Default preference")).toHaveValue("quality");
 await page.getByRole("button",{name:"Save profile",exact:true}).click();
 await expect(panel.getByRole("status")).toContainText("saved");
 expect(s.saves.at(-1).config.membership).toEqual({kind:"all_available"});
 await panel.getByRole("button",{name:"Full Auto template",exact:true}).click();
 await page.getByLabel("Profile name",{exact:true}).fill("All my models");
 await page.getByRole("button",{name:"Add routing rule",exact:true}).click();
 await page.setViewportSize({width:1440,height:1000});
 if(process.env.FOSNIE_ROUTING_SCREENSHOT_DIR)await page.screenshot({path:process.env.FOSNIE_ROUTING_SCREENSHOT_DIR+"/routing-desktop.png",fullPage:true});
 await page.setViewportSize({width:390,height:844});
 await expect(page.getByRole("button",{name:"Save profile",exact:true})).toBeVisible();
 if(process.env.FOSNIE_ROUTING_SCREENSHOT_DIR)await page.screenshot({path:process.env.FOSNIE_ROUTING_SCREENSHOT_DIR+"/routing-mobile.png",fullPage:true});
 expect(await page.evaluate(()=>document.documentElement.scrollWidth)).toBeLessThanOrEqual(390);
 await page.getByRole("button",{name:"Save profile",exact:true}).click();
 await expect(panel.getByRole("status")).toContainText("saved");
 expect(s.saves.at(-1).config.membership).toEqual({kind:"all_available"});
});
