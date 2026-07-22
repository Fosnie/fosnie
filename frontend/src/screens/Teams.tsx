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
import { useEffect, useMemo, useRef, useState } from "react";
import { useNavigate, useParams } from "react-router-dom";
import { useQueryClient } from "@tanstack/react-query";
import {
  addGroupChatMember,
  createGroupChat,
  createNote,
  deleteNote,
  fetchGroupMessages,
  messageAttachmentBlob,
  messageAttachmentUrl,
  removeGroupChatMember,
  sendGroupMessageRest,
  toggleReaction,
  updateNote,
  uploadMessageAttachment,
  useUsers,
  useGroupChat,
  useGroupChats,
  useGroupNotes,
  useWhoami,
  type GroupChatSummary,
  type GroupMessage,
  type GroupNote,
  type MessageAttachment,
  type ReactionAgg,
} from "@/api/client";
import { saveBlob } from "@/api/instance";
import { Avatar } from "@/components/Avatar";
import { Icon } from "@/components/icons";
import { Dropdown } from "@/components/Dropdown";
import { EmojiPicker } from "@/components/EmojiPicker";
import { wsStore, useWsStatus } from "@/ws/store";
import type { ServerFrame } from "@/ws/protocol";

function useUserNames() {
  const users = useUsers(); // admin-only; errors → empty map (non-admins)
  return useMemo(() => {
    const m = new Map<string, string>();
    users.data?.forEach((u) => m.set(u.id, u.email));
    return m;
  }, [users.data]);
}

export function Teams() {
  const { chatId } = useParams();
  const nav = useNavigate();
  const qc = useQueryClient();
  const chats = useGroupChats();
  const [creating, setCreating] = useState(false);
  const [q, setQ] = useState("");
  const ql = q.trim().toLowerCase();
  const match = (c: { name: string | null; kind: string }) => !ql || (c.name ?? c.kind).toLowerCase().includes(ql);
  // Project chats (auto, one per Project) and standalone Group chats are shown as
  // two sections; DMs are deferred to their own tab and hidden here.
  const projectChats = (chats.data ?? []).filter((c) => c.kind === "project" && match(c));
  const groupChats = (chats.data ?? []).filter((c) => c.kind === "group" && match(c));

  const item = (c: { id: string; name: string | null; kind: string; unread_count: number }) => (
    <button key={c.id} className={"group-item" + (chatId === c.id ? " on" : "")} onClick={() => nav(`/teams/${c.id}`)}>
      <div className="group-top">
        <span className="group-name">{c.name ?? c.kind}</span>
        {c.unread_count > 0 && chatId !== c.id && <span className="proj-count mono" style={{ marginLeft: "auto" }}>{c.unread_count}</span>}
      </div>
    </button>
  );

  return (
    <div className={"teams-shell" + (chatId ? " chat-open" : "")}>
      <aside className="teams-rail">
        <div className="teams-rail-head">
          <h4 style={{ margin: 0 }}>Team chats</h4>
          <button className="side-add" onClick={() => setCreating(true)} title="New group chat"><Icon.Plus size={15} /></button>
        </div>
        <div className="search-box" style={{ margin: "0 0 10px" }}>
          <Icon.Search size={14} /><input className="search-in" placeholder="Search chats" value={q} onChange={(e) => setQ(e.target.value)} />
        </div>
        {chats.isLoading && <div className="side-empty">Loading…</div>}
        {!chats.isLoading && projectChats.length === 0 && groupChats.length === 0 && <div className="side-empty">No team chats yet.</div>}
        {projectChats.length > 0 && <span className="side-label mono" style={{ display: "block", margin: "4px 0 6px" }}>Projects</span>}
        {projectChats.map(item)}
        {groupChats.length > 0 && <span className="side-label mono" style={{ display: "block", margin: "14px 0 6px" }}>Groups</span>}
        {groupChats.map(item)}
      </aside>

      {chatId ? (
        <ChatMain key={chatId} chatId={chatId} />
      ) : (
        <section className="teams-main">
          <div className="empty"><span className="empty-mark"><Icon.Team size={24} /></span><p className="empty-sub">Select a chat, or create one.</p></div>
        </section>
      )}

      {chatId ? <NotesPanel key={`notes-${chatId}`} chatId={chatId} /> : <aside className="notes-side" />}

      {creating && <CreateChatModal onClose={() => setCreating(false)} onCreated={(id) => { setCreating(false); qc.invalidateQueries({ queryKey: ["group-chats"] }); nav(`/teams/${id}`); }} />}
    </div>
  );
}

export function ChatMain({ chatId }: { chatId: string }) {
  const qc = useQueryClient();
  const nav = useNavigate();
  const who = useWhoami();
  const detail = useGroupChat(chatId);
  const names = useUserNames();
  const wsStatus = useWsStatus();
  const dir = useUsers();
  const [messages, setMessages] = useState<GroupMessage[]>([]);
  const [input, setInput] = useState("");
  const [search, setSearch] = useState("");
  const [showMembers, setShowMembers] = useState(false);
  const [pendingAtt, setPendingAtt] = useState<MessageAttachment[]>([]);
  const [uploading, setUploading] = useState(false);
  const [composerAnchor, setComposerAnchor] = useState<DOMRect | null>(null);
  const [reactMenu, setReactMenu] = useState<{ id: string; rect: DOMRect } | null>(null);
  const fileRef = useRef<HTMLInputElement | null>(null);
  const bottom = useRef<HTMLDivElement | null>(null);
  const meId = who.data?.user_id;

  // Update one message's reactions in place.
  function mutateReactions(messageId: string, fn: (rs: ReactionAgg[]) => ReactionAgg[]) {
    setMessages((p) => p.map((m) => (m.id === messageId ? { ...m, reactions: fn(m.reactions ?? []) } : m)));
  }
  // Toggle my reaction: optimistic local update + server call (the WS echo for my
  // own user is ignored, so no double count).
  function react(messageId: string, emoji: string) {
    setReactMenu(null);
    const msg = messages.find((m) => m.id === messageId);
    const wasMine = !!msg?.reactions?.find((r) => r.emoji === emoji)?.mine;
    mutateReactions(messageId, (rs) => {
      const cur = rs.find((r) => r.emoji === emoji);
      if (cur) {
        return rs
          .map((r) => (r.emoji === emoji ? { ...r, count: r.count + (wasMine ? -1 : 1), mine: !wasMine } : r))
          .filter((r) => r.count > 0);
      }
      return [...rs, { emoji, count: 1, mine: true }];
    });
    toggleReaction(chatId, messageId, emoji).catch((e) => toast(`Reaction failed: ${(e as Error).message}`));
  }

  // Mentions are scoped to the chat's own members — you can't @ someone who
  // isn't in the chat.
  const memberIds = new Set((detail.data?.members ?? []).map((m) => m.user_id));
  const memberUsers = (dir.data ?? []).filter((u) => memberIds.has(u.id));
  const mtok = /(?:^|\s)@([\p{L}\w.\-]*)$/u.exec(input);
  const mquery = mtok ? mtok[1].toLowerCase() : null;
  const suggestions = mquery != null
    ? memberUsers.filter((u) => u.display_name.toLowerCase().includes(mquery) || u.email.toLowerCase().includes(mquery)).slice(0, 6)
    : [];

  function pickMention(name: string) {
    const idx = input.lastIndexOf("@");
    setInput((idx >= 0 ? input.slice(0, idx) : input) + "@" + name + " ");
  }
  function mentionIds(content: string): string[] {
    return memberUsers.filter((u) => content.includes("@" + u.display_name)).map((u) => u.id);
  }
  function renderMentions(text: string) {
    return text.split(/(@[\p{L}\w.\-]+)/u).map((p, i) => (p.startsWith("@") ? <span key={i} className="mention">{p}</span> : <span key={i}>{p}</span>));
  }

  useEffect(() => {
    let cancelled = false;
    fetchGroupMessages(chatId, 0).then((m) => {
      if (cancelled) return;
      setMessages(m);
      // Opening the chat marked it read server-side; refresh the sidebar unread
      // badge now instead of waiting for the next message / refocus.
      qc.setQueryData<GroupChatSummary[]>(["group-chats"], (prev) =>
        prev?.map((c) => (c.id === chatId ? { ...c, unread_count: 0 } : c)));
      qc.invalidateQueries({ queryKey: ["group-chats"] });
    }).catch(() => {});
    return () => { cancelled = true; };
  }, [chatId, qc]);

  useEffect(() => {
    return wsStore.onFrame((f: ServerFrame) => {
      const ft = (f as { type: string }).type;
      if (ft === "group.message") {
        const g = f as unknown as GroupMessage & { chat_id: string };
        if (g.chat_id !== chatId) return;
        setMessages((p) => (p.some((m) => m.id === g.id) ? p : [...p, g]));
      } else if (ft === "group.reaction") {
        const g = f as unknown as { chat_id: string; message_id: string; emoji: string; user_id: string; added: boolean };
        if (g.chat_id !== chatId || g.user_id === meId) return; // my own change is already applied
        mutateReactions(g.message_id, (rs) => {
          const cur = rs.find((r) => r.emoji === g.emoji);
          if (cur) {
            return rs
              .map((r) => (r.emoji === g.emoji ? { ...r, count: r.count + (g.added ? 1 : -1) } : r))
              .filter((r) => r.count > 0);
          }
          return g.added ? [...rs, { emoji: g.emoji, count: 1, mine: false }] : rs;
        });
      }
    });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [chatId, meId]);

  useEffect(() => { bottom.current?.scrollIntoView({ behavior: "smooth" }); }, [messages]);

  const myRole = detail.data?.members.find((m) => m.user_id === who.data?.user_id)?.role;
  const canManage = myRole === "owner" || myRole === "admin";

  async function onPickFiles(files: FileList | File[] | null) {
    if (!files || !files.length) return;
    setUploading(true);
    try {
      const ups = await Promise.all(Array.from(files).map((f) => uploadMessageAttachment(f)));
      setPendingAtt((p) => [...p, ...ups]);
    } catch (e) {
      toast(`Upload failed: ${(e as Error).message}`);
    } finally {
      setUploading(false);
      if (fileRef.current) fileRef.current.value = "";
    }
  }

  function send() {
    const content = input.trim();
    const atts = pendingAtt;
    if (!content && !atts.length) return;
    setInput("");
    setPendingAtt([]);
    const mentions = mentionIds(content);
    // Attachments ride the REST send (the WS frame carries text only). Plain text
    // still goes live over the socket when it's open.
    if (!atts.length && wsStatus === "open") {
      wsStore.send({ type: "group.send", chat_id: chatId, content, mentions: mentions.length ? mentions : undefined });
    } else {
      sendGroupMessageRest(chatId, content, { attachments: atts, mentions })
        .then((r) => setMessages((p) => (p.some((m) => m.id === r.id) ? p : [...p, { id: r.id, seq: r.seq, sender_user_id: who.data?.user_id ?? null, message_type: "user", content, created_at: r.created_at, attachments: atts }])))
        .catch((e) => toast(`Send failed: ${(e as Error).message}`));
    }
  }
  function label(uid: string | null): string {
    if (!uid) return "system";
    if (uid === who.data?.user_id) return "You";
    return names.get(uid) ?? uid.slice(0, 8);
  }
  const time = (iso: string) => new Date(iso).toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" });

  const shown = search ? messages.filter((m) => m.content.toLowerCase().includes(search.toLowerCase())) : messages;
  const members = detail.data?.members ?? [];

  // The backend fills a DM's name with the other participant; members management
  // is hidden for DMs (1:1 — use a group chat to add people).
  const isDm = detail.data?.kind === "dm";
  const headerTitle = detail.data?.name ?? (isDm ? "Direct message" : detail.data?.kind ?? "Chat");

  return (
    <Dropzone className="teams-main" onFiles={onPickFiles}>
      <div className="teams-head">
        <div>
          <h2 className="serif teams-title">{headerTitle}</h2>
          <div className="presence">
            {members.slice(0, 8).map((m) => (
              <Avatar
                key={m.user_id}
                id={m.user_id}
                name={names.get(m.user_id) ?? m.user_id}
                className="pres-av"
                title={names.get(m.user_id) ?? m.user_id}
                overlay={<span className="pres-dot on" />}
              />
            ))}
            {!isDm && <button className="icon-btn" title="Manage members" onClick={() => setShowMembers((v) => !v)}><Icon.Plus size={14} /></button>}
          </div>
        </div>
        <div className="teams-search">
          <Icon.Search size={14} />
          <input className="search-in" placeholder="Search messages" value={search} onChange={(e) => setSearch(e.target.value)} />
        </div>
      </div>

      {!isDm && showMembers && detail.data && (
        <MembersBar chatId={chatId} members={detail.data.members} names={names} canManage={canManage} onChange={() => qc.invalidateQueries({ queryKey: ["group-chat", chatId] })} />
      )}

      <div className="teams-thread">
        {shown.length === 0 && <div className="side-empty">{search ? `No messages match “${search}”.` : "No messages yet. Say hello."}</div>}
        {shown.map((m) =>
          m.message_type === "system" ? (
            <div key={m.id} className="side-empty" style={{ alignSelf: "center" }}>
              {m.content}
              {m.shared_resources?.chat_id && (
                <button className="btn btn-line sm" style={{ marginLeft: 8 }} onClick={() => nav(`/c/${m.shared_resources!.chat_id}`)}>Open chat <Icon.ChevronR size={13} /></button>
              )}
            </div>
          ) : (
            <div key={m.id} className={"gmsg fade-up" + (m.sender_user_id === who.data?.user_id ? " me" : "")}>
              {m.sender_user_id !== who.data?.user_id && <Avatar id={m.sender_user_id} name={label(m.sender_user_id)} className="gmsg-av" />}
              <div className="gmsg-col">
                <div className="gmsg-head"><span className="gmsg-name">{label(m.sender_user_id)}</span><span className="gmsg-time mono">{time(m.created_at)}</span></div>
                {m.content.trim() && <div className="gmsg-bubble">{renderMentions(m.content)}</div>}
                {(m.attachments ?? []).map((a) => <AttachmentView key={a.id} att={a} />)}
                {m.shared_resources?.chat_id && (
                  <button className="btn btn-line sm" style={{ marginTop: 6, alignSelf: "flex-start" }} onClick={() => nav(`/c/${m.shared_resources!.chat_id}`)}>Open chat <Icon.ChevronR size={13} /></button>
                )}
                <div className="reaction-row">
                  {(m.reactions ?? []).map((r) => (
                    <button key={r.emoji} className={"reaction-pill" + (r.mine ? " on" : "")} onClick={() => react(m.id, r.emoji)} title={r.mine ? "Remove reaction" : "React"}>
                      <span className="reaction-emoji">{r.emoji}</span> {r.count}
                    </button>
                  ))}
                  <button className="react-add" title="Add reaction" onClick={(ev) => setReactMenu({ id: m.id, rect: ev.currentTarget.getBoundingClientRect() })}>🙂<Icon.Plus size={10} /></button>
                </div>
              </div>
            </div>
          ),
        )}
        <div ref={bottom} />
      </div>

      <div className="gcomposer">
        {suggestions.length > 0 && (
          <div className="mention-pop">
            <div className="menu-label mono">Mention</div>
            {suggestions.map((u) => (
              <button key={u.id} className="mention-row" onMouseDown={(e) => { e.preventDefault(); pickMention(u.display_name); }}>
                <Avatar id={u.id} name={u.display_name} avatarUpdatedAt={u.avatar_updated_at} className="avatar sm" /> {u.display_name}
                <span className="group-last" style={{ marginLeft: "auto" }}>{u.email}</span>
              </button>
            ))}
          </div>
        )}
        {pendingAtt.length > 0 && (
          <div className="chip-wrap" style={{ padding: "0 0 8px" }}>
            {pendingAtt.map((a) => (
              <span key={a.id} className="skill-chip">
                <Icon.Attach size={12} /> {a.filename}
                <button onClick={() => setPendingAtt((p) => p.filter((x) => x.id !== a.id))} style={{ background: "none", border: 0, color: "var(--red)", cursor: "pointer" }}><Icon.Close size={11} /></button>
              </span>
            ))}
          </div>
        )}
        <div className="composer">
          <input ref={fileRef} type="file" accept={ACCEPT_ATTR} multiple hidden onChange={(e) => onPickFiles(e.target.files)} />
          <div className="comp-tools">
            <button className="comp-attach" title="Attach a file" disabled={uploading} onClick={() => fileRef.current?.click()}><Icon.Attach size={18} /></button>
            <button className="comp-attach" title="Emoji" onClick={(ev) => setComposerAnchor(ev.currentTarget.getBoundingClientRect())}><Icon.Smile size={18} /></button>
          </div>
          <textarea
            className="comp-in"
            rows={1}
            value={input}
            placeholder={uploading ? "Uploading…" : "Message… use @ to mention"}
            onChange={(e) => setInput(e.target.value)}
            onKeyDown={(e) => { if (e.key === "Enter" && !e.shiftKey) { e.preventDefault(); send(); } }}
          />
          <button className={"comp-send" + (input.trim() || pendingAtt.length ? " ready" : "")} onClick={send} disabled={!input.trim() && !pendingAtt.length}><Icon.Send size={17} /></button>
        </div>
      </div>

      {composerAnchor && (
        <EmojiPicker anchor={composerAnchor} onPick={(e) => { setInput((v) => v + e); setComposerAnchor(null); }} onClose={() => setComposerAnchor(null)} />
      )}
      {reactMenu && (
        <EmojiPicker anchor={reactMenu.rect} onPick={(e) => react(reactMenu.id, e)} onClose={() => setReactMenu(null)} />
      )}
    </Dropzone>
  );
}

// Render a message attachment: images inline (authed blob), other files as a
// download chip. Object URLs are revoked on unmount.
function AttachmentView({ att }: { att: MessageAttachment }) {
  const isImage = att.mime.startsWith("image/");
  const [url, setUrl] = useState<string | null>(null);
  useEffect(() => {
    let u: string | null = null;
    let cancelled = false;
    if (isImage) {
      messageAttachmentUrl(att.id).then((x) => { if (cancelled) { URL.revokeObjectURL(x); } else { u = x; setUrl(x); } }).catch(() => {});
    }
    return () => { cancelled = true; if (u) URL.revokeObjectURL(u); };
  }, [att.id, isImage]);
  // Save rather than open in a tab: the bytes come from a credential-gated route,
  // so there is no URL a new tab could load on its own.
  async function open() {
    try { saveBlob(await messageAttachmentBlob(att.id), att.filename); }
    catch (e) { toast((e as Error).message); }
  }
  if (isImage) {
    return url
      ? <img src={url} alt={att.filename} onClick={open} style={{ maxWidth: 280, maxHeight: 280, borderRadius: 8, marginTop: 6, cursor: "pointer", display: "block" }} />
      : <div className="ed-hint mono" style={{ marginTop: 6 }}>Loading image…</div>;
  }
  return (
    <button className="skill-chip" style={{ marginTop: 6 }} onClick={open}>
      <Icon.Attach size={13} /> {att.filename}
    </button>
  );
}

function MembersBar({
  chatId, members, names, canManage, onChange,
}: {
  chatId: string;
  members: { user_id: string; role: string }[];
  names: Map<string, string>;
  canManage: boolean;
  onChange: () => void;
}) {
  const users = useUsers();
  const [addId, setAddId] = useState("");
  const memberIds = new Set(members.map((m) => m.user_id));

  return (
    <div style={{ padding: "10px 26px", borderBottom: "1px solid var(--line)", background: "var(--bg-0)" }}>
      <div className="chip-wrap" style={{ marginBottom: canManage ? 10 : 0 }}>
        {members.map((m) => (
          <span key={m.user_id} className="skill-chip">
            <Avatar id={m.user_id} name={names.get(m.user_id) ?? m.user_id} className="avatar sm" />
            {names.get(m.user_id) ?? m.user_id.slice(0, 8)} <span className="group-last">{m.role}</span>
            {canManage && m.role !== "owner" && (
              <button onClick={() => removeGroupChatMember(chatId, m.user_id).then(onChange).catch((e) => toast((e as Error).message))} style={{ background: "none", border: 0, color: "var(--red)", cursor: "pointer" }}><Icon.Close size={12} /></button>
            )}
          </span>
        ))}
      </div>
      {canManage && (
        <div className="col-add">
          <div style={{ flex: 1 }}>
            <Dropdown
              value={addId}
              onChange={setAddId}
              ariaLabel="Add member"
              fullWidth
              options={[
                { value: "", label: "Add member…" },
                ...(users.data ?? []).filter((u) => !memberIds.has(u.id)).map((u) => ({ value: u.id, label: u.email })),
              ]}
            />
          </div>
          <button className="btn btn-ghost sm" disabled={!addId} onClick={() => addId && addGroupChatMember(chatId, addId).then(() => { setAddId(""); onChange(); }).catch((e) => toast((e as Error).message))}>Add</button>
        </div>
      )}
    </div>
  );
}

function NotesPanel({ chatId }: { chatId: string }) {
  const qc = useQueryClient();
  const notes = useGroupNotes(chatId);
  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);
  const refresh = () => qc.invalidateQueries({ queryKey: ["group-notes", chatId] });

  async function add() {
    setBusy(true);
    try { await createNote(chatId, ""); await refresh(); } catch (e) { toast((e as Error).message); } finally { setBusy(false); }
  }

  return (
    <aside className="notes-side">
      <div className="notes-head">
        <span className="side-label mono">Shared notes</span>
        <button className="side-add" onClick={add} disabled={busy}><Icon.Plus size={14} /></button>
      </div>
      {notice && <p className="border-b border-gold-dark/40 bg-gold/10 px-4 py-1.5 text-xs text-gold-light">{notice} <button onClick={() => setNotice(null)} className="underline">ok</button></p>}
      <div className="notes-list">
        {notes.isLoading && <div className="side-empty">Loading…</div>}
        {notes.data?.length === 0 && <div className="side-empty">No notes yet.</div>}
        {notes.data?.map((n) => (
          <NoteRow key={n.id} chatId={chatId} note={n} onChanged={refresh} onConflict={() => { setNotice("Note changed elsewhere — reloaded."); refresh(); }} />
        ))}
      </div>
    </aside>
  );
}

function NoteRow({ chatId, note, onChanged, onConflict }: { chatId: string; note: GroupNote; onChanged: () => void; onConflict: () => void }) {
  const [draft, setDraft] = useState(note.content);
  const [busy, setBusy] = useState(false);
  const dirty = draft !== note.content;

  async function save() {
    setBusy(true);
    try { await updateNote(chatId, note.id, draft, note.version); onChanged(); }
    catch (e) {
      const msg = (e as Error).message;
      if (msg.includes("409") || msg.toLowerCase().includes("conflict")) onConflict();
      else toast(`Save failed: ${msg}`);
    } finally { setBusy(false); }
  }

  return (
    <div className="note-card">
      <textarea className="field sm" rows={3} value={draft} onChange={(e) => setDraft(e.target.value)} placeholder="Shared note…" style={{ resize: "vertical" }} />
      <div className="note-actions">
        <button className="btn btn-ghost sm" disabled={busy} onClick={async () => { if (await confirmDialog({ title: "Delete note?", danger: true, confirmLabel: "Delete" })) { setBusy(true); deleteNote(chatId, note.id).then(onChanged).catch((e) => toast((e as Error).message)).finally(() => setBusy(false)); } }}>Delete</button>
        <button className="btn btn-gold sm" disabled={busy || !dirty} onClick={save}><Icon.Save size={13} /> {busy ? "…" : "Save"}</button>
      </div>
    </div>
  );
}

function CreateChatModal({ onClose, onCreated }: { onClose: () => void; onCreated: (id: string) => void }) {
  const users = useUsers();
  const [name, setName] = useState("");
  const [members, setMembers] = useState<Set<string>>(new Set());
  const [busy, setBusy] = useState(false);

  function toggle(id: string) {
    setMembers((p) => { const n = new Set(p); if (n.has(id)) n.delete(id); else n.add(id); return n; });
  }
  async function submit() {
    if (busy) return;
    setBusy(true);
    // Always a standalone group chat — project chats are auto-created per Project,
    // DMs live in their own (future) tab.
    try { const { id } = await createGroupChat({ name: name.trim() || undefined, member_user_ids: [...members] }); onCreated(id); }
    catch (e) { toast(`Create failed: ${(e as Error).message}`); setBusy(false); }
  }

  return (
    <div className="modal-scrim" onClick={onClose}>
      <div className="modal" style={{ width: 520, maxWidth: "100%" }} onClick={(e) => e.stopPropagation()}>
        <div className="modal-head">
          <div><div className="eyebrow">Teams</div><h2 className="serif modal-title">New group chat</h2></div>
          <button className="icon-btn" onClick={onClose}><Icon.Close size={18} /></button>
        </div>
        <div className="modal-body">
          <label className="form-label">Name</label>
          <input className="field" value={name} onChange={(e) => setName(e.target.value)} placeholder="e.g. Legal review" />

          <label className="form-label">Members <span className="opt">you're added automatically</span></label>
          {users.data?.length ? (
            <div className="kb-list scroll">
              {users.data.map((u) => (
                <button key={u.id} className={"kb-opt" + (members.has(u.id) ? " on" : "")} onClick={() => toggle(u.id)}>
                  <span className="kb-check">{members.has(u.id) && <Icon.Check size={13} />}</span>
                  <Icon.User size={15} /><span className="kb-name">{u.email}</span>
                </button>
              ))}
            </div>
          ) : (
            <p className="ed-hint mono">No user directory available (admin-only). Create the chat; an admin can add members.</p>
          )}
        </div>
        <div className="modal-foot">
          <button className="btn btn-ghost" onClick={onClose}>Cancel</button>
          <button className="btn btn-gold" onClick={submit} disabled={busy}>{busy ? "Creating…" : "Create"}</button>
        </div>
      </div>
    </div>
  );
}
