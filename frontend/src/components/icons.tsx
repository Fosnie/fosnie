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

// Semantic icon map → lucide-react glyphs (mirrors the prototype's icons.jsx MAP).
// Use `<Icon.Agents className="h-[18px] w-[18px]" />`. Default stroke 1.75 per the
// brand; pass `strokeWidth`/`size`/`className` to override.

import {
  Activity, ArrowUp, ArrowUpRight, AtSign, Bell, Blocks, BookText, Brain,
  Calculator, CalendarDays, Check, ChevronDown, ChevronLeft, ChevronRight,
  CircleCheck, Clock, Copy, Database, Dot, Download, Ellipsis, FileStack,
  FileText, Files, Flag, Folder, Globe, Info, KeyRound, Layers, LayoutTemplate, ListFilter, Lock, LogOut,
  Menu, MessageSquare, Mic, NotebookPen, Paperclip, Pause, Pencil, Pin, Play, Plus, Quote,
  RotateCcw, Save, Scale, ScrollText, Search, SendHorizontal, ShieldCheck,
  Smile, SlidersHorizontal, Sparkles, Square, SquareTerminal, Table2, Telescope,
  ThumbsDown, ThumbsUp, TriangleAlert, Trash2, User, Users, Waypoints, Workflow,
  Wrench, X, Zap,
  type LucideProps,
} from "lucide-react";
import {
  Agents as AgentsGlyph, Skills as SkillsGlyph, ThinkingEffort, LiveVoice,
  Workspace as WorkspaceGlyph,
} from "./custom-icons";
import type { ReactElement } from "react";

const G = {
  Agents: AgentsGlyph, Skills: SkillsGlyph, Automations: Workflow, Prompts: ScrollText, Memory: Brain,
  Team: Users, Admin: ShieldCheck, Plus,
  Search, Send: ArrowUp, Stop: Square, Attach: Paperclip, Chevron: ChevronDown,
  ChevronR: ChevronRight, ChevronL: ChevronLeft, Check, Close: X, Spark: Sparkles,
  General: WorkspaceGlyph, Legal: Scale, Research: Telescope, Doc: FileText, Docs: Files, Source: FileStack,
  Shield: ShieldCheck, Flag, Pin, Dots: Ellipsis, Copy, Refresh: RotateCcw, Folder,
  Chat: MessageSquare, Quote, Scale, Lock, Grid: Table2, Download, Calendar: CalendarDays,
  Play, Pause, Edit: Pencil, Trash: Trash2, Code: SquareTerminal, External: ArrowUpRight,
  Filter: ListFilter, Clock, Sliders: SlidersHorizontal, Book: BookText, Check2: CircleCheck,
  Alert: TriangleAlert, Info, Dot, Logout: LogOut, Layers, Key: KeyRound, Activity, Database,
  Like: ThumbsUp, Dislike: ThumbsDown, Wrench, Blocks, At: AtSign, Bell, Note: NotebookPen,
  Globe, Calc: Calculator, Lightning: Zap, Tune: ThinkingEffort, User, Send2: SendHorizontal,
  Save, Smile, Mic, LiveVoice, Workflows: Waypoints, Menu, Template: LayoutTemplate,
} as const;

// Wrap each glyph so it carries the brand's 1.75 default stroke (overridable).
type IconName = keyof typeof G;
function make(C: (typeof G)[IconName]) {
  return (p: LucideProps) => <C strokeWidth={1.75} {...p} />;
}

export const Icon = Object.fromEntries(
  (Object.keys(G) as IconName[]).map((k) => [k, make(G[k])]),
) as Record<IconName, (p: LucideProps) => ReactElement>;
