# Pimble UI Style Guide

Design language and implementation patterns for the Pimble desktop application, built with the Rinch framework (Mantine-inspired components, CSS variables, dark mode).

---

## Design Principles

1. **Quiet interface** — The content is the focus. Chrome should recede. Use low-contrast borders (`--rinch-color-dark-4`), dimmed text, and subtle hover states. Never use bright or saturated colors for structural elements.

2. **VS Code-inspired layout** — Three-zone layout: sidebar (explorer), editor (main content), status bar. Users of VS Code, Obsidian, and Notion should feel immediately at home.

3. **Information density over decoration** — Prefer compact spacing and small font sizes for structural UI (sidebar, status bar). Reserve generous spacing for content editing areas.

4. **Consistency through CSS classes** — All layout regions use `pimble-*` CSS classes defined in `APP_CSS`. Avoid inline styles for anything that might need to change together. Inline styles are acceptable only for one-off layout constraints like `flex: 1`.

---

## Color Palette

Pimble uses Rinch's dark theme with blue as the primary color. Do not hardcode hex colors — use CSS variables.

### Backgrounds (darkest → lightest)

| Token | Usage |
|---|---|
| `--rinch-color-dark-8` | Reserved (near-black) |
| `--rinch-color-dark-7` | Sidebar, status bar background |
| `--rinch-color-dark-6` | Toolbar backgrounds, elevated surfaces |
| `--rinch-color-dark-5` | Hover states, code blocks, subtle fills |
| `--rinch-color-dark-4` | Borders, dividers, separator lines |
| `--rinch-color-body` | Editor panel background |

### Text

| Token | Usage |
|---|---|
| `--rinch-color-text` | Primary text (editor content, selected tree items) |
| `--rinch-color-dimmed` | Secondary text (sidebar headings, icons, hints) |

### Accent (use sparingly)

| Token | Usage |
|---|---|
| `--rinch-primary-color-4` | Links, active icon highlights |
| `--rinch-primary-color-7` | Blockquote borders |
| `--rinch-primary-color-9` | Selected tree item background (very subtle) |

### Status colors

| Color | Meaning |
|---|---|
| `green` | Connected / success |
| `yellow` | Connecting / warning |
| `red` | Error / disconnected |

---

## Typography

### Editor content (`.editor-content`)

- **Font**: System font stack (`-apple-system, BlinkMacSystemFont, Segoe UI, Roboto, ...`)
- **Size**: 15px
- **Line height**: 1.7 (generous for readability)
- **Max width**: 720px (content column, not full-width)

### Sidebar

- **Heading**: 11px, uppercase, `font-weight: 600`, `letter-spacing: 0.05em`, `--rinch-color-dimmed`
- **Tree labels**: Inherit (default ~14px), normal weight
- **Icons**: 0.8rem–1rem, `--rinch-color-dimmed` (brightens on selection)

### Status bar

- **Font size**: 11px
- **Color**: `--rinch-color-dimmed`

---

## Layout Regions

### Sidebar (`.pimble-sidebar`)

- Fixed width: 260px, min 200px
- Background: `--rinch-color-dark-7`
- Right border: `1px solid var(--rinch-color-dark-4)`
- Flex column layout

**Header** (`.pimble-sidebar__header`):
- Horizontal flex, vertically centered
- Contains: heading text (uppercase store name) + action icon (new node `+`)
- Padding: `8px 12px 6px`

**Tree** (`.pimble-sidebar__tree`):
- `flex: 1`, `overflow-y: auto`
- Padding: `0 4px 8px`

### Editor (`.pimble-editor`)

- `flex: 1` of the main content area
- Background: `--rinch-color-body`
- Flex column: toolbar → content → (or empty state)

**Toolbar** (`.pimble-editor__toolbar-wrap`):
- Bottom border: `--rinch-color-dark-4`
- Hidden when no document selected

**Content** (`.pimble-editor__content-wrap`):
- Padding: `24px 32px` (generous margins around content)
- `overflow-y: auto`

**Empty state** (`.pimble-empty-state`):
- Centered vertically and horizontally (`flex + align-items/justify-content: center`)
- Large dimmed icon (48px, 15% opacity)
- Primary text at 14px, 50% opacity
- Hint text at 12px, 30% opacity

### Status bar (`.pimble-status-bar`)

- Full width, pinned to bottom
- Background: `--rinch-color-dark-7`
- Top border: `--rinch-color-dark-4`
- Height: auto (compact, ~24px)
- Contains: connection badge (dot variant), server address, spacer

---

## Component Usage

### Preferred Rinch components

| Component | When to use |
|---|---|
| `ActionIcon` | Small icon buttons (toolbar, sidebar header). Use `variant: "subtle"` and `size: "xs"` or `"sm"`. |
| `Badge` | Status indicators only (connection status in status bar). Use `variant: "dot"`, `size: "xs"`. |
| `Tree` | Hierarchical navigation. Always provide `render_node` for drag-and-drop support. |
| `Tooltip` | Hover hints for icon buttons. Use for actions without visible labels. |
| `Divider` | Visual separation between sections. Prefer CSS borders for structural dividers. |
| `Text` / `Title` | Standalone text when Rinch component features (size, color props) are needed. For simple text, use raw `span` / `div` elements. |

### Avoid

| Component | Why |
|---|---|
| `Badge` (filled, for headings) | Too visually heavy for structural labels. Use plain text with uppercase + letter-spacing. |
| `Card` / `Paper` | Adds unnecessary visual weight. The sidebar and editor are already visually distinct zones. |
| `Alert` | Use `tracing` for backend messages. For user-facing errors, use the status bar or inline text. |
| `Modal` | Prefer file dialogs (via `rinch::dialogs`) and inline interactions (rename, etc.). |

---

## Interaction Patterns

### Tree selection

- Single click: select node, load content in editor
- Double click: enter inline rename mode
- Drag-and-drop: rearrange nodes within a store
- Selection style: `--rinch-color-dark-5` background (subtle, not bright)
- Hover style: `--rinch-color-dark-5` background

### Inline rename

- Input replaces label text in-place
- Enter commits, Escape cancels
- Uses keyboard interceptor to catch Escape without propagation
- Defers signal updates via `run_on_main_thread` to avoid re-entrancy

### Editor

- ContentEditable div with `rinch-editor` backing
- Content loaded via `EditorDocument::from_bytes()` → `load_content()` (CE API) or `set_inner_html()` (fallback)
- Content saved on node switch and window close
- Toolbar visibility toggles reactively via `show_editor` signal

---

## CSS Architecture

### Class naming

All Pimble-specific classes use the `pimble-` prefix with BEM-style nesting:

```
.pimble-{region}                    → .pimble-sidebar
.pimble-{region}__{element}         → .pimble-sidebar__header
.pimble-{region}__{element}--{mod}  → .pimble-sidebar__header--collapsed
```

Rinch component overrides use the `rinch-` prefix:

```
.rinch-tree__node-content--selected
.editor-toolbar
```

### Where styles live

| Location | Purpose |
|---|---|
| `APP_CSS` const | Layout regions, sidebar, status bar, tree overrides, empty state |
| `EDITOR_CSS` const | Editor content formatting (headings, code, lists, etc.) |
| Inline `style:` in RSX | One-off layout (`flex: 1`, `display: flex`, `height: 100%`) |
| Rinch theme (`ThemeProviderProps`) | Primary color, dark mode, default radius |

### Adding new styles

1. Add a `pimble-*` class to `APP_CSS` for layout / structural styles
2. Add to `EDITOR_CSS` for content formatting inside the editor
3. Never use inline styles for colors, fonts, or spacing that should be consistent
4. Always use CSS variables, never hardcoded hex (except for syntax highlighting colors like `#e06c75`)

---

## Adding New Features — Checklist

When adding a new UI feature:

- [ ] Does it follow the three-zone layout? (sidebar, editor, status bar)
- [ ] Are colors from CSS variables, not hardcoded?
- [ ] Are structural styles in `APP_CSS` with `pimble-*` classes?
- [ ] Is the feature hidden/shown reactively via a `Signal<bool>` + `Effect`?
- [ ] Does text in RSX use `{|| expr}` closures for reactive updates?
- [ ] Is the empty/loading state handled? (what does the user see before data loads?)
- [ ] Are interactive elements accessible via keyboard? (Enter, Escape, Tab)
- [ ] Is the feature compact enough for the sidebar (11-12px) or appropriately sized for the editor (15px)?
