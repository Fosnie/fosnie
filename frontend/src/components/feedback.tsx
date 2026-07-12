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
import { submitFeedback } from "@/api/client";
import { Icon } from "@/components/icons";

// Message feedback shared by the general chat and Legal mode: a thumb records the
// rating instantly (audited, admin-visible), then a modal offers an optional
// comment that attaches to the SAME feedback row (backend upserts per message+user).
export function useFeedback() {
  const [feedback, setFeedback] = useState<Record<string, "up" | "down">>({});
  const [pending, setPending] = useState<{ id: string; rating: "up" | "down" } | null>(null);

  function rate(id: string, r: "up" | "down") {
    setFeedback((p) => ({ ...p, [id]: r }));
    submitFeedback(id, r).catch(() => {}); // instant record
    setPending({ id, rating: r }); // then offer a comment
  }

  const modal = pending ? (
    <FeedbackCommentModal id={pending.id} rating={pending.rating} onClose={() => setPending(null)} />
  ) : null;

  return { feedback, rate, modal };
}

function FeedbackCommentModal({ id, rating, onClose }: { id: string; rating: "up" | "down"; onClose: () => void }) {
  const [text, setText] = useState("");
  const [busy, setBusy] = useState(false);

  async function save() {
    const c = text.trim();
    if (!c || busy) return;
    setBusy(true);
    try {
      await submitFeedback(id, rating, c);
      onClose();
    } catch (e) {
      toast(`Could not save comment: ${(e as Error).message}`);
      setBusy(false);
    }
  }

  return (
    <div className="modal-scrim" onClick={onClose}>
      <div className="modal" style={{ width: 480, maxWidth: "100%" }} onClick={(e) => e.stopPropagation()}>
        <div className="modal-head">
          <div>
            <div className="eyebrow">{rating === "up" ? "Thanks for the feedback" : "Thanks for the feedback"}</div>
            <h2 className="serif modal-title">
              {rating === "up" ? <Icon.Like size={18} /> : <Icon.Dislike size={18} />} Add a comment?
            </h2>
          </div>
          <button className="icon-btn" onClick={onClose}><Icon.Close size={18} /></button>
        </div>
        <div className="modal-body">
          <p className="ed-hint mono" style={{ marginBottom: 8 }}>
            Your rating is saved. A comment is optional — it helps power users improve the agent.
          </p>
          <textarea
            className="field"
            rows={4}
            autoFocus
            value={text}
            placeholder={rating === "up" ? "What worked well? (optional)" : "What went wrong? (optional)"}
            onChange={(e) => setText(e.target.value)}
            onKeyDown={(e) => { if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) save(); }}
            style={{ resize: "vertical" }}
          />
        </div>
        <div className="modal-foot">
          <button className="btn btn-ghost" onClick={onClose}>Skip</button>
          <button className="btn btn-gold" onClick={save} disabled={busy || !text.trim()}>{busy ? "Saving…" : "Save comment"}</button>
        </div>
      </div>
    </div>
  );
}
