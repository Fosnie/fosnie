// Copyright 2026 Private AI Ltd (SC881079)
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.

// Bespoke PAI glyphs that replace generic lucide stand-ins. Same API surface as
// lucide-react (LucideProps), so they drop straight into the `G` map in icons.tsx:
//
//   import { Agents, Skills, ThinkingEffort, LiveVoice, Workspace } from "./custom-icons";
//   const G = { Agents, Skills, /* ... */ Tune: ThinkingEffort, Mic: LiveVoice, General: Workspace } as const;
//
// All inherit `currentColor` and default to the brand's 1.75 stroke (overridable).

import type { LucideProps } from "lucide-react";
import type { ReactElement } from "react";

type Props = LucideProps;

function Svg({ size = 24, strokeWidth = 1.75, children, ...rest }: Props & { children: ReactElement | ReactElement[] }) {
  return (
    <svg
      xmlns="http://www.w3.org/2000/svg"
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth={strokeWidth}
      strokeLinecap="round"
      strokeLinejoin="round"
      {...rest}
    >
      {children}
    </svg>
  );
}

// Autonomous unit — hexagon core with an intelligence spark.
export const Agents = (p: Props): ReactElement => (
  <Svg {...p}>
    <path d="M12 2.8 19.9 7.4 19.9 16.6 12 21.2 4.1 16.6 4.1 7.4Z" />
    <path d="M12 8.7c.42 1.85 1.23 2.66 3.08 3.08-1.85.42-2.66 1.23-3.08 3.08-.42-1.85-1.23-2.66-3.08-3.08 1.85-.42 2.66-1.23 3.08-3.08Z" />
  </Svg>
);

// Skill tree — a root branching into mastered nodes.
export const Skills = (p: Props): ReactElement => (
  <Svg {...p}>
    <circle cx="12" cy="18.4" r="2.2" />
    <circle cx="6.5" cy="5.6" r="2.2" />
    <circle cx="17.5" cy="5.6" r="2.2" />
    <path d="M12 16.2V11M12 11 7.6 7.4M12 11 16.4 7.4" />
  </Svg>
);

// Adjustable reasoning depth — gauge with needle.
export const ThinkingEffort = (p: Props): ReactElement => (
  <Svg {...p}>
    <path d="M3 15.6a9 9 0 0 1 18 0" />
    <path d="M12 15.6 17.6 8.8" />
    <path d="M3.6 12.9l2 .8M20.4 12.9l-2 .8M12 6.4v2.1" />
    <circle cx="12" cy="15.6" r="1.5" fill="currentColor" stroke="none" />
  </Svg>
);

// Real-time voice — live waveform.
export const LiveVoice = (p: Props): ReactElement => (
  <Svg {...p}>
    <path d="M5 9.5v5M8.5 6.5v11M12 4.5v15M15.5 7.5v9M19 10v4" />
  </Svg>
);

// Break-glass privileged access — a shield fractured through the middle.
export const BreakGlass = (p: Props): ReactElement => (
  <Svg {...p}>
    <path d="M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z" />
    <path d="M12.6 6 10.4 11 13.2 12.2 11 17.6" />
  </Svg>
);

// The working surface — panelled layout.
export const Workspace = (p: Props): ReactElement => (
  <Svg {...p}>
    <rect x="3" y="4.5" width="18" height="15" rx="2.5" />
    <path d="M3 9.2h18M9 9.2v10.3" />
    <path d="M12 12.6h6M12 15.6h4" />
  </Svg>
);
