# Themes contract: how the app looks, and how a tree reads

Status: proposed 2026-11-24, nothing built. Asked for by Seth; sketched by Joe the same day
(two kinds, any node can have one, themes live in the store and copy easily between open
stores, plugins themed, about a dozen built in, maybe replacing the paragraph setting).
Written against the code at `ed48772` and rinch `main` `e8e04ccc`. The decisions Joe must
take are numbered in section 12, each with a recommendation; nothing is built until he
has answered them.

**Two kinds, kept apart:**
- **App themes** dress the chrome: the explorer, the panes' frames, menus, dialogs, the
  toolbar, the status bar, plugins' viewers. They are **personal**: what one person picks
  changes nothing anyone else sees.
- **Document themes** decide how text reads: one tree like a book, another like code,
  another like plain Markdown. They are **shared**: a theme applied to a tree is part of
  that tree, and everyone who opens it sees it the same way.

## 1. What exists today

- **The scheme.** `app/theme.rs`: `theme_props(dark_mode)` gives rinch a primary color
  (`"blue"`), the scheme and a radius; `theme_css` appends Pimble's own variables
  (`styles::LIGHT_THEME_VARS` redefines rinch's semantic variables, `--rinch-color-body`,
  `-surface`, `-text`, `-dimmed`, `-border`, the primary ramp `--rinch-primary-color-0..9`,
  the title bar; `DARK_THEME_VARS` adds the toolbar's). The result is put in force with
  `rinch::core::set_current_theme_css`, the one sheet rinch replaces on a switch. The
  editor follows with `EditorHandle::set_dark_mode` (its `data-pm-theme` attribute).
  `state.json` `dark_mode` (`localStorage` in the browser) remembers the choice; Settings >
  General holds it and View > "Toggle Dark Mode" is its shortcut.
- **The app's stylesheets** (`crates/pimble-app/src/styles/*.css`, 4,600 lines) use rinch's
  semantic variables and Pimble's `--pimble-*` ones, never the dark palette directly
  (CLAUDE.md "Tree appearance"). An app theme can therefore restyle the chrome by
  redefining variables, which is exactly how the light scheme was made on 2026-10-20.
- **Editor content** is styled by `styles/editor_content.css` under the pane's wrapper,
  `.pimble-editor__content-wrap > [data-pm-editor] ...`: 15px, line height 1.7, heading
  colors, links, hashtags, mentions, text color fixes. The font is rinch's default.
- **Paragraph spacing**, the "collapse paragraphs" feature (`docs/PARAGRAPHS_CONTRACT.md`,
  built 2026-11-13). Each node says `custom["paragraphs"]` = `"spaced"` or `"compact"`
  (absent is compact). A new node **copies** its parent's value when it is made
  (`custom_keys::INHERITED`), so changing a folder later changes none of the notes in it,
  and a note never needs its ancestors to be drawn: a share's member may hold none of them.
  Edit > "Compact Paragraphs" and the Appearance picker set it; the importers mark what
  they make spaced. It is drawn by one class, `pimble-editor__content-wrap--spaced`, on the
  pane's wrapper (`pane_view/pane.rs:85`), and `editor_content.css` sets `p { margin: 0 }`
  unless that class is there. Only paragraphs change.
- **Row appearance** (icon, text color, background) lives in `metadata.custom` keys, and
  `pimble_ui::appearance::display_color` lifts a dark stored color on the dark scheme and
  caps a light one on the light scheme at render time; the stored value is never altered.
- **Plugins** draw with inline styles only (a plugin cannot add a stylesheet,
  `pimble_ui::viewer`), and their DOM is applied by the host into the app's own document
  (`docs/PLUGINS_CONTRACT.md` 3.2), so inherited custom properties reach them. The viewer
  look uses semantic variables for text and accents, but its surfaces
  (`viewer::surfaces(dark)`) name palette steps: `--rinch-color-dark-4..6` in the dark
  scheme, `--rinch-color-gray-1/3` and `white` in the light one. The ABI tells a plugin
  only `HostState.dark` (ABI 3).
- **Nothing** today lets anyone choose a font, a measure, a size or a color for text.

## 2. Format: tokens, never CSS

A theme is a **fixed, versioned set of typed tokens**. There is no field that holds CSS,
selectors or URLs. Each token has a type, and the app checks every value against it when
it reads a theme; a value that fails is ignored (the base's value stands in) and the theme
editor says which token and why.

Why tokens:
1. **Themes travel in shared stores.** A document theme is store content and reaches
   every member of a share. Free CSS from someone else is untrusted input to the renderer:
   it could hide text, overlay the chrome, fetch remote URLs (in the browser), or exhaust
   layout. Typed values cannot.
2. **Two renderers.** rinch's desktop engine and the browser (rinch-web, real DOM) must draw
   the same theme the same way. A token maps to properties both are known to support
   (section 7); arbitrary CSS would draw differently on each.
3. **Tokens outlive rinch's markup.** The editor's DOM and class names are rinch's and
   change. Tokens are Pimble's, and one stylesheet in Pimble maps them onto whatever rinch
   draws today.
4. **The MCP and the theme editor can read and write them** field by field, and two people
   editing different tokens of one theme merge.

Token types: `color` (`#rrggbb`), `font` (one of the font stacks in section 5, by name),
`length` (a number and a unit, `px` or `em`, within a range per token), `number` (line
height, a ratio, within a range), `enum` (a fixed set of words), `bool`.

How tokens reach the screen: the app turns a theme into a string of custom properties
(`--pd-font-body: ...; --pd-p-spacing: ...`) written onto the element it applies to (the
pane's wrapper for a document theme, `:root` through the theme sheet for an app theme). The
values are produced by Pimble from checked typed values, never copied from the document,
so nothing a theme holds can end a declaration or open a rule. One stylesheet
(`styles/document_theme.css`, new) consumes them with `var(--pd-..., <default>)`.

### 2.1 Document theme tokens

| Group | Tokens |
| --- | --- |
| Page | `measure` (max line length: `none` or 40–100 `em`, centered when set), `page_padding` (length), `background` (color, optional), `text` (color, optional), `dimmed` (color, optional), `accent` (color for links, hashtags, mentions; optional) |
| Body | `font_body`, `size` (12–24 px), `line_height` (1.0–2.2), `letter_spacing` (optional), `align` (`start`, `justify`) |
| Paragraph | `spacing` (`compact`, `spaced`, `indented`: a book's first-line indent and no gap, the first paragraph after a heading not indented), `indent` (length, for `indented`), `gap` (length, for `spaced`) |
| Headings 1–6 | per level: `font` (default: `font_heading`), `size` (ratio of body), `weight` (`normal`, `medium`, `bold`), `case` (`none`, `upper`), `tracking` (letter spacing), `align` (`start`, `center`), `space_before`, `space_after`, `color` (optional), `rule_below` (bool) |
| Quote | `style` (`bar`, `indent`, `italic`, `boxed`), `font` (optional), `color` (optional) |
| Code | `font_code`, `size` (ratio), `background` (color, optional), `wrap` (bool) for code blocks; inline code shares font and size |
| Lists | `marker_color` (optional), `item_spacing` (length) |
| Tables | `style` (`grid`, `rows`, `minimal`), `header` (`bold`, `shaded`), `font` (optional), `size` (ratio) |
| Rule | `style` (`line`, `ornament`: a centered short rule, `space`) |

Colors are optional on purpose: a theme that sets none draws in the app theme's colors,
so it reads in dark and light alike. A theme that sets colors must set them for both
schemes (decision 7).

### 2.2 App theme tokens

The app theme is the set of variables the chrome already reads, nothing new to style:
rinch's semantic colors (`body`, `surface`, `text`, `dimmed`, `border`, `default`,
`default-hover`, `filled`, `filled-hover`, `placeholder`, `option-hover`,
`option-selected`), the primary ramp (given as one color; Pimble derives the ten steps, as
the light scheme's ramp around `#3f6491` was derived), the title bar's two, Pimble's own
(`--pimble-toolbar-*`, `--pimble-sheet-ghost`, the viewer surfaces of section 8), the
radius (`xs`..`xl`), the UI font (a stack from section 5) and the UI size (13–16 px).
Each app theme names its **scheme** (dark or light): that still drives the editor's
`set_dark_mode`, `display_color` and every color Pimble derives, so the existing dark/light
logic stays the one place that judges legibility.

## 3. Where themes live

### 3.1 Document themes are nodes

A document theme is a **node of the store**, `node_type = "theme"`, its tokens a yrs map in
`data.theme` (each token one key, so concurrent edits of different tokens merge, and two
edits of one token resolve as any map key does). Being a node, it is co-edited by everyone
who may edit it, replicates, encrypts in a vault store, is undeletable from Recently
Deleted, and has a link (CLAUDE.md "Everything is co-editable": no copy kept in step, no
projection). Its title is the theme's name; its content is free text, and the theme pane
shows a live sample under the token editor (section 9).

A theme node may name a **base**: `data.theme.base` = a built-in's id (`builtin:book`) or
another theme node of the same store. Tokens it does not set come from the base, then from
the built-in Default. A base in another store is not allowed (it would make one store's
look depend on another being open).

Where theme nodes sit in the tree is the person's business; "New Theme" offers a
**Themes** folder at the store root, made on first use, so they do not clutter writing.

### 3.2 Applying a document theme

A node names a theme with one key, `custom["theme"]` (`pimble_core::custom_keys::THEME`):
a built-in id (`builtin:book`) or a theme node's id in the same store. It is an
`applyEdit` of that key like any other appearance key (`BackendCommand::SetNodeAppearance`
grows `theme`), so applying is shared: everyone sees the book layout.

**The theme in force** for a node is the one named by the node itself or its nearest
ancestor that names one; with none, Default. Looked up, not copied, which is the reverse
of what paragraphs chose (section 4 says why and decision 3 asks): "this tree reads like a
book" means changing the theme on the tree's root changes the whole tree, including notes
already in it.

Lookup has two edges the copy rule never had:

- **Shares.** A member's partial replica holds the share's root and below, not its
  ancestors, and may not hold the theme node either (it may sit in a Themes folder outside
  the share). Decision 4 picks how members see the theme. The recommendation: **a scope's
  reach includes, read-only, the theme documents in force in it**: the theme named by the
  share root's nearest themed ancestor, and every theme named inside the share, with their
  bases. The owner's share upkeep already publishes scope sets as a union that only grows
  (`share.rs`, `docs/MOVE_CONTRACT.md`); it would add these, and the reach rule (`Reach` in
  `handler.rs`) would grant read on them. The share root's own effective theme is then
  recorded on the share (the accounts service already stores a share's name) so a member
  resolves "the theme in force above my root" without holding the ancestors. A member
  who may edit the share cannot edit a theme outside it: they see it, read-only, and may
  apply another (their own, or a built-in) inside the share.
- **Mounts.** A node is addressed everywhere by its canonical `(StoreId, NodeId)`, so a node
  should read the same wherever it is opened. The recommendation (decision 5): **the theme
  in force is resolved in the source store's own tree**, and the mounting store's chain is
  ignored. A mounted subtree of code notes stays code-like inside a book. The mount node
  itself may name a theme (it is a node of the mounting store), and that applies to the
  mount row only, not to what it shows.

Resolution is a pure function of the chain's documents (a pane's node and its ancestors,
which the tree has loaded to show it; wave 1 checks that the API holds the chain for a node
opened by link with its ancestors collapsed, and asks for it if not), memoized
per `(store, node)` and invalidated by `NodeMoved`, a change to any `custom["theme"]` on
the chain, and a change to the theme document or its base.

### 3.3 App themes

An app theme is chosen **per device**, like `dark_mode` and zoom today: `state.json`
`app_theme` (and `localStorage` in the browser). It names a built-in (`builtin:pimble-dark`)
or a theme node (`pimble:` link) whose `data.theme.kind` is `app`. So a person's own app
themes are made, edited and copied the same way document themes are (one model, one
editor), while choosing one is personal and edits nobody's document (decision 6). If the
named node's store is not open, the device falls back to the built-in of the same scheme
and says so once in Settings. View > "Toggle Dark Mode" becomes "switch to the other
scheme's theme": the last-used theme of the other scheme.

Following the account (the personal document) is possible later; the per-device choice
matches what Joe decided for places and views (window sizes and screens differ, and so do
displays), so it is the recommendation.

### 3.4 Built-ins

Built-ins ship with the app (`crates/pimble-themes`, a small crate of data: token tables
and their ids, both targets) and are referenced by id; they are never copied into a store
to be used. A store theme may derive from one (`base`). Built-ins are versioned with the
app: a later release may tune `builtin:book`, and every tree that uses it follows. A
person who wants it frozen makes a theme with that base and sets every token.

## 4. Paragraph spacing: subsumed

A document theme subsumes the paragraph setting. `spacing` (`compact`, `spaced`,
`indented`) is a token, so a book reads with indents, a report with gaps and a code tree
compact, and the theme says so for the whole tree.

Keeping both would be two settings for one margin, and the copy rule makes them collide:
every node under a folder made spaced before themes carries a copied `"spaced"`, so a book
theme applied to that folder would be overridden everywhere if the per-node key won.

The recommendation (decision 2):
- **No new writes of `custom["paragraphs"]`.** Edit > "Compact Paragraphs" and the
  Appearance picker's Paragraphs row are replaced by the Theme item (section 9), and
  `custom_keys::INHERITED` drops it.
- **Read-time fallback, no migration writes.** When no node on the chain names a theme,
  a node whose own `paragraphs` is `"spaced"` draws with `builtin:document` (Default with
  `spacing = spaced`); otherwise Default. Every existing note looks exactly as it does today,
  nothing is rewritten in anyone's store (a migration that edits every node of every store,
  read-only shares included, is the kind of write CLAUDE.md warns about), and the first
  theme applied anywhere above a note replaces the fallback.
- **Importers** set `custom["theme"] = "builtin:document"` on the import's top node instead
  of `paragraphs` on every node.
- The per-note distinction Joe made on 2026-11-13 ("most notes are quick notes") survives:
  a quick-notes tree and a writing tree each get their theme, and a single note can still
  name its own.

## 5. Fonts

A theme names fonts by **stack**, never by file or URL. The stacks are a fixed list that
the app guarantees on both targets by bundling the faces under open licenses (Pimble ships
no font today: rinch ships none, and the wordmark's Manrope is drawn as outlines in
`brand.rs`):

| Stack | Bundled face (proposal) | Fallbacks |
| --- | --- | --- |
| `sans` | Inter | system-ui, sans-serif |
| `serif` | Source Serif 4 | Georgia, serif |
| `book` | EB Garamond | serif |
| `slab` | Roboto Slab | serif |
| `mono` | JetBrains Mono | ui-monospace, monospace |
| `humanist` | Manrope | sans-serif |
| `system` | none | the platform's UI font |

Bundling is what makes "everyone sees the book layout" true: a font named by the person's
own system would draw differently on every device. Each face adds roughly 200–700 KB per
weight to the desktop binary and the web bundle (the web can load faces lazily, on first
use). Decision 8 asks whether a person may also name an installed system font for a theme
only they use.

## 6. Copying themes between stores

Joe asked that themes be easy to copy from any other open store, "seamlessly in the
background". What can and cannot be seamless:

- **Copying is explicit and one time.** Picking a theme from another open store in the
  Theme picker copies it into this store (a new theme node in this store's Themes folder,
  its tokens and its title, with `data.theme.copied_from = { store, node, at }` for
  provenance), then applies the copy. One click; no "import" step to think about. A base
  that is a built-in carries over by id; a base that is another theme node of the source
  store is flattened into the copy's own tokens.
- **What cannot be seamless: keeping the copy in step.** A copy that follows its original
  in the background is a mirror: one store's content rewritten by a device interpreting
  another store's edits, which is exactly what CLAUDE.md forbids ("Everything is
  co-editable": no mirrors, no copies someone keeps in step). It would also leak: edits in a
  private store would flow into a shared one without anyone choosing it.
- **What can feel seamless instead:**
  1. The picker lists every open store's themes beside this store's and the built-ins, so
     a theme from elsewhere is one click away, and applying it copies on first use.
  2. Copying the same original twice does not make a second copy: a theme in this store
     whose `copied_from` names the same original is reused.
  3. A copy shows "Copied from <theme> in <store>" and, when the original has changed
     since, an explicit **"Update from original"** action that copies its tokens again, as
     one edit the person chose (decision 9).
- **Referencing a theme in another store is not offered.** A cross-store reference would
  make one store's look depend on another being open, reachable and readable by every
  member of every share; mounts already show what that costs.

## 7. Drawing a document theme per pane, and rinch

What the app does (no rinch change needed for the core of it):
- `pane_view/pane.rs` already sets the wrapper's class per pane from the note
  (`pane_spaced`). It becomes `pane_theme(store, pane)`: the resolved theme's custom
  properties written as the wrapper's inline `style`, plus a class per enum token that
  changes structure rather than a value (`--para-indented`, `--quote-boxed`,
  `--table-rows`, `--rule-ornament`).
- `styles/document_theme.css` maps every token onto the editor's elements under
  `.pimble-editor__content-wrap > [data-pm-editor]`, with `var(--pd-..., <today's value>)`
  so Default is today's look to the pixel. `editor_content.css`'s spacing rules move there.
- Inline custom properties inherited by descendants already work on both targets: the
  plugin viewers rely on them (`pimble_ui::viewer::surfaces`, set on a viewer's root).

What rinch gives today (read at `e8e04ccc`, paths under `crates/`):
- **Desktop** cascades with Stylo: inline `style` custom properties are parsed and
  inherited by descendants (`rinch-dom/src/stylo_impl.rs:659`,
  `computed_style/from_stylo/mod.rs:320`), and `var(..., fallback)` in a stylesheet rule
  resolves them. The browser is real DOM, so all of CSS applies there.
- **Supported on the desktop:** `font-family`, `font-size`, `line-height`,
  `letter-spacing`, `word-spacing`, `text-transform`, `text-align: justify`, `text-indent`,
  `max-width`, `:first-child` and `p + p` (Stylo's selectors). That covers every token in
  section 2.1.
- **Not supported on the desktop:** `font-variant: small-caps`, `hyphens: auto` (no
  dictionary), multi-column layout, `::first-letter`. So no token offers small caps, drop
  caps or columns, and `justify` comes without hyphenation (decent at a book's measure,
  ragged gaps at narrow widths; the Book theme justifies only when `measure` is set).
  `text-indent` and `hyphens` go through a custom-property rewrite in rinch
  (`from_stylo/mod.rs:32-44`, `:239`, rinch #2870), so `var()` inside `text-indent` needs a
  test on both targets in wave 1.
- **Fonts:** the desktop takes bundled faces through `App::fonts(&[AppFont])` and
  `register_font_data` (`rinch/src/app_builder.rs:322`, `rinch/src/font.rs:260`), live after
  mount; there is no `@font-face` on the desktop. The browser ignores `App::fonts` and uses
  `@font-face`. So each stack in section 5 is registered once per target at start, by the
  same family name, and the tokens name only family names.
- **The theme sheet:** `ThemeProviderProps` has six fields (a built-in palette name for the
  primary, radius, `font_family`, `font_family_monospace`, scheme, primary shade); custom
  colors are only possible by overriding `--rinch-*` variables in the sheet, which is what
  `theme_css` already does. App themes keep doing that, and use `font_family` for the UI
  font.
- **Per editor:** `Editor {}` takes no class or style, but none is needed: Pimble's own
  wrapper already carries per-pane state, and rules under it outrank the editor's
  `[data-pm-editor]` defaults.

So the core needs **no rinch change**. Two general improvements are worth proposing
upstream as PRs, neither naming a Pimble concept: `font-variant-caps: small-caps`
(synthesized when the face has no `smcp`) in the desktop renderer, which would let
headings use small caps; and `hyphens: auto` with a dictionary, which would make justified
text good at any width. Until either lands, no token depends on it.

## 8. Plugins

- **App themes reach plugins without an ABI change**, once one thing is fixed in Pimble:
  `pimble_ui::viewer::surfaces(dark)` names palette steps (`--rinch-color-dark-6`,
  `--rinch-color-gray-1`, ...), which an app theme does not redefine, so a themed app would
  show stock-gray kanban columns. The fix is new semantic variables the app theme sets
  (`--pimble-panel`, `--pimble-panel-border`, `--pimble-item`, `--pimble-item-border`,
  defaulting to today's values in the two stock themes) and `surfaces` reading them.
  That is a `pimble-ui` change; plugins pick it up when rebuilt, and the library updates
  them on its own (`docs/SETTINGS_CONTRACT.md` decision 3). `HostState.dark` stays: it
  tells a plugin the scheme, which the app theme still has.
- **Document themes reach a plugin's host slots for free**: an `editor:<link>` slot is the
  host's own Editor in a host wrapper, themed as a pane is.
- **Plugins that draw text themselves** (a kanban card's title, a todo row) could read the
  document theme's fonts and colors too. Two ways (decision 10):
  1. **No ABI change** (recommended): the host writes the plugin's node's resolved
     document-theme properties on the plugin mount's root, and `pimble_ui::viewer` uses
     `var(--pd-font-body, inherit)` and friends where it draws node text. Plugins that want
     it use the variables; plugins that do not are unaffected.
  2. **ABI 7**: `HostState` gains `document_theme: Option<ThemeTokens>` (the resolved tokens
     of the plugin's node, typed, from `pimble-themes`), for a plugin that computes with
     them (a timeline sizing its rows to the body size). Only worth it when a plugin needs
     the values in code, not just in styles.
- A plugin may not define or apply themes through the ABI beyond what `Pimble` already lets
  it write (a plugin with write access could set `custom["theme"]` like any key, judged as
  any edit is).

## 9. The interface

- **Format > Theme...** (both targets; Edit > "Compact Paragraphs" leaves): the picker for
  the focused pane's node. Three groups: This store's themes, Built-in, and From other open
  stores (section 6). Each row draws a small sample in that theme. A line says what is in
  force and where it comes from ("Book, from 'Novel'", "Book, set here"), and "Use the
  parent's" clears the key. The store row's menu and "Appearance..." get the same item.
- **A theme node opens as the theme editor**: the tokens in groups (section 2.1), each with
  a control fitting its type, "inherits from <base>" shown greyed until set, a sample text
  under it drawn live, and a scheme switch to preview dark and light. Edits are `applyEdit`s
  of `data.theme`, so two people editing a theme see each other's changes as they land.
- **Settings > General > Theme**: the app theme, from built-ins and the app themes in open
  stores, with "New App Theme..." (makes a theme node of kind `app` in a store the person
  picks, based on the current one).
- **Read-only**: a node this device may only read shows the picker disabled with
  `StoreAccess::READ_ONLY_REFUSAL`'s sentence, as every other edit does.

## 10. Web, MCP, export

- **Web parity.** One code base: the resolution, the CSS mapping, the picker and the editor
  are all in `pimble-app` and `pimble-themes`, both targets. The browser loads the bundled
  font faces lazily as `@font-face` from `/app/fonts/` on first use. A vault store's theme
  nodes are documents like any other; the vault client decrypts and serves them.
- **MCP** (`docs/MCP_CONTRACT.md` gains a section): `list_themes` (built-ins, and each open
  store's themes with links and kinds), `get_theme` (a theme's tokens, its base, what it
  inherits, and where it is applied, by the nodes naming it), `set_theme` (apply a theme to
  a node, or clear it: one `custom["theme"]` edit; a theme from another store is copied as
  the picker does), `create_theme` / `update_theme` (tokens by name, checked by type; a
  bad value refused with a sentence naming the token and its range). `get_node` adds one
  line: the theme in force and where from.
- **Export, later.** A document theme is the natural style sheet for exporting a tree:
  Markdown export ignores it (Markdown has no styling), HTML export writes the same CSS
  the panes use, and a future PDF/EPUB export of a tree (a manuscript to a book) maps the
  tokens onto print: `measure` and `page_padding` to page size and margins, `spacing =
  indented` to typeset paragraphs, heading tokens to chapter openings, `rule.ornament` to
  section breaks. Tokens for print only (page size, running heads, chapter breaks) would be
  a group added then, ignored on screen. Nothing here is built for export now; the token
  set is chosen so it will not need redesigning for it.

## 11. The starting dozen

Document themes (all set no colors unless said, so they read in both schemes):
1. **Default**: today's look exactly (15 px sans, line height 1.7, compact).
2. **Document**: Default with spaced paragraphs; what spaced notes fall back to (section 4).
3. **Book**: serif body (`book`), 18 px, line height 1.55, 36 em measure centered, indented
   paragraphs, centered upper-case tracked headings, ornament rules, justified.
4. **Manuscript**: `serif` 16 px, double-spaced, indented paragraphs, wide margins, plain
   headings: a submission draft.
5. **Code**: `mono` body 14 px, compact, headings bold mono, code blocks shaded and
   unwrapped, tables in a grid: notes that are mostly code.
6. **Markdown**: plain `mono` text the size of the body, headings the same size but bold
   with no extra space, quotes as a bar: reads like the source.
7. **Report**: `sans` 15 px, spaced, numbered-feeling headings with rules below, tables with
   shaded headers.
8. **Journal**: `serif` 17 px, generous line height, 34 em measure, spaced: diary entries.
9. **Compact**: `sans` 13 px, line height 1.4, tight lists: dense reference notes.

App themes:
10. **Pimble Dark**: today's dark scheme.
11. **Pimble Light**: today's light scheme (the brand slate).
12. **Paper**: light, warm off-white surfaces, sepia text, `serif` UI font: pairs with Book.
13. **Midnight**: dark, near-black surfaces, high contrast, cyan accent: late-night writing.
That is nine document themes and four app themes (thirteen); decision 11 asks which to
cut or add.

## 12. Decisions for Joe

1. **Tokens, not CSS.** A theme is a fixed set of typed tokens (section 2); there is no way
   to write CSS. *Recommendation: yes.* Free CSS in shared stores is untrusted input to
   both renderers, and tokens survive rinch's markup changing.
2. **Paragraph spacing.** Themes subsume `custom["paragraphs"]`: no new writes, a read-time
   fallback so every existing note looks the same, importers name `builtin:document`, and
   Edit > "Compact Paragraphs" is replaced by Format > Theme (section 4). *Recommendation:
   yes.* Alternatives: keep `paragraphs` as a per-note override of the theme (two settings
   for one margin, and the copied values would override every new theme), or keep both
   unrelated (Compact Paragraphs and a theme's spacing fighting).
3. **Looked up, not copied.** The theme in force is the nearest ancestor's (section 3.2),
   so changing the theme on a tree's root changes the whole tree. *Recommendation: yes*:
   that is what "this tree reads like a book" means. Paragraphs chose copying so a note
   never needs its ancestors; decisions 4 and 5 handle the two places that matters.
4. **Shares.** How a member sees a theme set above the share or kept outside it.
   *Recommendation: (a)* a scope's reach includes, read-only, the theme documents in force
   in it, and the share records its root's inherited theme; members see the book layout
   the owner sees. (b) Members see Default unless the share's root or something inside it
   names a theme they can read (simpler; the owner and members see different layouts).
   (c) Copy at creation like paragraphs (no lookup at all; then changing a tree's theme
   changes only new notes). (a) touches the reach rules, which are yours to decide.
5. **Mounts.** *Recommendation:* a node's theme is resolved in its own (source) store and
   the mounting chain is ignored, so a node reads the same wherever it is opened.
   Alternative: the source chain first, the mounting chain when the source names none (a
   mounted plain subtree takes on the book it is mounted into, but reads differently
   opened directly).
6. **App themes are theme nodes of kind `app`, chosen per device** (section 3.3).
   *Recommendation: yes*: one model and one editor for making and copying themes, while
   choosing one stays personal and per device like dark mode and zoom. Alternatives: app
   themes kept as device files only (no co-editing, no copying through stores), or the
   choice following the account through the personal document.
7. **Colors in document themes.** *Recommendation:* optional; a theme that sets any color
   sets a light and a dark value for each, and `display_color` is not applied to them (the
   author chose both). Alternative: one set of colors, with the app's lift-and-cap applied in
   the other scheme (less to author, less control: a sepia page would be adjusted on dark).
   Or: no colors in document themes at all (the app theme owns color; document themes own
   type and space only). That last is the simplest and the safest for legibility.
8. **Fonts.** *Recommendation:* the fixed stacks of section 5, faces bundled on both
   targets (a few MB in all, lazy in the browser). Also allow an installed system font by
   name for a theme only its author uses? *Recommendation: no*, since a shared theme would
   draw differently on every device.
9. **"Update from original"** on a copied theme (section 6): an explicit, one-time re-copy.
   *Recommendation: yes*, as a later wave. Never automatic.
10. **Plugins.** *Recommendation:* no ABI change: app themes reach plugins through new
    semantic surface variables in `pimble_ui::viewer`, and document-theme properties are
    set on a plugin mount's root for plugins that use them (section 8). ABI 7's
    `HostState.document_theme` only when a plugin needs the values in code.
11. **The starting set** (section 11): nine document themes, four app themes. Cut, add or
    rename?
12. **Where theme nodes go.** *Recommendation:* anywhere; "New Theme" makes a Themes folder
    at the store root on first use. Alternative: themes kept out of the explorer, listed
    only in the picker (would need a place in the store that is not the tree, which node
    documents do not have).

## 13. Waves (once the decisions are in)

1. **Tokens and drawing.** `pimble-themes` (tokens, types, checks, built-ins, CSS
   generation), `custom_keys::THEME`, resolution (chain, base), `document_theme.css`, the
   pane wrapper, bundled fonts, the paragraphs fallback, Format > Theme with built-ins only.
   Default and Document are pixel-identical to today (checked by screenshot on both
   targets).
2. **Theme nodes.** `node_type = "theme"`, the theme editor pane, New Theme, bases, the
   picker's store group, copying from other open stores.
3. **App themes.** The app token set, Settings > General, the viewer surface variables,
   the two stock themes as data, Paper and Midnight.
4. **Shares and mounts** per decisions 4 and 5, with the sharing harness
   (`crates/pimble-server/tests/common`) covering a member who holds neither the
   ancestors nor the theme node.
5. **MCP** tools, and "Update from original".

## 14. How it is checked

- Unit: every token's checks (ranges, enums, colors), CSS generation never emits anything
  but declarations of `--pd-*`/`--pimble-*` properties, resolution over chains with bases,
  moves and deletions of a theme node (a node naming a deleted theme falls back to the
  base chain above it, then Default, and says so in the picker).
- Convergence: two devices editing different tokens of one theme, and applying different
  themes to one node, converge (`three_replicas_converge` style).
- Screens: Default and Document identical to today; each built-in on desktop (GPU and
  `--cpu`) and in the browser side by side.
- Sharing: a member sees the owner's book layout with neither the ancestors nor the Themes
  folder in their replica (decision 4 (a)).
