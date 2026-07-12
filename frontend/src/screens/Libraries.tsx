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

// Libraries — standalone, shareable Knowledge Bases.
// List grouped by tag (Personal / Team / Shared with me),
// create, and a detail view (documents + ingest status + Manage access). A
// Library can be attached to many Projects/chats; retrieval always honours the
// caller's own access (the intersection rule).

import { confirmDialog, toast } from "@/components/dialogs";
import { Dropzone } from "@/components/Dropzone";
import { ACCEPT_ATTR } from "@/lib/files";
import { useMemo, useRef, useState } from "react";
import { useNavigate, useParams } from "react-router-dom";
import { useQueryClient } from "@tanstack/react-query";
import {
  createLibrary,
  deleteLibrary,
  deleteLibraryDocument,
  promoteLibrary,
  uploadLibraryDocument,
  useLibraries,
  useLibrary,
  useUsers,
  type KbSummary,
  type KbVisibility,
} from "@/api/client";
import { Icon } from "@/components/icons";
import { PanelHead } from "@/components/editor";
import { ShareDialog } from "@/components/ShareDialog";
import { Toggle } from "@/components/ui";
import { Dropdown } from "@/components/Dropdown";

const VIS_ICON: Record<KbVisibility, typeof Icon.Lock> = {
  personal: Icon.Lock,
  project: Icon.Folder,
  team: Icon.Team,
  shared: Icon.Globe,
};

function VisChip({ v }: { v: KbVisibility }) {
  const I = VIS_ICON[v];
  return (
    <span className="role-chip mono" title={`Visibility: ${v}`}>
      <I size={11} /> {v}
    </span>
  );
}

const TERMINAL = ["ready", "error"];

type SortKey = "newest" | "oldest" | "name" | "visibility";

function relDate(iso: string): string {
  const ms = Date.parse(iso);
  if (Number.isNaN(ms)) return "";
  const s = (Date.now() - ms) / 1000;
  const rtf = new Intl.RelativeTimeFormat(undefined, { numeric: "auto" });
  if (s < 60) return "just now";
  if (s < 3600) return rtf.format(-Math.round(s / 60), "minute");
  if (s < 86400) return rtf.format(-Math.round(s / 3600), "hour");
  if (s < 2592000) return rtf.format(-Math.round(s / 86400), "day");
  return new Date(ms).toLocaleDateString(undefined, { day: "numeric", month: "short", year: "numeric" });
}

export function Libraries() {
  const { kbId } = useParams();
  const nav = useNavigate();
  const libs = useLibraries();
  const users = useUsers();
  const [creating, setCreating] = useState(false);
  const [q, setQ] = useState("");
  const [sort, setSort] = useState<SortKey>("newest");

  const ownerName = useMemo(() => {
    const m = new Map<string, string>();
    users.data?.forEach((u) => m.set(u.id, u.display_name || u.email));
    return m;
  }, [users.data]);

  if (kbId) return <LibraryDetail key={kbId} id={kbId} onBack={() => nav("/studio/libraries")} />;

  const SECTIONS: { key: string; label: string; match: (k: KbSummary) => boolean }[] = [
    { key: "personal", label: "Personal", match: (k) => k.mine && k.visibility !== "team" },
    { key: "team", label: "Team", match: (k) => k.visibility === "team" },
    { key: "shared", label: "Shared with me", match: (k) => !k.mine && k.visibility !== "team" },
  ];
  const ql = q.trim().toLowerCase();
  const sortFns: Record<SortKey, (a: KbSummary, b: KbSummary) => number> = {
    newest: (a, b) => b.created_at.localeCompare(a.created_at),
    oldest: (a, b) => a.created_at.localeCompare(b.created_at),
    name: (a, b) => a.name.localeCompare(b.name),
    visibility: (a, b) => a.visibility.localeCompare(b.visibility),
  };
  const data = (libs.data ?? [])
    .filter((k) => !ql || k.name.toLowerCase().includes(ql) || (k.description ?? "").toLowerCase().includes(ql))
    .slice()
    .sort(sortFns[sort]);
  const groups = SECTIONS.map((s) => [s, data.filter(s.match)] as const).filter(([, l]) => l.length);

  return (
    <div className="main-scroll">
      <div className="panel anim-on fade-in">
        <PanelHead
          title="Library"
          sub="Reusable, shareable Knowledge Bases — ingest once, ground many Projects and chats."
          action={<button className="btn btn-gold" onClick={() => setCreating(true)}><Icon.Plus size={16} /> New Knowledge Base</button>}
        />
        {!libs.isLoading && (libs.data?.length ?? 0) > 0 && (
          <div className="row" style={{ gap: 10, margin: "0 0 14px", alignItems: "center" }}>
            <div className="search-box" style={{ flex: 1, maxWidth: 360 }}>
              <Icon.Search size={14} />
              <input className="search-in" placeholder="Search KBs" value={q} onChange={(e) => setQ(e.target.value)} />
            </div>
            <Dropdown
              value={sort}
              onChange={setSort}
              ariaLabel="Sort KBs"
              icon={<Icon.Filter size={13} />}
              options={[
                { value: "newest", label: "Newest" },
                { value: "oldest", label: "Oldest" },
                { value: "name", label: "Name (A–Z)" },
                { value: "visibility", label: "Visibility" },
              ]}
            />
          </div>
        )}
        {libs.isLoading && <p className="text-sm text-slate">Loading…</p>}
        {!libs.isLoading && (libs.data?.length ?? 0) === 0 && (
          <div className="empty">
            <span className="empty-mark"><Icon.Book size={22} /></span>
            <div className="empty-title serif">No Knowledge Bases yet</div>
            <div className="empty-sub">Create one to ingest documents you can reuse across Projects.</div>
          </div>
        )}
        {!libs.isLoading && (libs.data?.length ?? 0) > 0 && data.length === 0 && (
          <p className="text-sm text-slate/70">No Knowledge Bases match "{q}".</p>
        )}
        {groups.map(([section, list]) => (
          <div key={section.key} className="prompt-group">
            <div className="prompt-group-head">
              <span className="side-label mono">{section.label}</span>
              <span className="ed-hint mono">{list.length}</span>
            </div>
            <div className="card-grid">
              {list.map((k) => (
                <div key={k.id} className="agent-card" style={{ cursor: "pointer" }} onClick={() => nav(`/studio/libraries/${k.id}`)}>
                  <div className="row" style={{ justifyContent: "space-between", alignItems: "flex-start" }}>
                    <h3 className="serif">{k.name}</h3>
                    <VisChip v={k.visibility} />
                  </div>
                  <p className="agent-card-desc">{k.description || "No description."}</p>
                  <div className="ed-hint mono" style={{ fontSize: 11, marginBottom: 6 }}>
                    by {ownerName.get(k.owner_id) ?? "—"} · {relDate(k.created_at)}
                  </div>
                  <div className="agent-card-foot mono">
                    <span className={"index-badge " + (k.status === "ready" ? "ready" : "indexing")}>{k.status}</span>
                    {k.restricted && <span className="role-chip mono"><Icon.Lock size={11} /> restricted</span>}
                    {!k.can_manage && <span className="role-chip mono" title="You have read-only access"><Icon.Lock size={11} /> read-only</span>}
                  </div>
                </div>
              ))}
            </div>
          </div>
        ))}
      </div>
      {creating && (
        <CreateLibrary
          onClose={() => setCreating(false)}
          onCreated={(id) => { setCreating(false); nav(`/studio/libraries/${id}`); }}
        />
      )}
    </div>
  );
}

function LibraryDetail({ id, onBack }: { id: string; onBack: () => void }) {
  const qc = useQueryClient();
  const lib = useLibrary(id);
  const fileRef = useRef<HTMLInputElement>(null);
  const [sharing, setSharing] = useState(false);
  const [busy, setBusy] = useState(false);

  if (lib.isLoading || !lib.data) return <div className="main-scroll"><div className="panel">Loading…</div></div>;
  const k = lib.data;

  async function onFiles(files: FileList | File[] | null) {
    if (!files?.length) return;
    setBusy(true);
    try {
      for (const f of Array.from(files)) await uploadLibraryDocument(id, f);
      await qc.invalidateQueries({ queryKey: ["kb", id] });
    } catch (e) {
      toast(`Upload failed: ${(e as Error).message}`);
    } finally {
      setBusy(false);
      if (fileRef.current) fileRef.current.value = "";
    }
  }

  async function removeDoc(docId: string) {
    if (!(await confirmDialog({ title: "Remove this document?", body: "Its chunks are purged.", danger: true, confirmLabel: "Remove" }))) return;
    try {
      await deleteLibraryDocument(id, docId);
      await qc.invalidateQueries({ queryKey: ["kb", id] });
    } catch (e) {
      toast(`Remove failed: ${(e as Error).message}`);
    }
  }

  async function promote() {
    if (!(await confirmDialog({ title: "Promote to a shared Library?", body: "It becomes attachable to other Projects. You can then grant specific people or teams.", confirmLabel: "Promote" }))) return;
    try {
      await promoteLibrary(id, { visibility: "shared" });
      await qc.invalidateQueries({ queryKey: ["kb", id] });
      await qc.invalidateQueries({ queryKey: ["kb"] });
    } catch (e) {
      toast(`Promote failed: ${(e as Error).message}`);
    }
  }

  async function archive() {
    if (!(await confirmDialog({ title: `Archive "${k.name}"?`, body: "It drops from every list (data retained).", danger: true, confirmLabel: "Archive" }))) return;
    try {
      await deleteLibrary(id);
      await qc.invalidateQueries({ queryKey: ["kb"] });
      onBack();
    } catch (e) {
      toast(`Archive failed: ${(e as Error).message}`);
    }
  }

  return (
    <div className="main-scroll">
      <Dropzone className="panel anim-on fade-in" onFiles={onFiles} disabled={!k.can_manage}>
        <button className="btn btn-line sm" onClick={onBack} style={{ marginBottom: 14 }}><Icon.ChevronL size={14} /> Libraries</button>
        <div className="proj-panel-head" style={{ alignItems: "flex-start" }}>
          <div>
            <div className="row" style={{ gap: 10, alignItems: "center" }}>
              <h2 className="serif" style={{ margin: 0 }}>{k.name}</h2>
              <VisChip v={k.visibility} />
              {k.restricted && <span className="role-chip mono"><Icon.Lock size={11} /> restricted</span>}
            </div>
            {k.description && <p className="text-sm text-slate/80" style={{ marginTop: 6 }}>{k.description}</p>}
          </div>
          {k.can_manage && (
            <div className="row" style={{ gap: 8 }}>
              {k.visibility === "project" && <button className="btn btn-line sm" onClick={promote}><Icon.Globe size={14} /> Promote</button>}
              <button className="btn btn-line sm" onClick={() => setSharing(true)}><Icon.Team size={14} /> Manage access</button>
            </div>
          )}
        </div>

        <div className="proj-panel-head" style={{ margin: "20px 0 8px" }}>
          <span className="form-label" style={{ margin: 0 }}>Documents</span>
          {k.can_manage && (
            <button className="btn btn-gold sm" onClick={() => fileRef.current?.click()} disabled={busy}>
              <Icon.Plus size={14} /> {busy ? "Uploading…" : "Add documents"}
            </button>
          )}
        </div>
        <input ref={fileRef} type="file" accept={ACCEPT_ATTR} multiple hidden onChange={(e) => onFiles(e.target.files)} />

        {k.documents.length === 0 ? (
          <p className="ed-hint mono">No documents yet. Upload to index them for retrieval.</p>
        ) : (
          <div className="docs-list flush">
            {k.documents.map((d) => (
              <div key={d.id} className="docs-row">
                <span className="docs-ic"><Icon.Book size={15} /></span>
                <div className="docs-main">
                  <span className="docs-name">{d.filename}</span>
                  <span className="docs-meta mono">{new Date(d.created_at).toLocaleDateString()}</span>
                </div>
                {d.source === "connector_import" && (
                  <span className="index-badge" title="Imported from a connected source">Imported</span>
                )}
                <span className={"index-badge " + (TERMINAL.includes(d.status) ? (d.status === "ready" ? "ready" : "error") : "indexing")}>
                  {!TERMINAL.includes(d.status) && <span className="cs-spin" />} {d.status}
                </span>
                {k.can_manage && (
                  <button className="icon-btn" title="Remove" onClick={() => removeDoc(d.id)}><Icon.Trash size={14} /></button>
                )}
              </div>
            ))}
          </div>
        )}

        {k.can_manage && (
          <div style={{ marginTop: 24 }}>
            <button className="btn btn-line sm" onClick={archive}><Icon.Trash size={13} /> Archive library</button>
          </div>
        )}
      </Dropzone>

      {sharing && <ShareDialog kbId={id} kbName={k.name} onClose={() => setSharing(false)} />}
    </div>
  );
}

function CreateLibrary({ onClose, onCreated }: { onClose: () => void; onCreated: (id: string) => void }) {
  const qc = useQueryClient();
  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const [visibility, setVisibility] = useState<KbVisibility>("personal");
  const [parentChild, setParentChild] = useState(false);
  const [files, setFiles] = useState<File[]>([]);
  const [busy, setBusy] = useState(false);

  async function submit() {
    if (!name.trim() || busy) return;
    setBusy(true);
    try {
      const { id } = await createLibrary({ name: name.trim(), description: description.trim() || undefined, visibility, parent_child: parentChild });
      for (const f of files) await uploadLibraryDocument(id, f);
      await qc.invalidateQueries({ queryKey: ["kb"] });
      onCreated(id);
    } catch (e) {
      toast(`Create failed: ${(e as Error).message}`);
      setBusy(false);
    }
  }

  return (
    <div className="modal-scrim" onClick={onClose}>
      <div className="modal" style={{ width: 520, maxWidth: "100%" }} onClick={(e) => e.stopPropagation()}>
        <div className="modal-head">
          <div><div className="eyebrow">Knowledge Base</div><h2 className="serif modal-title">New Knowledge Base</h2></div>
          <button className="icon-btn" onClick={onClose}><Icon.Close size={18} /></button>
        </div>
        <div className="modal-body">
          <label className="form-label">Name</label>
          <input className="field" autoFocus value={name} onChange={(e) => setName(e.target.value)} placeholder="e.g. House style & precedents" />
          <label className="form-label" style={{ marginTop: 12 }}>Description <span className="opt">optional</span></label>
          <input className="field" value={description} onChange={(e) => setDescription(e.target.value)} placeholder="What this library holds" />
          <label className="form-label" style={{ marginTop: 12 }}>Visibility</label>
          <Dropdown
            value={visibility}
            onChange={setVisibility}
            ariaLabel="Visibility"
            fullWidth
            options={[
              { value: "personal", label: "Personal — only me (until shared)" },
              { value: "team", label: "Team — a group I share with" },
              { value: "shared", label: "Shared — specific people/teams" },
            ]}
          />
          <div className="row" style={{ marginTop: 12, gap: 10, alignItems: "flex-start" }}>
            <Toggle on={parentChild} onChange={setParentChild} label="Parent–child chunking" />
            <div>
              <div className="form-label" style={{ margin: 0 }}>Parent–child chunking</div>
              <div className="ed-hint" style={{ marginTop: 2 }}>Recommended for statutes &amp; contracts — retrieves the enclosing section, so provisos and exceptions aren't lost.</div>
            </div>
          </div>
          <label className="form-label" style={{ marginTop: 12 }}>First documents <span className="opt">optional</span></label>
          <input className="field" type="file" accept={ACCEPT_ATTR} multiple onChange={(e) => setFiles(Array.from(e.target.files ?? []))} />
          {files.length > 0 && <div className="ed-hint mono" style={{ marginTop: 6 }}>{files.length} file{files.length > 1 ? "s" : ""} selected</div>}
        </div>
        <div className="modal-foot">
          <button className="btn btn-line sm" onClick={onClose}>Cancel</button>
          <button className="btn btn-gold sm" onClick={submit} disabled={!name.trim() || busy}>{busy ? "Creating…" : "Create Knowledge Base"}</button>
        </div>
      </div>
    </div>
  );
}
