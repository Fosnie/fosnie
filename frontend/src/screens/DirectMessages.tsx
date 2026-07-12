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

import { toast } from "@/components/dialogs";
import { useState } from "react";
import { useNavigate, useParams } from "react-router-dom";
import { startDm, useGroupChats, useUsers } from "@/api/client";
import { Icon } from "@/components/icons";
import { ChatMain } from "@/screens/Teams";

// Direct messages — 1:1 threads, reusing the Teams ChatMain (messages + live WS +
// composer + attachments). Project/group chats live under Teams; this tab is just
// person-to-person conversations.
export function DirectMessages() {
  const { chatId } = useParams();
  const nav = useNavigate();
  const chats = useGroupChats();
  const users = useUsers();
  const [picking, setPicking] = useState(false);
  const [q, setQ] = useState("");
  const ql = q.trim().toLowerCase();
  const dms = (chats.data ?? []).filter((c) => c.kind === "dm");

  async function start(userId: string) {
    try { const { id } = await startDm(userId); setPicking(false); nav(`/dm/${id}`); }
    catch (e) { toast((e as Error).message); }
  }

  return (
    <div className={"teams-shell" + (chatId ? " chat-open" : "")}>
      <aside className="teams-rail">
        <div className="teams-rail-head">
          <h4 style={{ margin: 0 }}>Direct messages</h4>
          <button className="side-add" onClick={() => setPicking(true)} title="New message"><Icon.Plus size={15} /></button>
        </div>
        {chats.isLoading && <div className="side-empty">Loading…</div>}
        {!chats.isLoading && dms.length === 0 && <div className="side-empty">No direct messages yet.</div>}
        {dms.map((c) => (
          <button key={c.id} className={"group-item" + (chatId === c.id ? " on" : "")} onClick={() => nav(`/dm/${c.id}`)}>
            <div className="group-top">
              <span className="group-name">{c.name ?? "Direct message"}</span>
              {c.unread_count > 0 && chatId !== c.id && <span className="proj-count mono" style={{ marginLeft: "auto" }}>{c.unread_count}</span>}
            </div>
          </button>
        ))}
      </aside>

      {chatId ? (
        <ChatMain key={chatId} chatId={chatId} />
      ) : (
        <section className="teams-main">
          <div className="empty"><span className="empty-mark"><Icon.Chat size={24} /></span><p className="empty-sub">Select a conversation, or start one.</p></div>
        </section>
      )}
      <aside className="notes-side" />

      {picking && (
        <div className="modal-scrim" onClick={() => setPicking(false)}>
          <div className="modal" style={{ width: 480, maxWidth: "100%" }} onClick={(e) => e.stopPropagation()}>
            <div className="modal-head">
              <div><div className="eyebrow">Direct message</div><h2 className="serif modal-title">New message</h2></div>
              <button className="icon-btn" onClick={() => setPicking(false)}><Icon.Close size={18} /></button>
            </div>
            <div className="modal-body">
              <div className="search-box" style={{ marginBottom: 10 }}>
                <Icon.Search size={14} /><input className="search-in" placeholder="Search people" value={q} onChange={(e) => setQ(e.target.value)} />
              </div>
              {users.data?.length ? (
                <div className="kb-list scroll">
                  {users.data
                    .filter((u) => !ql || u.email.toLowerCase().includes(ql) || u.display_name.toLowerCase().includes(ql))
                    .map((u) => (
                      <button key={u.id} className="kb-opt" onClick={() => start(u.id)}>
                        <Icon.User size={15} /><span className="kb-name">{u.display_name || u.email}</span>
                        <span className="group-last" style={{ marginLeft: "auto" }}>{u.email}</span>
                      </button>
                    ))}
                </div>
              ) : (
                <p className="ed-hint mono">No user directory available (admin-only).</p>
              )}
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
