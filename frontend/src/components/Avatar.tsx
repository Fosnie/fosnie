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

import { type ReactNode, useEffect, useState } from "react";
import { useUsers } from "@/api/client";
import { apiRequest } from "@/api/instance";

/** Two-letter initials from a name or email — the avatar fallback. */
export function initialsOf(name?: string | null): string {
  if (!name) return "··";
  const parts = name.replace(/@.*/, "").split(/[ ._-]+/).filter(Boolean);
  return ((parts[0]?.[0] ?? "") + (parts[1]?.[0] ?? "")).toUpperCase() || name.slice(0, 2).toUpperCase();
}

// The avatar route is credential-gated, so a plain <img src> 401s. Fetch the
// bytes once per (id, version) and hand the <img> a blob URL. Cached +
// in-flight-deduped across every avatar on screen; URLs live for the session.
const urlCache = new Map<string, string>();
const inflight = new Map<string, Promise<string | null>>();

function loadAvatar(id: string, ts: number): Promise<string | null> {
  const key = `${id}:${ts}`;
  const cached = urlCache.get(key);
  if (cached) return Promise.resolve(cached);
  let pending = inflight.get(key);
  if (!pending) {
    pending = (async () => {
      try {
        const res = await apiRequest(`/api/users/${id}/avatar?v=${ts}`);
        if (!res.ok) return null;
        const url = URL.createObjectURL(await res.blob());
        urlCache.set(key, url);
        return url;
      } catch {
        return null;
      } finally {
        inflight.delete(key);
      }
    })();
    inflight.set(key, pending);
  }
  return pending;
}

/**
 * A user avatar: the uploaded image when there is one, otherwise initials.
 *
 * Pass `avatarUpdatedAt` (an epoch, doubling as cache key) when you already hold
 * it — e.g. the sidebar footer from `whoami`. Otherwise leave it `undefined` and
 * the component self-resolves from the cached user directory, so any chat surface
 * can just render `<Avatar id name className="gmsg-av" />`. `className` keeps the
 * existing per-surface box styling (`avatar`, `avatar sm`, `gmsg-av`…).
 */
export function Avatar({
  id,
  name,
  email,
  avatarUpdatedAt,
  className = "avatar",
  title,
  overlay,
}: {
  id?: string | null;
  name?: string | null;
  email?: string | null;
  avatarUpdatedAt?: number | null;
  className?: string;
  /** Tooltip on the chip (e.g. the presence row shows the member's name). */
  title?: string;
  /** Extra node rendered inside the chip, over the image/initials — e.g. a
   *  presence dot — so it stays positioned relative to the avatar box. */
  overlay?: ReactNode;
}) {
  const dir = useUsers();
  const ts =
    avatarUpdatedAt !== undefined
      ? avatarUpdatedAt
      : id
        ? (dir.data?.find((u) => u.id === id)?.avatar_updated_at ?? null)
        : null;

  const [url, setUrl] = useState<string | null>(() =>
    id && ts ? (urlCache.get(`${id}:${ts}`) ?? null) : null,
  );

  useEffect(() => {
    let alive = true;
    if (id && ts) {
      const cached = urlCache.get(`${id}:${ts}`);
      if (cached) {
        setUrl(cached);
      } else {
        setUrl(null);
        loadAvatar(id, ts).then((u) => alive && setUrl(u));
      }
    } else {
      setUrl(null);
    }
    return () => {
      alive = false;
    };
  }, [id, ts]);

  if (id && ts && url) {
    return (
      <span className={className} title={title}>
        <img className="avatar-img" src={url} alt="" />
        {overlay}
      </span>
    );
  }
  return (
    <span className={className} title={title}>
      {initialsOf(name ?? email)}
      {overlay}
    </span>
  );
}
