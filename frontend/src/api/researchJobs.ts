// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
import { useQuery } from "@tanstack/react-query";
import { apiFetch } from "./client";
import type { DocumentJob, DocumentSelection } from "./documentJobs";

export type ResearchSelection = DocumentSelection;
export interface ResearchJobCapabilities {
  schema_version: number; enabled: boolean; selection_modes: string[];
  sources: string[]; templates: string[]; maximum_documents: number; verification: boolean;
  web_enabled?: boolean; maximum_web_pages?: number;
  custom_templates?: boolean; maximum_template_sections?: number;
  clarification?: boolean;
  deepening?: "existing_sources_v1";
  history?: boolean; idempotent_launch?: boolean;
}
export interface ResearchJob {
  job_id: string; chat_id: string; status: DocumentJob["status"] | "awaiting_input"; message_id: string | null;
  selection: ResearchSelection; decision_id: string; reason_codes: string[];
  usage: DocumentJob["usage"]; accounting: "conservative_snapshot";
  deadline?: string;
  options?: { verify: boolean; deepen: boolean };
  verification?: { method: string; score: number; flagged: number; total: number; model: string } | null;
  clarification?: { id: string; questions: { prompt: string; options: string[] }[] } | null;
}
export interface ResearchJobRequest {
  question: string; kb_ids: string[]; template: string; refinements: string[];
  source: "files" | "web" | "hybrid";
  verify?: boolean; deepen?: boolean;
}
export function useResearchJobScope(enabled = true) {
  return useQuery({ queryKey: ["research-job-scope"], retry: false, enabled,
    queryFn: () => apiFetch<{ id: string; name: string; kind: string; doc_count: number }[]>("/api/research-jobs/scope") });
}
export const researchJobKey = (id: string) => ["research-job", id] as const;
export const researchJobActive = (job?: ResearchJob) => job?.status === "queued" || job?.status === "running" || job?.status === "awaiting_input";
export function useResearchJobCapabilities() {
  return useQuery({ queryKey: ["research-job-capabilities"],
    queryFn: () => apiFetch<ResearchJobCapabilities>("/api/routing/research-jobs"),
    retry: false, refetchInterval: 15000 });
}
export function useResearchJob(id: string) {
  return useQuery({ queryKey: researchJobKey(id), retry: false,
    queryFn: ({ signal }) => apiFetch<ResearchJob>(`/api/research-jobs/${encodeURIComponent(id)}`, { signal }),
    refetchInterval: q => q.state.status === "error" ? false : researchJobActive(q.state.data) ? 1500 : false });
}
export function launchResearchJob(request: ResearchJobRequest, selection: ResearchSelection, clarify = false, requestId?: string) {
  return apiFetch<{ job_id: string; chat_id: string; status_url: string }>("/api/research-jobs", {
    method: "POST", body: JSON.stringify({ request, selection, ...(clarify ? { clarify: true } : {}), ...(requestId ? { request_id: requestId } : {}) }),
  });
}
export interface ResearchLaunchReceipt {
  request_id: string; status: "planning" | "succeeded" | "failed" | "interrupted";
  job_id?: string; chat_id?: string; status_url?: string;
}
export function useResearchLaunchReceipt(id: string | null, enabled: boolean) {
  return useQuery({ queryKey: ["research-launch-receipt", id], enabled: !!id && enabled, retry: false,
    queryFn: ({ signal }) => apiFetch<ResearchLaunchReceipt>(`/api/research-launches/${encodeURIComponent(id!)}`, { signal }),
    refetchInterval: q => q.state.status !== "error" && q.state.data?.status === "planning" ? 2000 : false });
}
export interface ResearchHistoryItem {
  job_id: string; status: ResearchJob["status"]; created_at: string; accessible: boolean;
  chat_id?: string; title?: string; selection?: ResearchSelection; usage?: ResearchJob["usage"];
}
export function useResearchHistory(before?: string) {
  return useQuery({ queryKey: ["research-history", before ?? null], retry: false,
    queryFn: ({ signal }) => apiFetch<{ items: ResearchHistoryItem[]; next_cursor: string | null }>(
      `/api/research-jobs${before ? `?before=${encodeURIComponent(before)}` : ""}`, { signal }),
    refetchInterval: q => q.state.status !== "error" && q.state.data?.items.some(i => ["queued", "running", "awaiting_input"].includes(i.status)) ? 5000 : false });
}
export function answerResearchJob(id: string, clarification_id: string, answers: string[]) {
  return apiFetch(`/api/research-jobs/${encodeURIComponent(id)}/answers`, {
    method: "POST", body: JSON.stringify({ clarification_id, answers }),
  });
}
export function cancelResearchJob(id: string) {
  return apiFetch<{ job_id: string; status: ResearchJob["status"] }>(`/api/research-jobs/${encodeURIComponent(id)}/cancel`, { method: "POST" });
}
export function setResearchJobsEnabled(enabled: boolean) {
  return apiFetch("/api/routing/research-jobs", { method: "PUT", body: JSON.stringify({ enabled }) });
}
