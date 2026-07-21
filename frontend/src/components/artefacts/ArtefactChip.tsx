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

// The artefact chip under an answer. The chip itself opens the artefact beside
// the chat (the rich render lives there, not in the thread); the download icon
// stays as a secondary one-click action for people who only want the file.

import type { Artefact } from "@/api/client";
import { Icon } from "@/components/icons";

export function ArtefactChip({
  artefact: a,
  onOpen,
  onDownload,
  selected,
}: {
  artefact: Artefact;
  onOpen: (a: Artefact) => void;
  onDownload: (a: Artefact) => void;
  selected?: boolean;
}) {
  return (
    <span className={"artefact-chip-wrap" + (selected ? " selected" : "")}>
      <button
        className="artefact-chip"
        title={`Open ${a.title}`}
        aria-pressed={selected ? true : undefined}
        onClick={() => onOpen(a)}
      >
        <Icon.Doc size={14} />
        <span className="artefact-name">{a.title}</span>
        <span className="artefact-kind mono">{a.kind}</span>
      </button>
      <button
        className="artefact-chip-dl"
        title={`Download ${a.title}`}
        aria-label={`Download ${a.title}`}
        onClick={() => onDownload(a)}
      >
        <Icon.Download size={14} />
      </button>
    </span>
  );
}
