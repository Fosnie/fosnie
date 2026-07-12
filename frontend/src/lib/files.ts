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

// Accepted upload formats — the set the ML extractor (ml/app/extract.py) can ingest:
// documents (native parsers) + images (OCR). Kept in sync with the backend guard
// (backend/src/upload.rs). Used by the click-pickers (`accept=`) and the dropzone.

export const ACCEPTED_EXT = [
  "pdf", "docx", "xlsx", "xlsm", "pptx", "txt", "md", "text",
  "png", "jpg", "jpeg", "webp", "bmp", "tif", "tiff", "gif",
] as const;

/** Value for an `<input type="file" accept=…>` attribute. */
export const ACCEPT_ATTR = ACCEPTED_EXT.map((e) => "." + e).join(",");

/** Human label for the allowed set (toast / overlay copy). */
export const ACCEPTED_LABEL = "PDF, DOCX, XLSX, PPTX, TXT, MD, images";

/** Upper bound on a single chat attachment — must match the backend route limit
 *  (`DefaultBodyLimit::max` on `/api/chat-attachments`, backend/src/http/mod.rs). */
export const MAX_CHAT_ATTACHMENT_BYTES = 64 * 1024 * 1024;

/** Human label for the size cap (toast copy). */
export const MAX_CHAT_ATTACHMENT_LABEL = "64 MB";

/** Partition files by the size cap. `tooBig` holds the rejected file names. */
export function splitBySize(
  files: File[],
  max = MAX_CHAT_ATTACHMENT_BYTES,
): { ok: File[]; tooBig: string[] } {
  const ok: File[] = [];
  const tooBig: string[] = [];
  for (const f of files) {
    if (f.size <= max) ok.push(f);
    else tooBig.push(f.name);
  }
  return { ok, tooBig };
}

function ext(name: string): string {
  const dot = name.lastIndexOf(".");
  return dot >= 0 ? name.slice(dot + 1).toLowerCase() : "";
}

/** Partition files by whether their extension is accepted. `bad` holds the
 *  rejected file names (for an error toast). */
export function splitFiles(files: FileList | File[]): { ok: File[]; bad: string[] } {
  const ok: File[] = [];
  const bad: string[] = [];
  for (const f of Array.from(files)) {
    if ((ACCEPTED_EXT as readonly string[]).includes(ext(f.name))) ok.push(f);
    else bad.push(f.name);
  }
  return { ok, bad };
}
