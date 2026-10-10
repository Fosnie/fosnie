import { test, expect } from '@playwright/test';
import { readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';

const out = process.env.FOSNIE_MIXED_OUTPUT!;
const state = () => JSON.parse(readFileSync(process.env.FOSNIE_DEMO_STATE!, 'utf8'));
async function status(request: any) { return (await request.get(state().provider_url + '/pilot/status')).json(); }
async function open(page: any, request: any) {
  const info = await (await request.get('/demo/info')).json();
  expect(info.cloud_models_connected).toBe(true);
  await page.goto('/tests/local-routing-demo/index.html');
  await expect(page.getByLabel('Chat model', { exact: true })).toBeEnabled();
  return info;
}
async function answer(page: any, request: any, prompt: string) {
  await page.locator('textarea.comp-in').fill(prompt);
  await page.locator('textarea.comp-in').press('Enter');
  await expect(page.getByLabel('Actual model')).toBeVisible({timeout:150000});
  const chat = new URL(page.url()).hash.match(/\/c\/([^/?]+)/)?.[1];
  expect(chat).toBeTruthy();
  let rows: any[] = [];
  await expect.poll(async () => {
    rows = await (await request.get(`/api/chats/${chat}/messages`)).json();
    return rows.find(m => m.role === 'assistant')?.routing?.status;
  }, { timeout: 150000 }).toBe('completed');
  expect(rows.filter(m => m.role === 'user')).toHaveLength(1);
  const result = rows.find(m => m.role === 'assistant');
  expect(result.content.trim().length).toBeGreaterThan(0);
  return { chat, result };
}
function receipt(name: string, value: any) {
  writeFileSync(join(out, name + '.json'), JSON.stringify({ ...value, quality_evidence: false, p12_eligible: false }, null, 2), {flag:'wx'});
}

test('Local Auto performs actual quick web search without cloud inference', async ({page,request}) => {
  const tools: any[] = [];
  page.on('websocket',(socket: any) => socket.on('framereceived',(event: any) => {
    try { const frame=JSON.parse(String(event.payload)); if(frame.type==='chat.tool') tools.push(frame); } catch {}
  }));
  const info = await open(page,request);
  const before = await status(request);
  await page.locator('.agent-trigger').click();
  await page.locator('.agent-row').filter({hasText:'Quick web demo'}).click();
  await page.getByLabel('Chat model',{exact:true}).selectOption('auto');
  await page.getByLabel('Chat routing policy').selectOption(info.policies.find((p:any)=>p.strategy==='rules').id);
  await page.getByLabel('Local inference only').check();
  const {chat,result}=await answer(page,request,'Use web_search once with depth quick to find a current Glasgow weather forecast. Give a brief sourced answer. If current weather is not established by dated snippets, say so.');
  expect(result.routing.actual.model).toContain('qwen3.5');
  expect(tools.some(t=>t.name==='web_search')).toBe(true);
  expect((await status(request)).cloud_calls).toEqual(before.cloud_calls);
  await page.reload();
  await expect(page.getByLabel('Actual model')).toContainText('qwen3.5');
  receipt('local-web',{chat,actual:result.routing.actual,web_tool_observed:true,cloud_calls_added:0,reason_codes:result.routing.reason_codes});
  await page.screenshot({path:join(out,'local-web.png'),fullPage:true});
});

test('Cloud-only Auto uses a cloud judge and cloud executor', async ({page,request}) => {
  const info=await open(page,request);
  const before=await status(request);
  await page.getByLabel('Chat model',{exact:true}).selectOption('auto');
  await page.getByLabel('Routing objective').selectOption('quality');
  await page.getByLabel('Chat routing policy').selectOption(info.policies.find((p:any)=>p.label==='Cloud-only Classifier').id);
  await page.getByLabel('Local inference only').uncheck();
  const {chat,result}=await answer(page,request,'Prove that the square root of 2 is irrational. Give a concise proof.');
  expect(result.routing.actual.model).toMatch(/gpt-5.4-mini|gemini-3.1-flash-lite/);
  expect(result.routing.reason_codes).not.toContain('classifier_fallback');
  const after=await status(request);
  expect(after.cloud_calls.gemini).toBeGreaterThan(before.cloud_calls.gemini);
  receipt('cloud-only',{chat,actual:result.routing.actual,reason_codes:result.routing.reason_codes,cloud_before:before.cloud_calls,cloud_after:after.cloud_calls});
});

test('Mixed Auto classifies a complex task within its permitted pool', async ({page,request}) => {
  const info=await open(page,request);
  await page.getByLabel('Chat model',{exact:true}).selectOption('auto');
  await page.getByLabel('Routing objective').selectOption('quality');
  await page.getByLabel('Chat routing policy').selectOption(info.policies.find((p:any)=>p.label==='Mixed Hybrid').id);
  await page.getByLabel('Local inference only').uncheck();
  const before=await status(request);
  const {chat,result}=await answer(page,request,'Write a concise Python binary search and explain why its loop invariant proves correctness.');
  expect(result.routing.actual.model).toMatch(/qwen3.5|Llama-3.2-3B|gpt-5.4-mini|gemini-3.1-flash-lite/);
  expect(result.routing.reason_codes).not.toContain('classifier_fallback');
  const after=await status(request);
  expect(after.cloud_calls.gemini).toBeGreaterThan(before.cloud_calls.gemini);
  receipt('mixed-auto',{chat,actual:result.routing.actual,reason_codes:result.routing.reason_codes,cloud_before:before.cloud_calls,cloud_after:after.cloud_calls});
  await page.screenshot({path:join(out,'mixed-auto.png'),fullPage:true});
});

test('Manual cloud model preserves explicitly selected high effort', async ({page,request}) => {
  const info=await open(page,request);
  await page.getByLabel('Chat model',{exact:true}).selectOption('auto');
  await page.getByLabel('Chat routing policy').selectOption(info.policies.find((p:any)=>p.label==='Mixed Hybrid').id);
  await page.getByLabel('Chat model',{exact:true}).selectOption(info.cloud_providers.openai);
  await page.getByLabel('Local inference only').uncheck();
  await page.getByLabel('Routing effort').selectOption('high');
  const {chat,result}=await answer(page,request,'Is 17 prime? Answer briefly and give one reason.');
  expect(result.routing.actual).toEqual({model:'gpt-5.4-mini',effort:'high'});
  expect(result.routing.selection_mode).toBe('manual');
  receipt('manual-high',{chat,actual:result.routing.actual});
});

test('Mixed Auto selects local Qwen for a fresh fact with permitted web search', async ({page,request}) => {
  const info=await open(page,request);
  await page.locator('.agent-trigger').click();
  await page.locator('.agent-row').filter({hasText:'Quick web demo'}).click();
  await page.getByLabel('Chat model',{exact:true}).selectOption('auto');
  await page.getByLabel('Routing objective').selectOption('economy');
  await page.getByLabel('Chat routing policy').selectOption(info.policies.find((p:any)=>p.label==='Mixed Hybrid').id);
  await page.getByLabel('Local inference only').uncheck();
  const before=await status(request);
  const {chat,result}=await answer(page,request,'What is the latest Glasgow weather forecast? Use web_search once with depth quick, cite dated snippets, and do not invent current observations.');
  expect(result.routing.actual.model).toContain('qwen3.5');
  expect(result.routing.reason_codes).toContain('classified_requirements');
  const after=await status(request);
  expect(after.cloud_calls.gemini).toBeGreaterThan(before.cloud_calls.gemini);
  expect(after.cloud_calls.openai).toBe(before.cloud_calls.openai);
  receipt('mixed-weather',{chat,actual:result.routing.actual,reason_codes:result.routing.reason_codes,cloud_before:before.cloud_calls,cloud_after:after.cloud_calls});
  await page.screenshot({path:join(out,'mixed-weather.png'),fullPage:true});
});
