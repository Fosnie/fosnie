# Markdown style + gotchas (DOCX / PDF)

Shared reference for the document skills. The renderer is pandoc (DOCX) or a
Markdown→HTML→WeasyPrint path (PDF); both honour standard Markdown. These are the
things that go wrong.

## Headings

- Exactly **one** `#` (the title). Everything else is `##` / `###`.
- Do not skip levels (`#` then `###`) — the Word/PDF outline relies on the order.
- No trailing `#` (`## Heading ##` renders the trailing hashes literally in some
  paths).

## Lists

- Blank line **before** a list, or the first item is swallowed into the paragraph.
- Nest with **two spaces** per level, not a tab.
- A numbered list restarts at `1.` — write `1.` for every item and let the renderer
  number them, or write the real numbers; do not mix.

## Tables

- Pipe tables need a header row and a `---` separator row:

  ```
  | Party        | Obligation              | Due        |
  | ------------ | ----------------------- | ---------- |
  | Discloser    | Mark documents          | On supply  |
  | Recipient    | Restrict to the purpose | Throughout |
  ```

- Escape a literal pipe inside a cell as `\|`.
- Alignment is set in the separator row: `:---` left, `:--:` centre, `---:` right.
- Keep cells to a few words; long prose belongs in paragraphs, not cells.
- A table cannot contain block elements (no lists or headings inside a cell).

## Emphasis + punctuation

- `**bold**` and `*italic*`; do not use underscores for emphasis inside words
  (`file_name_here` should not go italic — escape as `file\_name\_here` or use
  backticks).
- Use a real em dash `—`, not `--`.
- Use curly quotes in prose where the house style expects them; straight quotes are
  fine and render unchanged.

## Things that do NOT work

- Raw HTML — strip it; it does not survive cleanly into DOCX.
- ASCII art, hand-drawn rules (`====`), or manual page breaks — the styles and the
  page engine handle layout.
- Footnote syntax beyond the renderer's support — prefer inline citations or a
  "References" section.
