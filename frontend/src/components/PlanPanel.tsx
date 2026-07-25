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

// The current turn's plan, pinned above the stream so the user can see where the
// work has reached without scrolling back to the activity block. Collapsed it is
// one line ("Step 3 of 7: writing the migration"); expanded it is the whole
// checklist. It is a pure view over the plan the model registered with track_steps
// (the same steps the activity timeline shows), so it holds no state of its own
// beyond open/closed and vanishes the moment the turn has no live plan.

import { useState } from "react";

import { Icon } from "@/components/icons";
import { StepRow } from "@/components/agentActivity";

type Step = { title: string; status: string };

/** The one-line summary of where a plan has reached: how many steps are done, the
 *  step being worked on, and a human 1-based position for it. Pure, so the wording
 *  can be tested without rendering. */
export function planLine(steps: Step[]) {
  const done = steps.filter((s) => s.status === "done").length;
  const total = steps.length;
  // The step the work is on: the one running, else the first not yet done, else
  // the last (everything is done but the turn has not finished).
  const current =
    steps.find((s) => s.status === "running") ??
    steps.find((s) => s.status !== "done" && s.status !== "skipped") ??
    steps[total - 1];
  // A 1-based position, clamped so a finished-looking plan on a still-running turn
  // reads "Step 7 of 7" rather than "Step 8 of 7".
  const position = Math.min(done + 1, total);
  return { done, total, current, position };
}

export function PlanPanel({ steps }: { steps: Step[] }) {
  const [open, setOpen] = useState(false);

  if (steps.length === 0) return null;

  const { total, current, position } = planLine(steps);

  return (
    <div className="plan-pin">
      <button className="plan-pin-head" type="button" onClick={() => setOpen((v) => !v)}>
        <Icon.Automations size={13} />
        <span className="plan-pin-line">
          Step {position} of {total}
          {current ? <span className="plan-pin-current">: {current.title}</span> : null}
        </span>
        <Icon.Chevron size={13} className="plan-pin-chev" style={{ transform: open ? "rotate(180deg)" : "none" }} />
      </button>
      {open ? (
        <div className="aa-timeline plan-pin-body">
          {steps.map((s, i) => (
            <StepRow key={i} title={s.title} status={s.status} />
          ))}
        </div>
      ) : null}
    </div>
  );
}
