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

// Per-Library RBAC management (Google-Drive-style). Search a user or group, pick
// read/manage, add; list current grantees with their permission + a remove
// control. Widening the audience triggers a confirmation because it is a
// **disclosure event** (Libraries §9). Rendered only for manage-holders.

import { confirmDialog, toast } from "@/components/dialogs";
import { useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import {
  deleteKbGrant,
  putKbGrant,
  useGroups,
  useKbGrants,
  useUsers,
  type KbGrant,
} from "@/api/client";
import { Icon } from "@/components/icons";
import { Dropdown } from "@/components/Dropdown";

export function ShareDialog({ kbId, kbName, onClose }: { kbId: string; kbName: string; onClose: () => void }) {
  const qc = useQueryClient();
  const grants = useKbGrants(kbId);
  const users = useUsers();
  const groups = useGroups();
  const [principal, setPrincipal] = useState(""); // "user:<id>" | "group:<id>"
  const [permission, setPermission] = useState<"read" | "manage">("read");
  const [busy, setBusy] = useState(false);

  const nameOf = (g: KbGrant) => g.name ?? `${g.principal_type} ${g.principal_id.slice(0, 8)}`;

  async function add() {
    if (!principal || busy) return;
    const [ptype, pid] = principal.split(":") as ["user" | "group", string];
    const label =
      ptype === "user"
        ? users.data?.find((u) => u.id === pid)?.display_name ?? "this person"
        : `the ${groups.data?.find((x) => x.id === pid)?.name ?? "group"} team`;
    // Disclosure confirmation — widening who can read this Library's contents.
    if (!(await confirmDialog({ title: "Grant access?", body: `This will let ${label} ${permission === "manage" ? "manage" : "read"} this Library's contents.`, confirmLabel: "Grant" }))) return;
    setBusy(true);
    try {
      await putKbGrant(kbId, { principal_type: ptype, principal_id: pid, permission });
      setPrincipal("");
      setPermission("read");
      await qc.invalidateQueries({ queryKey: ["kb-grants", kbId] });
    } catch (e) {
      toast(`Share failed: ${(e as Error).message}`);
    } finally {
      setBusy(false);
    }
  }

  async function remove(g: KbGrant) {
    if (!(await confirmDialog({ title: `Remove ${nameOf(g)}'s access?`, danger: true, confirmLabel: "Remove" }))) return;
    try {
      await deleteKbGrant(kbId, g.id);
      await qc.invalidateQueries({ queryKey: ["kb-grants", kbId] });
    } catch (e) {
      toast(`Remove failed: ${(e as Error).message}`);
    }
  }

  return (
    <div className="modal-scrim" onClick={onClose}>
      <div className="modal" style={{ width: 520, maxWidth: "100%" }} onClick={(e) => e.stopPropagation()}>
        <div className="modal-head">
          <div>
            <div className="eyebrow">Manage access</div>
            <h2 className="serif modal-title">{kbName}</h2>
          </div>
          <button className="icon-btn" onClick={onClose}><Icon.Close size={18} /></button>
        </div>
        <div className="modal-body">
          <label className="form-label">Add a person or team</label>
          <div className="row" style={{ gap: 8 }}>
            <div style={{ flex: 1 }}>
              <Dropdown
                value={principal}
                onChange={setPrincipal}
                ariaLabel="User or group to share with"
                fullWidth
                options={[
                  { value: "", label: "Select a user or group…" },
                  ...(users.data ?? []).map((u) => ({ value: `user:${u.id}`, label: u.display_name || u.email, group: "People" })),
                  ...(groups.data ?? []).map((g) => ({ value: `group:${g.id}`, label: g.name, group: "Teams" })),
                ]}
              />
            </div>
            <Dropdown
              value={permission}
              onChange={setPermission}
              ariaLabel="Permission"
              options={[
                { value: "read", label: "Can read" },
                { value: "manage", label: "Can manage" },
              ]}
            />
            <button className="btn btn-gold sm" onClick={add} disabled={!principal || busy}>
              <Icon.Plus size={14} /> Add
            </button>
          </div>

          <div className="ed-hint mono" style={{ marginTop: 10 }}>
            Retrieval honours each person's own access — a teammate sees a source only if they too are granted it.
          </div>

          <div className="rows" style={{ marginTop: 14 }}>
            {grants.isLoading && <p className="text-sm text-slate">Loading…</p>}
            {grants.data?.length === 0 && <p className="ed-hint mono">No one else has access yet.</p>}
            {grants.data?.map((g) => (
              <div key={g.id} className="list-row">
                <span className="avatar">{g.principal_type === "group" ? <Icon.Team size={14} /> : <Icon.User size={14} />}</span>
                <div className="foot-id">
                  <span className="foot-name">{nameOf(g)}</span>
                  <span className="foot-org mono">{g.principal_type}</span>
                </div>
                <span className="role-chip mono">{g.permission}</span>
                <button className="icon-btn" title="Remove access" onClick={() => remove(g)}><Icon.Close size={15} /></button>
              </div>
            ))}
          </div>
        </div>
      </div>
    </div>
  );
}
