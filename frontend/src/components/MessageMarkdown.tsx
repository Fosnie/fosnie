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

// Memoised streaming-answer renderer, shared by Chat and Legal (optimisation
// audit L5b / re-audit R17). react-markdown re-parses its whole input on every
// render; inside a message list that meant every message re-parsed on every
// token. `memo` shallow-compares props, so only the message whose `answer`
// actually changed (the streaming one) re-parses — completed siblings skip.
// Groundedness spans only become truthy post-completion, so the rehype walk
// never runs mid-stream.

import { memo } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";

import { CodeBlock } from "@/components/code";
import { rehypeGroundedness } from "@/components/groundednessHighlight";

export const MD =
  "[&_p]:my-1 [&_ul]:my-1 [&_ul]:list-disc [&_ul]:pl-5 [&_ol]:my-1 [&_ol]:list-decimal [&_ol]:pl-5 [&_a]:text-gold [&_a]:underline [&_code]:rounded [&_code]:bg-navy-lighter [&_code]:px-1";

// react-markdown renderers: fenced blocks → CodeBlock (copy + highlight),
// inline code stays a styled <code>. `pre` is unwrapped so CodeBlock isn't nested.
export const MD_COMPONENTS = {
  pre: ({ children }: { children?: React.ReactNode }) => <>{children}</>,
  // Wrap GFM tables so a wide table scrolls horizontally inside the bubble instead
  // of overflowing — a bare <table> can't scroll without a block wrapper.
  table: ({ children }: { children?: React.ReactNode }) => (
    <div className="md-table-wrap"><table>{children}</table></div>
  ),
  code: ({ className, children }: { className?: string; children?: React.ReactNode }) => {
    const text = String(children ?? "");
    const lang = /language-(\w+)/.exec(className ?? "")?.[1];
    if (lang || text.includes("\n")) return <CodeBlock code={text.replace(/\n$/, "")} lang={lang} />;
    return <code className="rounded bg-navy-lighter px-1">{children}</code>;
  },
};

export type FlagSpan = { start: number; end: number; text: string; label: string };

export const MessageMarkdown = memo(function MessageMarkdown({
  answer,
  pending,
  className = "ai-text " + MD,
  groundednessOn = false,
  spans,
}: {
  answer: string;
  pending?: boolean;
  className?: string;
  groundednessOn?: boolean;
  spans?: FlagSpan[];
}) {
  return (
    <div className={className} aria-live={pending ? "polite" : undefined} aria-busy={!!pending}>
      <ReactMarkdown
        remarkPlugins={[remarkGfm]}
        rehypePlugins={groundednessOn && spans?.length ? [[rehypeGroundedness, spans]] : []}
        components={MD_COMPONENTS}
      >{answer}</ReactMarkdown>
      {pending && <span className="caret" />}
    </div>
  );
});
