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

import { useState, type ReactNode } from "react";

const PY_KEYWORDS = new Set([
  "def", "return", "if", "elif", "else", "for", "while", "in", "not", "and", "or",
  "import", "from", "as", "class", "try", "except", "finally", "raise", "with",
  "lambda", "yield", "global", "nonlocal", "pass", "break", "continue", "True",
  "False", "None", "is", "del", "assert", "async", "await",
]);

/** Tiny dep-free Python tokeniser → coloured spans. Comments, strings, numbers,
 *  keywords; everything else plain. Good enough to read, no syntax lib. */
function highlightPython(code: string): ReactNode[] {
  const out: ReactNode[] = [];
  // Order matters: comments + strings first (they swallow keyword-like text).
  const re = /(#[^\n]*)|('(?:\\.|[^'\\])*'|"(?:\\.|[^"\\])*")|(\b\d[\d_.]*\b)|([A-Za-z_]\w*)/g;
  let last = 0;
  let m: RegExpExecArray | null;
  let k = 0;
  while ((m = re.exec(code))) {
    if (m.index > last) out.push(code.slice(last, m.index));
    const [tok, comment, str, num, word] = m;
    if (comment) out.push(<span key={k++} className="text-slate/60">{tok}</span>);
    else if (str) out.push(<span key={k++} className="text-gold-light">{tok}</span>);
    else if (num) out.push(<span key={k++} className="text-urgency-amber">{tok}</span>);
    else if (word && PY_KEYWORDS.has(word)) out.push(<span key={k++} className="text-gold">{tok}</span>);
    else out.push(tok);
    last = m.index + tok.length;
  }
  if (last < code.length) out.push(code.slice(last));
  return out;
}

function isPython(lang?: string): boolean {
  return lang === "python" || lang === "py";
}

export function CodeBlock({ code, lang }: { code: string; lang?: string }) {
  const [copied, setCopied] = useState(false);
  const body = isPython(lang) ? highlightPython(code) : code;

  function copy() {
    navigator.clipboard?.writeText(code).then(
      () => { setCopied(true); setTimeout(() => setCopied(false), 1200); },
      () => {},
    );
  }

  return (
    <div className="my-2 overflow-hidden rounded-lg border border-navy-lighter bg-navy">
      <div className="flex items-center justify-between border-b border-navy-lighter px-3 py-1 text-[0.65rem] text-slate/70">
        <span className="uppercase tracking-wide">{lang || "code"}</span>
        <button onClick={copy} className="hover:text-gold">{copied ? "copied ✓" : "copy"}</button>
      </div>
      <pre className="overflow-x-auto p-3 text-xs leading-relaxed text-slate-lightest"><code>{body}</code></pre>
    </div>
  );
}
