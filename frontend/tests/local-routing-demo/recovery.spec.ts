import { test, expect } from '@playwright/test';
import { readFileSync, writeFileSync, existsSync } from 'node:fs';
import { join } from 'node:path';

test('Native quota failure switches once to real local Llama and survives reload', async ({ page, request }) => {
  const infoResponse = await request.get('/demo/info');
  expect(infoResponse.ok()).toBeTruthy();
  const info = await infoResponse.json();
  expect(info.error_handling_stage).toBe('V45');
  expect(info.technical_fallback).toBe(true);
  expect(info.cloud_models_connected).toBe(false);
  const state = JSON.parse(readFileSync(process.env.FOSNIE_DEMO_STATE!, 'utf8'));
  expect(state.state).toBe('ready');
  expect(state.source_stage).toBe('V46');
  const marker = join(state.run_dir, 'quota-once');
  expect(existsSync(marker)).toBe(false);
  const sent: string[] = [];
  page.on('websocket', socket => socket.on('framesent', event => {
    try { const frame = JSON.parse(String(event.payload)); if (frame.type === 'chat.routed.send') sent.push(frame.request.content); } catch {}
  }));
  await page.goto('/tests/local-routing-demo/index.html');
  await expect(page.getByLabel('Chat model', { exact: true })).toBeEnabled();
  await page.getByLabel('Chat model', { exact: true }).selectOption('auto');
  await page.getByLabel('Routing objective').selectOption('economy');
  const rules = info.policies.find((p: { strategy: string }) => p.strategy === 'rules');
  await page.getByLabel('Chat routing policy').selectOption(rules.id);
  await page.getByLabel('Local inference only').check();
  writeFileSync(marker, 'quota_once\n', { flag: 'wx' });
  const prompt = 'What is 7 times 8? Answer with only the number.';
  await page.locator('textarea.comp-in').fill(prompt);
  await page.locator('textarea.comp-in').press('Enter');
  await expect(page.getByLabel('Actual model')).toContainText('Alternate model answered', { timeout: 100000 });
  await expect(page.getByLabel('Actual model')).toContainText('Llama-3.2-3B');
  expect(sent).toEqual([prompt]);
  expect(existsSync(marker)).toBe(false);
  const badge = await page.getByLabel('Actual model').textContent();
  const chatId = new URL(page.url()).hash.match(/\/c\/([^/?]+)/)?.[1];
  expect(chatId).toBeTruthy();
  const messagesResponse = await request.get(`/api/chats/${chatId}/messages`);
  expect(messagesResponse.ok()).toBeTruthy();
  const messages = await messagesResponse.json();
  expect(messages.filter((m: { role: string }) => m.role === 'user')).toHaveLength(1);
  const answer = messages.find((m: { role: string }) => m.role === 'assistant');
  expect(answer.content.trim().length).toBeGreaterThan(0);
  expect(answer.routing.status).toBe('completed');
  expect(answer.routing.actual.model).toContain('Llama-3.2-3B');
  expect(answer.routing.reason_codes).toContain('technical_fallback');
  expect(answer.routing.attempts).toHaveLength(1);
  expect(answer.routing.attempts[0].failure).toBe('quota_exhausted');
  const events = readFileSync(join(state.run_dir, 'provider-events.jsonl'), 'utf8').trim().split('\n').map(line => JSON.parse(line));
  expect(events.filter(e => e.result === 'injected_native_quota')).toHaveLength(1);
  expect(events.some(e => e.result === 'forwarded_to_owned_ollama' && e.model.includes('Llama-3.2-3B'))).toBe(true);
  await page.reload();
  await expect(page.getByLabel('Actual model')).toHaveText(badge!);
  await expect(page.getByLabel('Chat model', { exact: true })).toHaveValue('auto');
  expect(sent).toEqual([prompt]);
  writeFileSync(process.env.FOSNIE_DEMO_RECEIPT!, JSON.stringify({ schema_version: 1, stage: 'V46',
    source: 'real Core/ML/browser and local Llama; injected native quota on Qwen',
    chat_id: chatId, user_turns: 1, native_quota_failures: 1, alternate_attempts: 1,
    actual_badge: badge, reload_preserved: true, external_model_api_calls: 0,
    quality_evidence: false, p12_eligible: false }, null, 2), { flag: 'wx' });
  await page.screenshot({ path: process.env.FOSNIE_DEMO_SCREENSHOT!, fullPage: true });
});
