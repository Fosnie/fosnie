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

// Reusable drag-and-drop file region. Wrap any container; dropped files are
// validated against the accepted formats (lib/files.ts) — rejects raise a toast,
// the rest are handed to onFiles (the surface's existing upload handler). A gold
// overlay shows while a file drag is over the region.

import { useRef, useState, type ReactNode } from "react";
import { ACCEPTED_LABEL, splitFiles } from "@/lib/files";
import { toast } from "@/components/dialogs";
import { Icon } from "@/components/icons";

export function Dropzone({
  onFiles,
  className,
  disabled = false,
  label = "Drop files to upload",
  children,
}: {
  onFiles: (files: File[]) => void;
  className?: string;
  disabled?: boolean;
  label?: string;
  children: ReactNode;
}) {
  const [over, setOver] = useState(false);
  const depth = useRef(0);
  const carriesFiles = (e: React.DragEvent) => Array.from(e.dataTransfer.types).includes("Files");

  function onDragEnter(e: React.DragEvent) {
    if (disabled || !carriesFiles(e)) return;
    e.preventDefault();
    depth.current += 1;
    setOver(true);
  }
  function onDragOver(e: React.DragEvent) {
    if (disabled || !carriesFiles(e)) return;
    e.preventDefault();
    e.dataTransfer.dropEffect = "copy";
  }
  function onDragLeave() {
    if (disabled) return;
    depth.current = Math.max(0, depth.current - 1);
    if (depth.current === 0) setOver(false);
  }
  function onDrop(e: React.DragEvent) {
    if (disabled || !carriesFiles(e)) return;
    e.preventDefault();
    depth.current = 0;
    setOver(false);
    const { ok, bad } = splitFiles(e.dataTransfer.files);
    if (bad.length) toast(`Unsupported: ${bad.join(", ")} — allowed: ${ACCEPTED_LABEL}`, { variant: "error" });
    if (ok.length) onFiles(ok);
  }

  return (
    <div
      className={"dropzone" + (className ? " " + className : "")}
      onDragEnter={onDragEnter}
      onDragOver={onDragOver}
      onDragLeave={onDragLeave}
      onDrop={onDrop}
    >
      {children}
      {over && !disabled && (
        <div className="drop-overlay">
          <span className="drop-overlay-card"><Icon.Download size={20} /> {label}</span>
        </div>
      )}
    </div>
  );
}
