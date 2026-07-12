---
name: Web frontend
description: Write clean, modern, accessible HTML/CSS/JS (and React) — components, pages, layouts, and the markup behind dashboards and informative pages. Use when building or reviewing frontend code/markup, structuring a layout, or improving accessibility, semantics, or responsive behaviour. For downloadable HTML artefacts, defer to the dashboard skill's zero-egress rules. Do NOT use for backend code, document/PDF authoring, or spreadsheets.
default: true
compatibility: both-profiles
license: proprietary
---

# Web frontend

Conventions for clean, accessible, modern frontend work.

## For downloadable HTML artefacts — read this first

When the output is an `html` **artefact** (a dashboard / informative page the user
downloads), the **dashboard** skill governs and its rules are absolute:

- **No external resources** — never `<script src>`/`<link href>` to a CDN, no web-font
  URLs, no remote images. The platform inlines vendored libraries at the
  `<!-- pai:echarts -->` / `<!-- pai:theme -->` markers; everything else is inline.
- **Plain CSS variables**, no Tailwind/build step — a self-contained single file that
  opens offline. (This is a deliberate platform decision for artefacts.)

The guidance below applies to frontend code in general (e.g. helping a user with a
component); the artefact rules above override it whenever the deliverable is a file.

## Semantics & accessibility (always)

- Use the **right element**: `<button>` for actions, `<a>` for navigation, `<nav>`,
  `<main>`, `<header>`, `<footer>`, `<section>` with headings; one `<h1>` per page,
  no skipped heading levels.
- **Label everything**: `<label for>`/`aria-label` on inputs and icon buttons; `alt`
  on meaningful images (`alt=""` on decorative ones).
- **Keyboard + focus**: everything operable by keyboard; visible focus styles; logical
  tab order; don't trap focus.
- **Contrast & motion**: meet WCAG AA contrast; honour `prefers-reduced-motion`.
- Prefer native controls over re-implemented ones; reach for ARIA only to fill gaps,
  and follow the ARIA authoring patterns when you do.

## Modern CSS

- Layout with **flexbox / grid**; space with `gap`; size with `rem`/`clamp()`, not
  fixed pixels everywhere.
- **Custom properties** (`--token`) for colour/space/type scales; theme via variables,
  not duplicated values.
- Mobile-first, responsive by default; container/media queries for breakpoints.
- Logical properties (`margin-inline`, `inset`) where i18n matters.

## React (when writing components)

- **Function components + hooks**; keep components small and single-purpose.
- Derive state; don't duplicate it. Lift state only as far as needed; memoise
  (`useMemo`/`useCallback`) only real hot paths, not by reflex.
- **Keys** are stable ids, never the array index for dynamic lists.
- Side effects in `useEffect` with correct deps; clean them up.
- Accessible by construction (semantic elements, labels) — not bolted on after.

## Discipline

- Progressive enhancement; the page should make sense without JS where feasible.
- No dead code, no inline styles when a class will do, no `!important` arms races.
- Validate inputs, escape user content, never `dangerouslySetInnerHTML` untrusted data.

## Why this matters

The platform's users are in regulated environments with real accessibility and
robustness obligations. Semantic, accessible, dependency-light frontend is correctness,
not polish — and for downloadable artefacts the zero-egress, single-file rule is a
security guarantee, not a style choice.
