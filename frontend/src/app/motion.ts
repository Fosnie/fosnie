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

// Shared motion presets for the glass-era chrome (UI refresh Phase 2). One set of
// variants/transitions so every overlay (modals, menus, popovers, toasts, the
// citation rail) animates consistently. Reduced-motion is honoured globally by the
// <MotionConfig reducedMotion> wrapper in Shell — no per-call gating needed.

import type { Transition, Variants } from "motion/react";

// Quick, slightly springy — chrome entrances stay ≤~220ms (NN/g: motion on chrome
// must not get in the way).
export const spring: Transition = { type: "spring", stiffness: 520, damping: 34, mass: 0.7 };

export const scrimVariants: Variants = {
  initial: { opacity: 0 },
  animate: { opacity: 1 },
  exit: { opacity: 0 },
};

// Floating popovers/menus/modals: fade + a small rise and scale from the trigger.
export const popVariants: Variants = {
  initial: { opacity: 0, y: 6, scale: 0.98 },
  animate: { opacity: 1, y: 0, scale: 1 },
  exit: { opacity: 0, y: 4, scale: 0.98 },
};

// Right-hand rail (citation panel): slide in from the edge.
export const railRightVariants: Variants = {
  initial: { x: "100%" },
  animate: { x: 0 },
  exit: { x: "100%" },
};

// Toast stack items.
export const toastVariants: Variants = {
  initial: { opacity: 0, y: 8 },
  animate: { opacity: 1, y: 0 },
  exit: { opacity: 0, y: 8 },
};

// Panels that ease down into place from just above (Deep Research scope panels).
export const slideDownVariants: Variants = {
  initial: { opacity: 0, y: -10 },
  animate: { opacity: 1, y: 0 },
  exit: { opacity: 0, y: -8 },
};
