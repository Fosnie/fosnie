// Copyright 2026 Private AI Ltd (SC881079)
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

import { confirmDialog, toast } from "@/components/dialogs";
import { Dropzone } from "@/components/Dropzone";
import { ACCEPT_ATTR } from "@/lib/files";
import { useRef, useState } from "react";
import { useNavigate, useParams } from "react-router-dom";
import { useQueryClient } from "@tanstack/react-query";
import {
  attachProjectLibrary,
  createGrant,
  createKnowledge,
  deleteProject,
  detachProjectLibrary,
  uploadDocument,
  uploadWorkspaceDoc,
  useChats,
  useGrants,
  useLibraries,
  useProjectDocs,
  useProjectLinks,
  useProjects,
  useReviews,
  useUsers,
  useWorkspaceDocs,
} from "@/api/client";
import { useActiveProject } from "@/app/ProjectContext";
import { Icon } from "@/components/icons";
import { Popover } from "@/components/Popover";
import { Dropdown } from "@/components/Dropdown";
import { CreateReviewModal } from "@/components/CreateReviewModal";
import { getProjectDocsPanels } from "@/ext/registry";

type ProjTab = "chats" | "documents" | "knowledge" | "reviews" | "sharing";

const PROJ_TABS: { id: ProjTab; label: string; Glyph: typeof Icon.Doc }[] = [
  { id: "chats", label: "Chats", Glyph: Icon.Chat },
  { id: "documents", label: "Documents", Glyph: Icon.Doc },
  { id: "knowledge", label: "Project Knowledge", Glyph: Icon.Book },
  { id: "reviews", label: "Tabular Reviews", Glyph: Icon.Grid },
  { id: "sharing", label: "Sharing", Glyph: Icon.Team },
];

export function ProjectWorkspace() {
  const { projectId } = useParams();
  const nav = useNavigate();
  const qc = useQueryClient();
  const projects = useProjects();
  const docs = useProjectDocs(projectId);
  const wsDocs = useWorkspaceDocs(projectId);
  const reviews = useReviews(projectId);
  const grants = useGrants("project", projectId);
  const { setActive } = useActiveProject();
  const kbFileInput = useRef<HTMLInputElement | null>(null);
  const wsFileInput = useRef<HTMLInputElement | null>(null);
  const [busy, setBusy] = useState(false);
  const [wsBusy, setWsBusy] = useState(false);
  const [creating, setCreating] = useState(false);
  const [tab, setTab] = useState<ProjTab>("documents");

  const project = projects.data?.find((p) => p.id === projectId);
  const legal = project?.sector === "legal";

  async function makeKb() {
    if (!projectId) return;
    setBusy(true);
    try { await createKnowledge(projectId); await qc.invalidateQueries({ queryKey: ["project-docs", projectId] }); }
    catch (e) { toast(`Create knowledge base failed: ${(e as Error).message}`); } finally { setBusy(false); }
  }
  async function onKbFiles(files: FileList | File[] | null) {
    if (!projectId || !files?.length) return;
    setBusy(true);
    try { for (const f of Array.from(files)) await uploadDocument(projectId, f); await qc.invalidateQueries({ queryKey: ["project-docs", projectId] }); }
    catch (e) { toast(`Upload failed: ${(e as Error).message}`); } finally { setBusy(false); if (kbFileInput.current) kbFileInput.current.value = ""; }
  }
  async function onWsFiles(files: FileList | File[] | null) {
    if (!projectId || !files?.length) return;
    setWsBusy(true);
    try { for (const f of Array.from(files)) await uploadWorkspaceDoc(projectId, f); await qc.invalidateQueries({ queryKey: ["workspace-docs", projectId] }); }
    catch (e) { toast(`Upload failed: ${(e as Error).message}`); } finally { setWsBusy(false); if (wsFileInput.current) wsFileInput.current.value = ""; }
  }
  function startChat() { if (project) setActive(project); nav("/"); }
  async function archiveProject() {
    if (!projectId) return;
    if (!(await confirmDialog({ title: `Archive "${project?.name ?? "this project"}"?`, body: "It's hidden from lists but recoverable.", danger: true, confirmLabel: "Archive" }))) return;
    try {
      await deleteProject(projectId);
      await qc.invalidateQueries({ queryKey: ["projects"] });
      setActive(null);
      nav("/");
    } catch (e) { toast(`Archive failed: ${(e as Error).message}`); }
  }

  const peopleCount = (grants.data?.length ?? 0) + 1;

  return (
    <div className="proj-ws main-scroll">
      <input ref={kbFileInput} type="file" accept={ACCEPT_ATTR} multiple hidden onChange={(e) => onKbFiles(e.target.files)} />
      <input ref={wsFileInput} type="file" accept={ACCEPT_ATTR} multiple hidden onChange={(e) => onWsFiles(e.target.files)} />

      <div className="proj-hero">
        <div className="proj-hero-top">
          <div className="proj-folder"><Icon.Folder size={20} /></div>
          <span className={"mode-chip" + (legal ? " legal" : "")}>{legal ? "Legal" : "General"} workmode</span>
        </div>
        <h1 className="serif proj-name-h">{project?.name ?? "Project"}</h1>
        {project?.description && <p className="proj-desc">{project.description}</p>}
        <div className="proj-hero-actions">
          <button className="btn btn-gold" onClick={startChat}><Icon.Chat size={15} /> New chat in project</button>
          <button className="btn btn-ghost" onClick={archiveProject} title="Archive project (recoverable)"><Icon.Trash size={15} /> Delete project</button>
        </div>
        <div className="proj-stats">
          <Stat value={wsDocs.data?.length ?? 0} label="Documents" />
          <Stat value={docs.data?.documents.length ?? 0} label="Knowledge sources" />
          {legal && <Stat value={reviews.data?.length ?? 0} label="Tabular reviews" />}
          <Stat value={peopleCount} label="People" />
        </div>
      </div>

      <div className="proj-tabs">
        {PROJ_TABS.filter((t) => t.id !== "reviews" || legal).map(({ id, label, Glyph }) => (
          <button key={id} className={"proj-tab" + (tab === id ? " on" : "")} onClick={() => setTab(id)}><Glyph size={15} /> {label}</button>
        ))}
      </div>

      <div className="proj-panel">
        {tab === "chats" && projectId && <ChatsTab projectId={projectId} />}

        {tab === "documents" && (
          <Dropzone onFiles={onWsFiles}>
            <div className="proj-panel-head">
              <span className="side-label mono">Workspace documents</span>
              <span className="row" style={{ gap: 8, alignItems: "center" }}>
                {projectId && getProjectDocsPanels().map((p) => p.toolbar && <p.toolbar key={p.key} projectId={projectId} />)}
                <button className="btn btn-gold sm" disabled={wsBusy} onClick={() => wsFileInput.current?.click()}><Icon.Plus size={14} /> {wsBusy ? "Uploading…" : "Upload"}</button>
              </span>
            </div>
            <p className="ed-hint mono" style={{ marginTop: 0, marginBottom: 12 }}>Editable working documents — what tabular reviews run over (distinct from Project Knowledge).</p>
            {wsDocs.isLoading ? <p className="text-sm text-slate">Loading…</p> : !wsDocs.data?.length ? <p className="text-sm text-slate/70">No documents yet.</p> : (
              <div className="docs-list flush">
                {wsDocs.data.map((d) => (
                  <div key={d.id} className="docs-row" style={{ cursor: "pointer" }} onClick={() => nav(`/p/${projectId}/d/${d.id}`)}>
                    <span className="docs-ic"><Icon.Doc size={17} /></span>
                    <div className="docs-main">
                      <span className="docs-name">{d.original_filename}</span>
                      <span className="docs-meta mono">{(d.mime ?? "document").split("/").pop()} · review redlines</span>
                    </div>
                    {projectId && getProjectDocsPanels().map((p) => p.rowBadge && <p.rowBadge key={p.key} projectId={projectId} doc={d} />)}
                    <span className="row" style={{ gap: 6, alignItems: "center" }} onClick={(e) => e.stopPropagation()}>
                      {projectId && getProjectDocsPanels().map((p) => p.rowAction && <p.rowAction key={p.key} projectId={projectId} doc={d} />)}
                    </span>
                    <button className="icon-btn"><Icon.ChevronR size={16} /></button>
                  </div>
                ))}
              </div>
            )}
          </Dropzone>
        )}

        {tab === "knowledge" && (
          <Dropzone onFiles={onKbFiles} disabled={!docs.data?.knowledge}>
            <div className="proj-panel-head">
              <span className="side-label mono">Indexed knowledge</span>
              {docs.data?.knowledge && <button className="btn btn-gold sm" disabled={busy} onClick={() => kbFileInput.current?.click()}><Icon.Plus size={14} /> {busy ? "Uploading…" : "Add knowledge"}</button>}
            </div>
            <p className="ed-hint mono" style={{ marginTop: 0, marginBottom: 12 }}>Indexed RAG sources the AI cites — distinct from the editable Documents tab.</p>
            {docs.isLoading && <p className="text-sm text-slate">Loading…</p>}
            {!docs.isLoading && docs.data && !docs.data.knowledge && (
              <div>
                <p className="ed-hint mono" style={{ marginTop: 0, marginBottom: 18 }}>No knowledge base yet. Create one to upload documents the AI can cite.</p>
                <button className="btn btn-gold" disabled={busy} onClick={makeKb}><Icon.Plus size={15} /> {busy ? "Creating…" : "Create knowledge base"}</button>
              </div>
            )}
            {docs.data?.knowledge && (
              docs.data.documents.length === 0 ? <p className="text-sm text-slate/70">No documents yet.</p> : (
                <div className="docs-list flush">
                  {docs.data.documents.map((d) => (
                    <div key={d.id} className="docs-row">
                      <span className="docs-ic"><Icon.Book size={17} /></span>
                      <div className="docs-main">
                        <span className="docs-name">{d.filename}</span>
                        <span className="docs-meta mono">vector index</span>
                      </div>
                      <span className={"index-badge " + (d.status === "ready" ? "ready" : "indexing")}>
                        {d.status === "ready" ? <Icon.Check2 size={13} /> : <span className="cs-spin" />} {d.status === "ready" ? "ready" : d.status}
                      </span>
                    </div>
                  ))}
                </div>
              )
            )}

            {projectId && <AttachedLibraries projectId={projectId} />}
          </Dropzone>
        )}

        {tab === "reviews" && (
          <>
            <div className="proj-panel-head">
              <span className="side-label mono">Structured reviews</span>
              <button className="btn btn-gold sm" title="Reviews run over Workspace documents (upload them in the Documents tab)" onClick={() => setCreating(true)}><Icon.Plus size={14} /> New review</button>
            </div>
            {reviews.isLoading ? <p className="text-sm text-slate">Loading…</p> : !reviews.data?.length ? <p className="text-sm text-slate/70">No reviews yet. Extract a column of answers across your documents.</p> : (
              <div className="review-grid">
                {reviews.data.map((r) => (
                  <button key={r.id} className="review-card" onClick={() => nav(`/p/${projectId}/t/${r.id}`)}>
                    <div className="review-card-top">
                      <span className="docs-ic"><Icon.Grid size={16} /></span>
                      <span className={"badge " + (r.status === "complete" || r.status === "done" ? "complete" : r.status === "running" ? "running" : "draft")}>{r.status}</span>
                    </div>
                    <h3 className="serif review-name">{r.name}</h3>
                    <div className="review-meta mono">Open review →</div>
                  </button>
                ))}
              </div>
            )}
          </>
        )}

        {tab === "sharing" && projectId && <SharingTab projectId={projectId} />}
      </div>

      {creating && projectId && (
        <CreateReviewModal
          projectId={projectId}
          onClose={() => setCreating(false)}
          onCreated={(id) => { setCreating(false); qc.invalidateQueries({ queryKey: ["reviews", projectId] }); nav(`/p/${projectId}/t/${id}`); }}
        />
      )}
    </div>
  );
}

function Stat({ value, label }: { value: number; label: string }) {
  return (
    <div className="proj-stat">
      <span className="serif proj-stat-v">{value}</span>
      <span className="proj-stat-l">{label}</span>
    </div>
  );
}

// All chats belonging to this project (newest first) — open one to continue it.
function ChatsTab({ projectId }: { projectId: string }) {
  const nav = useNavigate();
  const chats = useChats();
  const list = (chats.data ?? [])
    .filter((c) => c.project_id === projectId)
    .slice()
    .sort((a, b) => b.created_at.localeCompare(a.created_at));
  return (
    <>
      <div className="proj-panel-head">
        <span className="side-label mono">Project chats</span>
        <span className="ed-hint mono">{list.length}</span>
      </div>
      {chats.isLoading ? (
        <p className="text-sm text-slate">Loading…</p>
      ) : list.length === 0 ? (
        <p className="text-sm text-slate/70">No chats in this project yet. Start one from “New chat in project”.</p>
      ) : (
        <div className="docs-list flush">
          {list.map((c) => (
            <button
              key={c.id}
              className="docs-row"
              style={{ width: "100%", textAlign: "left", background: "none", border: 0, cursor: "pointer" }}
              onClick={() => nav(`/c/${c.id}`)}
            >
              <span className="docs-ic"><Icon.Chat size={16} /></span>
              <div className="docs-main">
                <span className="docs-name">{c.title}</span>
                <span className="docs-meta mono">{new Date(c.created_at).toLocaleDateString()}</span>
              </div>
              <Icon.ChevronR size={16} />
            </button>
          ))}
        </div>
      )}
    </>
  );
}

// Attach / detach reusable Libraries to this Project. Attaching grounds answers
// in the Library, but retrieval still honours each member's own access (the
// intersection rule) — a teammate without a grant simply sees fewer sources.
function AttachedLibraries({ projectId }: { projectId: string }) {
  const qc = useQueryClient();
  const links = useProjectLinks(projectId);
  const libs = useLibraries();
  const [menuOpen, setMenuOpen] = useState(false);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [busy, setBusy] = useState(false);
  const pickBtnRef = useRef<HTMLButtonElement | null>(null);

  const attachedIds = new Set((links.data ?? []).map((l) => l.id));
  // Attachable = readable Libraries not already linked and not restricted to
  // another origin (the backend enforces this too).
  const attachable = (libs.data ?? []).filter((k) => !attachedIds.has(k.id) && !k.restricted);
  const extras = (links.data ?? []).filter((l) => !l.is_default);

  function toggle(kbId: string) {
    setSelected((cur) => {
      const n = new Set(cur);
      if (n.has(kbId)) n.delete(kbId); else n.add(kbId);
      return n;
    });
  }

  // Attach every selected KB (one call each — no batch endpoint), then a single
  // refetch. The picker stays available afterwards so more can be added.
  async function attachSelected() {
    if (selected.size === 0 || busy) return;
    setBusy(true);
    try {
      for (const kbId of selected) await attachProjectLibrary(projectId, kbId);
      setSelected(new Set());
      setMenuOpen(false);
      await qc.invalidateQueries({ queryKey: ["project-kb-links", projectId] });
    } catch (e) {
      toast(`Attach failed: ${(e as Error).message}`);
    } finally {
      setBusy(false);
    }
  }
  async function detach(kbId: string) {
    if (!(await confirmDialog({ title: "Detach this Library?", body: "The next query won't see it.", danger: true, confirmLabel: "Detach" }))) return;
    try {
      await detachProjectLibrary(projectId, kbId);
      await qc.invalidateQueries({ queryKey: ["project-kb-links", projectId] });
    } catch (e) {
      toast(`Detach failed: ${(e as Error).message}`);
    }
  }

  return (
    <div style={{ marginTop: 22 }}>
      <div className="proj-panel-head">
        <span className="side-label mono">Attached Knowledge Bases</span>
        {attachable.length > 0 && (
          <>
            <button ref={pickBtnRef} className="btn btn-line sm" disabled={busy} onClick={() => setMenuOpen((v) => !v)}>
              <Icon.Plus size={14} /> Attach Knowledge Bases…
            </button>
            <Popover anchorRef={pickBtnRef} open={menuOpen} onClose={() => { setMenuOpen(false); setSelected(new Set()); }} placement="bottom-end" offset={8} className="menu glass glass--menu" role="menu">
              <div style={{ minWidth: 280, maxWidth: 360 }}>
                <div className="menu-label mono">Select Knowledge Bases</div>
                {attachable.map((k) => (
                  <button key={k.id} className="menu-item" onClick={() => toggle(k.id)}>
                    {selected.has(k.id) ? <Icon.Check size={14} /> : <span style={{ width: 14 }} />}
                    <span style={{ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>{k.name}</span>
                    <span className="mono" style={{ marginLeft: "auto", opacity: 0.55, fontSize: 10 }}>{k.visibility}</span>
                  </button>
                ))}
                {selected.size > 0 && (
                  <>
                    <div style={{ height: 1, background: "var(--line)", margin: "4px 0" }} />
                    <div style={{ padding: "6px 8px" }}>
                      <button className="btn btn-gold sm" style={{ width: "100%" }} disabled={busy} onClick={attachSelected}>
                        {busy ? "Attaching…" : `Attach (${selected.size})`}
                      </button>
                    </div>
                  </>
                )}
              </div>
            </Popover>
          </>
        )}
      </div>
      <p className="ed-hint mono" style={{ marginTop: 0, marginBottom: 10 }}>
        Reusable Knowledge Bases grounding this Project. Retrieval honours each member's own access.
      </p>
      {extras.length === 0 ? (
        <p className="text-sm text-slate/70">No libraries attached. Attach one to reuse shared knowledge here.</p>
      ) : (
        <div className="docs-list flush">
          {extras.map((l) => (
            <div key={l.id} className="docs-row">
              <span className="docs-ic"><Icon.Layers size={16} /></span>
              <div className="docs-main">
                <span className="docs-name">{l.name}</span>
                <span className="docs-meta mono">{l.visibility}</span>
              </div>
              <span className="role-chip mono">{l.visibility}</span>
              <button className="icon-btn" title="Detach" onClick={() => detach(l.id)}><Icon.Close size={15} /></button>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

function SharingTab({ projectId }: { projectId: string }) {
  const qc = useQueryClient();
  const grants = useGrants("project", projectId);
  const users = useUsers();
  const [pickUser, setPickUser] = useState("");
  const [perm, setPerm] = useState("read");
  const [busy, setBusy] = useState(false);

  const nameOf = (id: string) => { const u = users.data?.find((x) => x.id === id); return u ? (u.display_name || u.email) : id.slice(0, 8); };

  async function add() {
    if (!pickUser || busy) return;
    setBusy(true);
    try { await createGrant({ resource_type: "project", resource_id: projectId, principal_type: "user", principal_id: pickUser, permission: perm }); setPickUser(""); await qc.invalidateQueries({ queryKey: ["grants", "project", projectId] }); }
    catch (e) { toast(`Add failed: ${(e as Error).message} (granting access is admin-only)`); } finally { setBusy(false); }
  }

  return (
    <>
      <div className="proj-panel-head">
        <span className="side-label mono">People with access</span>
      </div>
      <div className="col-add" style={{ marginBottom: 16 }}>
        <div style={{ flex: 1 }}>
          <Dropdown
            value={pickUser}
            onChange={setPickUser}
            ariaLabel="Person to grant access"
            fullWidth
            icon={<Icon.User size={14} />}
            options={[
              { value: "", label: "Select a person…" },
              ...(users.data ?? []).map((u) => ({ value: u.id, label: u.display_name || u.email })),
            ]}
          />
        </div>
        <Dropdown
          value={perm}
          onChange={setPerm}
          ariaLabel="Permission"
          options={[
            { value: "read", label: "Can view" },
            { value: "write", label: "Can edit" },
            { value: "share", label: "Can share" },
          ]}
        />
        <button className="btn btn-gold" disabled={!pickUser || busy} onClick={add}><Icon.Plus size={15} /> {busy ? "Adding…" : "Add"}</button>
      </div>
      {grants.isLoading ? <p className="text-sm text-slate">Loading…</p> : !grants.data?.length ? <p className="text-sm text-slate/70">Only the owner has access. Add people above (admin only).</p> : (
        <div className="rows">
          {grants.data.map((g) => (
            <div key={g.id} className="list-row">
              <span className="avatar">{nameOf(g.principal_id).slice(0, 2).toUpperCase()}</span>
              <div className="row-main">
                <span className="row-title">{g.principal_type === "group" ? "Group " : ""}{nameOf(g.principal_id)}</span>
                <span className="row-sub mono">{g.principal_type}</span>
              </div>
              <span className="role-chip mono">{g.permission}</span>
            </div>
          ))}
        </div>
      )}
    </>
  );
}

// CreateReviewModal lives in components/ now (shared with the Legal shell).
