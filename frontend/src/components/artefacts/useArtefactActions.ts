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

// The actions that can be taken on a generated artefact, in one place. Both the
// chip under a message and the artefact panel's header offer these, and the
// legal shell offers the download; keeping them here stops the same
// convert-then-download-then-invalidate dance being written three times.

import { useQueryClient } from "@tanstack/react-query";
import { useCallback, useState } from "react";

import { convertArtefact, createPage, downloadArtefact, startVerifyDraft, type Artefact } from "@/api/client";
import { toast } from "@/components/dialogs";

export interface ArtefactActions {
  download: (a: Artefact) => void;
  /** Convert a markdown artefact and download the result. */
  convert: (a: Artefact, to: "docx" | "pdf") => void;
  /** Turn a Deep Research report into a self-contained HTML page. */
  toPage: (a: Artefact) => void;
  verify: (a: Artefact) => void;
  /** Verification run id per artefact; "starting" while the POST is in flight. */
  verifyRuns: Record<string, string>;
}

export function useArtefactActions(chatId: string | undefined): ArtefactActions {
  const qc = useQueryClient();
  const [verifyRuns, setVerifyRuns] = useState<Record<string, string>>({});
  const refresh = useCallback(() => {
    if (chatId) qc.invalidateQueries({ queryKey: ["artefacts", chatId] });
  }, [qc, chatId]);

  const download = useCallback((a: Artefact) => {
    downloadArtefact(a.id, a.title, a.kind).catch((e) => toast((e as Error).message));
  }, []);

  const convert = useCallback(
    (a: Artefact, to: "docx" | "pdf") => {
      convertArtefact(a.id, to)
        .then((n) => {
          void downloadArtefact(n.id, n.title, n.kind);
          refresh();
        })
        .catch((e) => toast((e as Error).message));
    },
    [refresh],
  );

  const toPage = useCallback(
    (a: Artefact) => {
      createPage(a.id)
        .then(() => refresh())
        .catch((e) => toast((e as Error).message));
    },
    [refresh],
  );

  const verify = useCallback(async (a: Artefact) => {
    setVerifyRuns((v) => ({ ...v, [a.id]: "starting" }));
    try {
      const { run_id } = await startVerifyDraft("draft", a.id);
      setVerifyRuns((v) => ({ ...v, [a.id]: run_id }));
    } catch (e) {
      setVerifyRuns((v) => {
        const n = { ...v };
        delete n[a.id];
        return n;
      });
      toast((e as Error).message, { variant: "error" });
    }
  }, []);

  return { download, convert, toPage, verify, verifyRuns };
}
