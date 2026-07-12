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

// A tiny, dependency-free rehype plugin that highlights groundedness "flagged
// spans" INLINE in a rendered assistant answer: it walks the hast tree and wraps
// any occurrence of a flagged claim's text in <mark class="claim-contradicted |
// claim-unsupported">, reusing the existing groundedness colours. It never
// descends into code/pre, so code blocks are left untouched. Used by Chat.tsx via
// react-markdown's `rehypePlugins`. No unist/hast helper packages needed.

interface HastNode {
  type: string;
  tagName?: string;
  value?: string;
  properties?: Record<string, unknown>;
  children?: HastNode[];
}

export interface FlagSpan {
  text: string;
  label: string; // "contradicted" | "not_mentioned"
}

const SKIP = new Set(["code", "pre"]);

/** rehype plugin factory: `rehypePlugins={[[rehypeGroundedness, spans]]}`. */
export function rehypeGroundedness(spans: FlagSpan[]) {
  // Phrase → class, longest first so a longer phrase wins over a substring of it.
  const phrases = (spans ?? [])
    .map((s) => ({
      text: (s.text ?? "").trim(),
      cls: s.label === "contradicted" ? "claim-contradicted" : "claim-unsupported",
    }))
    .filter((p) => p.text.length >= 3)
    .sort((a, b) => b.text.length - a.text.length);

  return (tree: HastNode) => {
    if (!phrases.length) return;

    const splitText = (value: string): HastNode[] => {
      const lower = value.toLowerCase();
      let best: { idx: number; len: number; cls: string } | null = null;
      for (const p of phrases) {
        const idx = lower.indexOf(p.text.toLowerCase());
        if (idx >= 0 && (best === null || idx < best.idx || (idx === best.idx && p.text.length > best.len))) {
          best = { idx, len: p.text.length, cls: p.cls };
        }
      }
      if (!best) return [{ type: "text", value }];
      const before = value.slice(0, best.idx);
      const match = value.slice(best.idx, best.idx + best.len);
      const after = value.slice(best.idx + best.len);
      const mark: HastNode = {
        type: "element",
        tagName: "mark",
        properties: { className: [best.cls] },
        children: [{ type: "text", value: match }],
      };
      return [...(before ? [{ type: "text", value: before }] : []), mark, ...splitText(after)];
    };

    const walk = (node: HastNode) => {
      if (!node.children) return;
      const out: HastNode[] = [];
      for (const child of node.children) {
        if (child.type === "element") {
          if (!SKIP.has(child.tagName ?? "")) walk(child);
          out.push(child);
        } else if (child.type === "text" && typeof child.value === "string") {
          out.push(...splitText(child.value));
        } else {
          out.push(child);
        }
      }
      node.children = out;
    };

    walk(tree);
  };
}
