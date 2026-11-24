# Text color: colored text in notes, legible on both themes (2026-11-21)

Joe, 2026-11-21: "We need a way of coloring text in the text nodes. Needs smart handling of
both light/dark mode."

Status: **built 2026-11-21**, not merged or released. Code: `crates/pimble-app/src/text_color.rs`
(palette, plugin, apply), `toolbar.rs` (`render_color_btn`), `menus.rs` (Edit > Text Color),
`styles/editor_content.css` (`EDITOR_CSS`, "Text color") and `styles/toolbar.css` (the swatches beside `.pimble-toolbar__label`).

## 1. Palette

"Default" plus nine colors. Each has a light-theme shade (what is stored, and what is shown on
the light theme; at least 4.5:1 on the white page) and a dark-theme shade (shown on the dark
theme; at least 6:1 on the dark editor, `#242424`). The numbers are checked by the test
`the_palette_reads_in_both_themes`.

| Name | Light (stored) | on white | Dark | on `#242424` |
| --- | --- | --- | --- | --- |
| Gray | `#6b7280` | 4.83 | `#a8adb7` | 6.89 |
| Red | `#c92a2a` | 5.46 | `#ff8787` | 6.70 |
| Orange | `#c2410c` | 5.18 | `#ffa94d` | 8.16 |
| Yellow | `#a16207` | 4.92 | `#ffd43b` | 10.89 |
| Green | `#2b7a3b` | 5.32 | `#69db7c` | 8.89 |
| Teal | `#0f766e` | 5.47 | `#38d9a9` | 8.62 |
| Blue | `#1971c2` | 5.02 | `#74c0fc` | 7.90 |
| Purple | `#7c3aed` | 5.70 | `#b197fc` | 6.43 |
| Pink | `#c2255c` | 5.66 | `#f783ac` | 6.50 |

Yellow's light shade is an amber: a true yellow does not read on white.

## 2. Storage

The existing rinch `text_color` mark, `color` = the palette entry's light hex, lowercase
`#rrggbb`. Markdown and the MCP write it as `<span style="color:#c92a2a">`, copied HTML
carries it, and another app shows a real color that reads on white paper. Nothing new enters
the document: no Pimble class, no `var()`. "Default" removes the mark. rinch's `text_color`
does not exclude itself, so coloring a range removes every color in it first, then adds the
new one (never two colors on one character).

At a caret (no selection) a color sets the stored marks, so the next typed text is colored;
"Default" at a caret inside colored text makes the next typed text plain.

Code blocks take no marks (rinch's schema), so the button is dimmed there and Edit > Text Color
says "Text in a code block has no color." Inline code may be colored (`code` excludes bold,
italic, underline, strike and link, not color).

## 3. Display: the smart part

`TextColorPlugin` (every pane's editor, after the hashtags plugin) gives every colored run an
`Inline` decoration. Its span sits inside the mark's span, so a class rule on it outweighs the
mark's inline color. Classes:

- `pimble-tc` on every colored run.
- A **palette color** (compared after normalizing `#rgb`, `#rrggbb`, `#rrggbbaa`, `rgb()`,
  `rgba()`, case): `pimble-tc-<name> pimble-tc-fix-light pimble-tc-fix-dark`, so it shows the
  current theme's shade.
- **Any other color** (pasted): its contrast against the light page (`#ffffff`) and the dark
  editor (`#242424`) is computed. Where it is below 3:1 it gets `pimble-tc-fix-light` and/or
  `pimble-tc-fix-dark` with `pimble-tc-<nearest>`: the palette entry nearest in hue, or
  `pimble-tc-neutral` when the color has hardly any hue (channel spread under 0.12: black,
  white, grays, near-black navy). Fixed, neutral is the theme's ordinary text color, so pasted
  black on the dark theme is ordinary text. Where it reads, it is shown as authored.
- A color that is not one of those spellings (or not one of the common names) is shown as
  authored.

The stylesheet applies a fix only in the theme it is flagged for. Color only: no padding,
weight or size, so caret, selection, typing and layout are untouched. The stored value is never
altered by showing it.

**A hashtag inside colored text takes the text's color** (`.pimble-hashtag.pimble-tc` is
`color: inherit`, and the palette rules outrank or follow it).

Incremental like the hashtags plugin: per top-level block cached runs, only the blocks an edit
changed are read again, untouched blocks share their `Rc`.

## 4. Interface

- Toolbar, Text group: a "Text Color" button (Tabler `text-color`) after Inline Code. A bar
  under the icon shows the color at the selection or caret, as the current theme shows it
  (none for Default). It opens a popover of ten swatches (Default and the palette): an "A" over
  a bar, each in the current theme's shade, tooltip naming it, the current one outlined. A
  swatch colors and closes; Escape or a click outside closes. The keyboard returns to the
  editor. Dimmed where the toolbar is (no document, read-only) and in a code block.
- Edit > Text Color: Default and the nine colors, the same list. There is no Format menu.
- Both run `text_color::apply`, which refuses with a notice when no note is open or it is
  read-only.

## Built (2026-11-21)

- Decided beyond the brief: a palette color is "fixed in both themes" (same classes as a
  pasted color that fails in both), which keeps the stylesheet to one rule per shade.
- Decided beyond the brief: the pasted-color threshold is 3:1; the dark editor background is
  `#242424` (`--rinch-color-body`, rinch `dark-7`), the light one rinch's white page.
- Not done: a custom color (anything outside the palette) from the picker.
