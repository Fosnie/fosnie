// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
import { useQuery } from "@tanstack/react-query";
import { apiFetch } from "./client";

export type DocumentSelection = { mode: "default" } | { mode: "manual"; provider_id: string }
  | { mode: "auto"; objective: "balanced" };
export interface DocumentJobCapabilities {
  schema_version: number; enabled: boolean; delayed_repair: boolean;
  selection_modes: string[]; target_types: string[];
}
export interface DocumentJob {
  id: string; document_id: string | null; target_id?: string; target_type?: "document" | "draft";
  status: "queued" | "running" | "succeeded" | "failed" | "cancelled" | "interrupted";
  phase: "verify" | "repair"; repair: boolean; selection: DocumentSelection;
  decision_id: string; reason_codes: string[];
  planned_document_target: { model: string; effort: string | null } | null;
  usage: { calls: number; tokens: number; cost_microusd: number; unknown_cost_calls: number; unknown_token_calls: number; exhausted: boolean };
  accounting: "conservative_snapshot"; result_id: string | null; updated_at: string;
}
export interface RoutingModel {
  scope?: string;
  id: string; label: string | null; model: string | null; enabled: boolean;
  provider_revision: number; profile_revision: number | null;
  profile: { provider_id: string; provider_revision: number; revision: number; model: string } | null;
}
export const jobActive = (job?: DocumentJob) => job?.status === "queued" || job?.status === "running";
export const currentModel = (entry: RoutingModel) => !!entry.enabled && !!entry.profile
  && entry.profile.provider_id === entry.id && entry.profile.provider_revision === entry.provider_revision
  && entry.profile.revision === entry.profile_revision && entry.profile.model === entry.model;
export const jobKey = (id: string | null) => ["document-job", id] as const;
export function useDocumentJobCapabilities(enabled = true) {
  return useQuery({ queryKey: ["document-job-capabilities"], enabled,
    queryFn: () => apiFetch<DocumentJobCapabilities>("/api/routing/document-jobs"), retry: false,
    refetchInterval: 15000 });
}
export function useRoutingModels(enabled: boolean) {
  return useQuery({ queryKey: ["routing-models"], enabled,
    queryFn: () => apiFetch<RoutingModel[]>("/api/routing/catalog"), retry: false });
}
export function useDocumentJob(id: string | null) {
  return useQuery({ queryKey: jobKey(id), enabled: !!id, retry: false,
    queryFn: ({ signal }) => apiFetch<DocumentJob>(`/api/document-verification-jobs/${encodeURIComponent(id!)}`, { signal }),
    refetchInterval: q => q.state.status === "error" ? false : jobActive(q.state.data) ? 1500 : false });
}
export function launchDocumentJob(documentId: string, selection: DocumentSelection, repair: boolean, targetType: "document" | "draft" = "document") {
  if (targetType === "draft") return apiFetch<{ job_id: string }>("/api/artefact-verification-jobs", {
    method: "POST", body: JSON.stringify({ artefact_id: documentId, selection }),
  });
  return apiFetch<{ job_id: string }>("/api/document-verification-jobs", {
    method: "POST", body: JSON.stringify({ document_id: documentId, selection, repair }),
  });
}
export function changeDocumentJob(id: string, action: "cancel" | "repair") {
  return apiFetch(`/api/document-verification-jobs/${encodeURIComponent(id)}/${action}`, { method: "POST" });
}
export function setDocumentJobsEnabled(enabled: boolean) {
  return apiFetch("/api/routing/document-jobs", { method: "PUT", body: JSON.stringify({ enabled }) });
}
