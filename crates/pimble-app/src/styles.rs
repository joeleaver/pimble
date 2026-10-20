//! CSS constants for the Pimble application.

/// Application-wide layout styles.
pub(crate) const APP_CSS: &str = "
/* ── Title bar brand (brand.rs) ─────────────────────────────── */

.pimble-titlebar-brand {
    display: flex;
    align-items: center;
    flex-shrink: 0;
    /* 4px on top centres the lockup 2px lower in the 36px row, which puts the
       word's baseline on the menu labels' (measured on the release build). */
    padding: 4px 12px 0 10px;
    gap: 4px;
}

.pimble-titlebar-brand__mark {
    display: flex;
    align-items: center;
}

/* ── Sidebar ────────────────────────────────────────────────── */

.pimble-sidebar {
    flex-shrink: 0;
    display: flex;
    flex-direction: column;
    background: var(--rinch-color-body);
}

/* The explorer's right edge: a hairline with a wider strip to catch the drag. */
.pimble-sidebar__resizer {
    flex-shrink: 0;
    width: 5px;
    margin-right: -4px;
    z-index: 1;
    cursor: col-resize;
    border-left: 1px solid var(--rinch-color-border);
}

.pimble-sidebar__resizer:hover {
    border-left-color: var(--rinch-primary-color-filled);
}

.pimble-sidebar__header {
    display: flex;
    align-items: center;
    padding: 8px 12px 6px;
    gap: 8px;
    border-top: 1px solid var(--rinch-color-border);
}

.pimble-sidebar__heading {
    flex: 1;
    font-size: 11px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--rinch-color-dimmed);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
}

/* The tree and the search results each sit in a pane that takes the rest of
   the sidebar and may be shorter than what it holds (`min-height: 0`), so the
   pane's content is what scrolls. Without it the pane grew to its content
   (the tree was 1134px in a 707px sidebar on the family store with a few
   folders open): the tree ran off the window with nothing to scroll, and a
   revealed row below the fold could not be reached. */
.pimble-sidebar__pane {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
}

.pimble-sidebar__tree {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    padding: 0 4px 8px 8px;
}

/* ── Tree refinements ───────────────────────────────────────── */

.rinch-tree__chevron, .rinch-tree__spacer {
    width: 0.875rem;
    height: 0.875rem;
    margin-right: 2px;
}
.rinch-tree__chevron svg, .rinch-tree__icon svg {
    width: 0.8rem;
    height: 0.8rem;
}
.rinch-tree__icon {
    width: 1rem;
    height: 1rem;
    margin-right: 4px;
    color: var(--rinch-color-dimmed);
}
.rinch-tree__node-content {
    border-radius: var(--rinch-radius-sm);
    margin: 0;
    padding: 2px 4px;
}
.rinch-tree__node-content:hover {
    background-color: var(--rinch-color-option-hover);
}
.rinch-tree__node-content--selected {
    background-color: var(--rinch-color-option-hover);
    color: var(--rinch-color-text);
}
.rinch-tree__node-content--selected:hover {
    background-color: var(--rinch-color-option-selected);
}
.rinch-tree__node-content--selected .rinch-tree__icon {
    color: var(--rinch-primary-color-4);
}
.rinch-tree__node-content--selected .rinch-tree__chevron {
    color: var(--rinch-color-text);
}
/* A node's row color (`custom_keys::BACKGROUND`): a band the label draws
   across the whole row, indent included, under the chevron, icon and title.
   Held back on the selected row so the selection still shows through it. */
.rinch-tree__node-content {
    position: relative;
}
.rinch-tree__chevron, .rinch-tree__spacer, .rinch-tree__label > span {
    position: relative;
    z-index: 1;
}
.rinch-tree__label > .pimble-row-band {
    position: absolute;
    left: 0;
    top: 0;
    right: 0;
    bottom: 0;
    z-index: 0;
    border-radius: var(--rinch-radius-sm);
    pointer-events: none;
}
.rinch-tree__node-content--selected .pimble-row-band {
    opacity: 0.5;
}

/* ── Context menu ───────────────────────────────────────────── */

.rinch-context-menu__dropdown {
    background-color: var(--rinch-color-surface);
    border: 1px solid var(--rinch-color-border);
    border-radius: var(--rinch-radius-default);
    box-shadow: 0 4px 12px rgba(0, 0, 0, 0.25);
}

.rinch-dropdown-menu__item {
    color: var(--rinch-color-text);
    padding: 6px 12px;
    font-size: 13px;
}

.rinch-dropdown-menu__item:hover {
    background-color: var(--rinch-color-option-hover);
}

.rinch-dropdown-menu__divider {
    border-color: var(--rinch-color-border);
}

/* One dimmed line at the top of a context menu saying why the items below
   are greyed out (docs/NODE_DOCUMENT_CONTRACT.md section 5): a menu that
   disables items without a reason is what this prevents. */
.pimble-menu-note {
    padding: 6px 12px;
    max-width: 260px;
    font-size: 11px;
    line-height: 1.4;
    color: var(--rinch-color-dimmed);
    border-bottom: 1px solid var(--rinch-color-border);
}

/* The badge a shared node's row carries after its label
   (docs/NODE_DOCUMENT_CONTRACT.md section 5). */
.pimble-tree__share {
    display: inline-flex;
    align-items: center;
    margin-left: 6px;
    color: var(--rinch-color-dimmed);
    opacity: 0.75;
}
.pimble-tree__share svg {
    width: 0.75rem;
    height: 0.75rem;
}

/* What a store row says about a store someone shared with this account:
   who shared it and what it may do here. */
.pimble-tree__shared-by {
    margin-left: 6px;
    font-size: 10px;
    font-weight: 400;
    text-transform: none;
    letter-spacing: normal;
    opacity: 0.6;
    cursor: default;
}

/* Mount point icon gets a distinct color */
.rinch-tree__icon--mount {
    color: var(--rinch-primary-color-6) !important;
}
.rinch-tree__node-content--selected .rinch-tree__icon--mount {
    color: var(--rinch-primary-color-4) !important;
}

/* ── Appearance picker ──────────────────────────────────────── */
.pimble-appearance__label {
    font-size: 12px;
    opacity: 0.7;
    margin: 10px 0 6px 0;
}
.pimble-appearance__row {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
}
.pimble-swatch {
    width: 26px;
    height: 26px;
    border-radius: 6px;
    border: 2px solid transparent;
    cursor: pointer;
}
.pimble-swatch--active {
    border-color: var(--rinch-color-text);
}
.pimble-swatch--none {
    background: transparent;
    border: 2px dashed var(--rinch-color-placeholder);
    color: var(--rinch-color-dimmed);
    font-size: 11px;
    display: flex;
    align-items: center;
    justify-content: center;
    width: auto;
    padding: 0 8px;
}
.pimble-icon-choice {
    width: 32px;
    height: 32px;
    border-radius: 6px;
    border: 2px solid transparent;
    background: var(--rinch-color-surface);
    display: flex;
    align-items: center;
    justify-content: center;
    cursor: pointer;
}
.pimble-icon-choice--active {
    border-color: var(--rinch-primary-color-5);
}
.pimble-icon-choice svg {
    width: 18px;
    height: 18px;
}

/* ── Share modal ────────────────────────────────────────────── */

.pimble-share__note {
    font-size: 12px;
    color: var(--rinch-color-dimmed);
    line-height: 1.5;
}

/* One of the two ways to share from a store that is not hosted: its button,
   then its sentence. */
.pimble-share__way {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 4px;
}

.pimble-share__members {
    display: flex;
    flex-direction: column;
    border: 1px solid var(--rinch-color-border);
    border-radius: var(--rinch-radius-sm);
}

.pimble-share__member {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 6px 8px;
    border-bottom: 1px solid var(--rinch-color-border);
}
.pimble-share__member:last-child {
    border-bottom: none;
}

.pimble-share__member-email {
    flex: 1;
    min-width: 0;
    font-size: 13px;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
}

.pimble-share__member-status {
    font-size: 11px;
    color: var(--rinch-color-dimmed);
    white-space: nowrap;
}

.pimble-share__invite {
    display: flex;
    align-items: flex-end;
    gap: 8px;
}

/* ── \"Recently Deleted...\" modal ──────────────────────────────── */

.pimble-deleted__list {
    display: flex;
    flex-direction: column;
    border: 1px solid var(--rinch-color-border);
    border-radius: var(--rinch-radius-sm);
    max-height: 320px;
    overflow-y: auto;
}

.pimble-deleted__row {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 6px 8px;
    border-bottom: 1px solid var(--rinch-color-border);
}
.pimble-deleted__row:last-child {
    border-bottom: none;
}

/* ── Editor panel ───────────────────────────────────────────── */

.pimble-editor {
    flex: 1;
    display: flex;
    flex-direction: column;
    overflow: hidden;
    background: var(--rinch-color-body);
}

/* ── The split view (docs/SPLIT_VIEW_CONTRACT.md) ───────────── */

/* The toolbar above the tiled area, then the tiled area. */
.pimble-panes-area {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
}

/* The tiled area: every pane and divider slot is positioned inside it. */
.pimble-panes {
    flex: 1;
    position: relative;
    overflow: hidden;
}

.pimble-pane {
    position: absolute;
    box-sizing: border-box;
    display: flex;
    flex-direction: column;
    overflow: hidden;
    background: var(--rinch-color-body);
}

.pimble-pane__title {
    flex-shrink: 0;
    display: flex;
    align-items: center;
    gap: 6px;
    height: 28px;
    padding: 0 6px 0 12px;
    font-size: 12px;
    color: var(--rinch-color-dimmed);
    border-top: 2px solid var(--rinch-color-border);
    user-select: none;
}

/* The focused pane's strip carries the accent color. */
.pimble-pane--focused > .pimble-pane__title {
    color: var(--rinch-color-text);
    border-top-color: var(--rinch-primary-color);
}

.pimble-pane__icon {
    flex-shrink: 0;
    display: inline-flex;
    align-items: center;
}

.pimble-pane__icon-glyph {
    display: inline-flex;
    align-items: center;
}

.pimble-pane__icon svg {
    width: 14px;
    height: 14px;
}

.pimble-pane__name {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
}

/* A split's whole space, which lets every press through but the ones on
   its line (see `pane_view::render_divider`). */
.pimble-pane-split {
    position: absolute;
    z-index: 5;
    pointer-events: none;
}

/* The line between two panes: a strip wide enough to catch the drag. */
.pimble-pane-divider {
    position: absolute;
    pointer-events: auto;
}

.pimble-pane-divider--right {
    top: 0;
    bottom: 0;
    width: 7px;
    margin-left: -3px;
    cursor: col-resize;
}

.pimble-pane-divider--down {
    left: 0;
    right: 0;
    height: 7px;
    margin-top: -3px;
    cursor: row-resize;
}

.pimble-pane-divider:hover {
    background: var(--rinch-primary-color);
    opacity: 0.5;
}

.pimble-editor__toolbar-wrap {
    flex-shrink: 0;
    border-bottom: 1px solid var(--rinch-color-border);
}

/* ── Boards (docs/IMAGE_MARKUP_CONTRACT.md) ─────────────────── */

/* A board pane: the surface fills it; the selection outline and the markup
   control sit over it and let the pointer through to it except where the
   control is. */
.pimble-board {
    flex: 1;
    display: flex;
    flex-direction: row;
    position: relative;
    z-index: 0;
    overflow: hidden;
    min-height: 0;
}

.pimble-board__canvas {
    flex: 1;
    position: relative;
    overflow: hidden;
    min-width: 0;
    display: flex;
}

.pimble-board__outline {
    position: absolute;
    border: 1px solid var(--rinch-primary-color-5);
    pointer-events: none;
}

.pimble-board__selection {
    position: absolute;
    border: 1px dashed var(--rinch-primary-color-5);
    pointer-events: none;
}

.pimble-board__handle {
    position: absolute;
    width: 9px;
    height: 9px;
    margin-left: -5px;
    margin-top: -5px;
    background: var(--rinch-color-body);
    border: 1px solid var(--rinch-primary-color-5);
    pointer-events: none;
}

.pimble-board__handle--nw { left: 0; top: 0; }
.pimble-board__handle--n { left: 50%; top: 0; }
.pimble-board__handle--ne { left: 100%; top: 0; }
.pimble-board__handle--e { left: 100%; top: 50%; }
.pimble-board__handle--se { left: 100%; top: 100%; }
.pimble-board__handle--s { left: 50%; top: 100%; }
.pimble-board__handle--sw { left: 0; top: 100%; }
.pimble-board__handle--w { left: 0; top: 50%; }
.pimble-board__handle--rotate { left: 50%; top: -22px; border-radius: 50%; }

.pimble-board__marquee {
    position: absolute;
    border: 1px solid var(--rinch-primary-color-5);
    background: rgba(34, 139, 230, 0.08);
    pointer-events: none;
}

.pimble-board__text-input {
    position: absolute;
    z-index: 2;
    margin: 0;
    padding: 0 2px;
    border: 1px dashed var(--rinch-primary-color-5);
    outline: none;
    resize: none;
    overflow: hidden;
    line-height: 1.25;
    font-family: sans-serif;
    white-space: pre-wrap;
}

.pimble-board__tip {
    position: absolute;
    z-index: 3;
    padding: 3px 8px;
    border-radius: 4px;
    font-size: 12px;
    white-space: nowrap;
    pointer-events: none;
    color: var(--rinch-color-text);
    background: var(--rinch-color-body);
    border: 1px solid var(--rinch-color-border);
}

.pimble-board__markup {
    position: absolute;
    top: 8px;
    right: 8px;
    display: flex;
    align-items: center;
    gap: 4px;
    padding: 2px 8px;
    border-radius: 12px;
    font-size: 12px;
    cursor: pointer;
    color: var(--rinch-color-text);
    background: var(--rinch-color-body);
    border: 1px solid var(--rinch-color-border);
    opacity: 0;
}

/* Shown while the pointer is over the board or the control itself. */
.pimble-board__markup--shown,
.pimble-board__markup:hover {
    opacity: 1;
}

/* Markup hidden: the control stays, dimmed, so nobody forgets the marks. */
.pimble-board__markup--hidden {
    opacity: 0.55;
}

.pimble-board__markup-icon {
    display: inline-flex;
    width: 14px;
    height: 14px;
}

/* Over a picture in text (docs/IMAGE_MARKUP_CONTRACT.md section 5): a box
   the size of the picture that lets the pointer through to the editor, with
   the controls in its top-right corner. */
.pimble-picture-overlay {
    position: fixed;
    z-index: 40;
    pointer-events: none;
}

.pimble-picture-controls {
    position: absolute;
    top: 6px;
    right: 6px;
    display: flex;
    align-items: center;
    gap: 4px;
    pointer-events: auto;
}

.pimble-picture-controls__eye,
.pimble-picture-controls__menu {
    position: static;
}

/* The Done button over a picture being marked up in text. */
.pimble-board__done {
    position: absolute;
    top: 8px;
    left: 8px;
    z-index: 5;
}

.pimble-board__panel {
    width: 232px;
    flex-shrink: 0;
    overflow-y: auto;
    overflow-x: hidden;
    padding: 8px;
    border-left: 1px solid var(--rinch-color-border);
    background: var(--rinch-color-body);
    font-size: 12px;
    color: var(--rinch-color-text);
}

.pimble-board__panel-title {
    font-size: 11px;
    font-weight: 600;
    text-transform: uppercase;
    color: var(--rinch-color-dimmed);
    margin: 6px 0 4px 0;
}

.pimble-board__nav {
    position: relative;
    width: 214px;
    height: 140px;
    display: flex;
    overflow: hidden;
    border: 1px solid var(--rinch-color-border);
    border-radius: 4px;
}

.pimble-board__nav-view {
    position: absolute;
    border: 1.5px solid var(--rinch-primary-color-5);
    pointer-events: none;
}

.pimble-board__frame-row {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 2px 4px;
    cursor: pointer;
    border-radius: 3px;
}

.pimble-board__frame-row:hover {
    background: var(--rinch-color-option-hover);
}

.pimble-board__section {
    margin-top: 8px;
}

.pimble-board__label {
    margin: 6px 0 3px 0;
    color: var(--rinch-color-dimmed);
}

.pimble-board__row {
    display: flex;
    gap: 4px;
    align-items: center;
}

.pimble-board__row--wrap {
    flex-wrap: wrap;
    margin-top: 6px;
}

.pimble-board__swatches {
    display: flex;
    flex-wrap: wrap;
    gap: 5px;
}

.pimble-board__picker {
    margin-top: 6px;
}

.pimble-board-swatch--none {
    background: linear-gradient(135deg, transparent 45%, #e5484d 45%, #e5484d 55%, transparent 55%);
}

.pimble-board-swatch--picker {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    border-radius: 4px;
    color: var(--rinch-color-dimmed);
}

.pimble-board-chip {
    padding: 2px 7px;
    border-radius: 4px;
    cursor: pointer;
    border: 1px solid var(--rinch-color-border);
    color: var(--rinch-color-text);
}

.pimble-board-chip--active {
    border-color: var(--rinch-primary-color-4);
    color: var(--rinch-primary-color-4);
}

.pimble-board__layers {
    display: flex;
    flex-direction: column;
    gap: 1px;
}

.pimble-board__layer {
    display: flex;
    align-items: center;
    gap: 4px;
    padding: 3px 4px;
    border-radius: 3px;
    cursor: pointer;
    border: 1px solid transparent;
}

.pimble-board__layer--picked {
    border-color: var(--rinch-primary-color-4);
}

.pimble-board__layer-icon {
    display: inline-flex;
    width: 16px;
    height: 16px;
    color: var(--rinch-color-dimmed);
}

.pimble-board__layer-name {
    flex: 1;
    overflow: hidden;
    white-space: nowrap;
}

.pimble-board__layer-name-input {
    flex: 1;
    width: 100%;
    min-width: 0;
    font-size: 12px;
    height: 18px;
}

.pimble-board__layer-count {
    color: var(--rinch-color-dimmed);
    font-size: 11px;
}

.pimble-board-toolbar {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    align-items: center;
    padding: 6px 12px;
}

.pimble-board-toolbar__group {
    display: flex;
    gap: 6px;
    align-items: center;
}

.pimble-board-toolbar__divider {
    width: 1px;
    height: 20px;
    background: var(--rinch-color-border);
    margin: 0 2px;
}

.pimble-board-tool {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 30px;
    height: 30px;
    border-radius: 4px;
    cursor: pointer;
    border: 1px solid transparent;
    color: var(--rinch-color-dimmed);
}

.pimble-board-tool--active {
    border-color: var(--rinch-primary-color-4);
    color: var(--rinch-primary-color-4);
}

.pimble-board-swatch {
    width: 18px;
    height: 18px;
    border-radius: 50%;
    cursor: pointer;
    border: 2px solid transparent;
    /* The black swatch on the dark theme. */
    box-shadow: 0 0 0 1px var(--rinch-color-border);
}

.pimble-board-swatch--active {
    border-color: var(--rinch-color-text);
}

.pimble-board-width {
    border-radius: 50%;
    background: currentColor;
}

.pimble-editor__content-wrap {
    flex: 1;
    display: flex;
    flex-direction: column;
    overflow-y: auto;
    /* `position: relative` + an explicit `z-index` (rather than the default
       `auto`) makes this its own CSS stacking context
       (rinch-dom/src/node.rs `Node::creates_stacking_context`: a positioned
       box only qualifies when `z_index.is_some()`). Without it, the overlay
       scrollbar this wrap paints for itself
       (rinch-dom/src/paint/mod.rs, the scroll-container overlay block right
       after this node's own children are painted) is drawn too early: the
       editor is `position: relative; z-index: 0`
       (rinch-editor-view/src/styles.rs, for its caret/selection overlays),
       which makes IT a stacking context too, so rinch's paint order
       (rinch-dom/src/stacking.rs) hoists the editor out of this wrap's own
       tree-order paint and defers it to the *nearest stacking-context
       ancestor* — previously several levels up, past this scrollbar draw —
       so the editor's own opaque background painted over the scrollbar a
       moment after it was drawn. Making this wrap the nearest stacking
       context keeps that hoist local: the editor now paints (via
       `paint_children_with_stacking`) before control returns to this node's
       own `paint_node` call, where the scrollbar is drawn last, on top. */
    position: relative;
    z-index: 0;
}

/* Size the rinch Editor to fill the content area. Done via a CSS rule (NOT an
   inline `style:` prop on the Editor) so it doesn't clobber the inline
   position/z-index the editor view sets for its caret + selection overlays.
   No `min-height: 0` here: the editor never sets its own `overflow-y`
   (it stays the CSS default, `visible`), so its automatic minimum size is
   content-based — forcing it to 0 let flex-shrink compress the editor's own
   layout box down to the content-wrap's visible height on a long document,
   which made every descendant below the fold report as *inside* the editor's
   box instead of overflowing it. `find_scroll_container`
   (rinch/src/app/hit_testing.rs) and the paint scrollbar
   (rinch-dom/src/paint/scrollbar.rs) both size a container from its
   immediate children's own layout boxes, not a deep descendant walk, so an
   editor box that never grows past its parent's visible height reads as
   nothing to scroll — the wheel handler never finds a scrollable ancestor,
   and no scrollbar is drawn. `.pimble-editor__content-wrap` supplies its own
   `overflow-y: auto`, so its *own* automatic minimum size already resolves to
   0 without help; the editor's must stay content-based so its box (and thus
   this wrap's measured content height) reflects what is actually on the
   page. A short document still fills the pane: `flex: 1 1 auto` grows it
   from that content-based floor, same as before. */
.pimble-editor__content-wrap > [data-pm-editor] {
    flex: 1 1 auto;
    border: none;
    border-radius: 0;
}

/* A picture is never wider than the pane it is read in
   (docs/IMAGES_CONTRACT.md): a phone's photograph is thousands of pixels
   across, and a narrow split pane is a few hundred. */
.pimble-editor__content-wrap > [data-pm-editor] img {
    max-width: 100%;
    height: auto;
}

.editor-toolbar {
    background: var(--rinch-color-surface) !important;
    border-bottom-color: var(--rinch-color-border) !important;
}
.editor-toolbar svg {
    width: 18px;
    height: 18px;
}

/* ── Empty state ────────────────────────────────────────────── */

.pimble-empty-state {
    flex: 1;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 12px;
    color: var(--rinch-color-dimmed);
    user-select: none;
}

.pimble-empty-state__icon svg {
    width: 48px;
    height: 48px;
    opacity: 0.15;
}

.pimble-empty-state__text {
    font-size: 14px;
    opacity: 0.5;
}

.pimble-empty-state__hint {
    font-size: 12px;
    opacity: 0.3;
}

/* ── Search bar + results panel ─────────────────────────────── */

.pimble-search-bar {
    display: flex;
    align-items: center;
    gap: 8px;
    /* As tall as the toolbar beside it (42px), so the line under both is one
       line across the window: the explorer header's top border here, the
       toolbar's bottom border there. */
    padding: 11px 12px;
    background: var(--rinch-color-body);
    flex-shrink: 0;
}

.pimble-search-bar__icon {
    display: inline-flex;
    color: var(--rinch-color-dimmed);
    width: 15px;
    height: 15px;
}
.pimble-search-bar__icon svg { width: 15px; height: 15px; }

.pimble-search-bar__input {
    flex: 1;
    min-width: 0;
    background: transparent;
    border: none;
    outline: none;
    color: var(--rinch-color-text);
    font-size: 13px;
    padding: 2px 0;
}

.pimble-search-results {
    flex: 1;
    display: flex;
    flex-direction: column;
    gap: 2px;
    overflow-y: auto;
    padding: 4px 6px 8px 8px;
}

.pimble-search-results__message {
    padding: 10px 4px;
    font-size: 12px;
    color: var(--rinch-color-dimmed);
    opacity: 0.6;
}

.pimble-search-result {
    display: flex;
    flex-direction: column;
    padding: 6px 8px;
    border-radius: var(--rinch-radius-sm);
    cursor: pointer;
}
.pimble-search-result:hover {
    background-color: var(--rinch-color-option-hover);
}

.pimble-search-result__title-row {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 6px;
}

.pimble-search-result__title {
    flex: 1;
    min-width: 0;
    font-size: 13px;
    font-weight: 500;
    color: var(--rinch-color-text);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
}

.pimble-search-result__kind {
    flex-shrink: 0;
    font-size: 10px;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--rinch-color-dimmed);
    opacity: 0.6;
}

.pimble-search-result__store {
    font-size: 11px;
    color: var(--rinch-primary-color-6);
    opacity: 0.8;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    margin-top: 1px;
}

.pimble-search-result__snippet {
    font-size: 12px;
    color: var(--rinch-color-dimmed);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    margin-top: 2px;
}

/* ── Status bar ─────────────────────────────────────────────── */

.pimble-status-bar {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 3px 12px;
    border-top: 1px solid var(--rinch-color-border);
    background: var(--rinch-color-body);
    font-size: 11px;
    color: var(--rinch-color-dimmed);
    flex-shrink: 0;
}

.pimble-status-bar__addr {
    opacity: 0.5;
    font-size: 11px;
}

/* The signed-in account; a click opens the Account modal. */
.pimble-status-bar__account {
    font-size: 11px;
    opacity: 0.7;
}

/* Restart to update (docs/UPDATE_CONTRACT.md): the one thing on the bar that
   asks for a click, so in the accent. */
.pimble-status-bar__update {
    font-size: 11px;
    color: var(--rinch-primary-color-6);
    cursor: pointer;
}

.pimble-status-bar__update:hover {
    text-decoration: underline;
}

.pimble-status-bar__account:hover {
    opacity: 1;
    text-decoration: underline;
}

/* A refusal the server sent back: not a broken connection, so it reads as a
   plain sentence rather than as the error badge. */
.pimble-status-bar__notice {
    font-size: 11px;
    color: var(--rinch-color-yellow-6);
}

/* The editor pane's read-only line, shown where the toolbar would be. */
.pimble-editor__read-only {
    padding: 6px 12px;
    font-size: 11px;
    color: var(--rinch-color-dimmed);
    border-top: 1px solid var(--rinch-color-border);
    border-bottom: 1px solid var(--rinch-color-border);
}

/* The link picker (docs/LINKS_CONTRACT.md \"Making a link\"). */
.pimble-link-picker {
    position: fixed;
    z-index: 1001;
    width: 320px;
    padding: 4px 0;
    background: var(--rinch-color-surface);
    border: 1px solid var(--rinch-color-border);
    border-radius: 6px;
    font-size: 13px;
}
.pimble-link-picker__query {
    padding: 4px 10px 6px;
    margin-bottom: 4px;
    border-bottom: 1px solid var(--rinch-color-border);
    color: var(--rinch-color-text);
}
.pimble-link-picker__row {
    display: flex;
    gap: 8px;
    padding: 4px 10px;
    cursor: pointer;
}
.pimble-link-picker__row--selected {
    background: var(--rinch-color-option-hover);
}
.pimble-link-picker__label {
    flex: 1;
    color: var(--rinch-color-text);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
}
.pimble-link-picker__detail {
    color: var(--rinch-color-dimmed);
    white-space: nowrap;
}
.pimble-link-picker__empty {
    padding: 4px 10px;
    color: var(--rinch-color-dimmed);
}

/* The tooltip over a hovered link (docs/LINKS_CONTRACT.md). */
.pimble-link-tooltip {
    position: fixed;
    z-index: 1000;
    /* Fills max-width instead of fitting its text: joeleaver/rinch#893. */
    max-width: 420px;
    padding: 4px 8px;
    background: var(--rinch-color-surface);
    border: 1px solid var(--rinch-color-border);
    border-radius: 4px;
    font-size: 12px;
    pointer-events: none;
}
.pimble-link-tooltip__target {
    color: var(--rinch-color-text);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
}
.pimble-link-tooltip__hint {
    color: var(--rinch-color-dimmed);
}
";

/// Editor content styles for the editor pane.
///
/// rinch's editor ships its own stylesheet, scoped to `[data-pm-editor]`, with a
/// dark scheme keyed on `data-pm-theme="dark"` (set from `editor::start_editing`).
/// These rules ride on top of that scheme with higher specificity and swap its
/// GitHub-dark palette for the app's own theme tokens, so the pane matches the
/// tree and toolbar around it.
pub(crate) const EDITOR_CSS: &str = "
.pimble-editor__content-wrap > [data-pm-editor][data-pm-theme=\"dark\"] {
    background: var(--rinch-color-body);
    color: var(--rinch-color-text);
    border-color: var(--rinch-color-border);
    font-size: 15px;
    line-height: 1.7;
}

.pimble-editor__content-wrap > [data-pm-editor][data-pm-theme=\"dark\"] h1,
.pimble-editor__content-wrap > [data-pm-editor][data-pm-theme=\"dark\"] h2,
.pimble-editor__content-wrap > [data-pm-editor][data-pm-theme=\"dark\"] h3,
.pimble-editor__content-wrap > [data-pm-editor][data-pm-theme=\"dark\"] h4 {
    color: var(--rinch-color-text);
}
.pimble-editor__content-wrap > [data-pm-editor][data-pm-theme=\"dark\"] h5,
.pimble-editor__content-wrap > [data-pm-editor][data-pm-theme=\"dark\"] h6 {
    color: var(--rinch-color-dimmed);
}

/* Links, Pimble and web alike (docs/LINKS_CONTRACT.md \"What a link looks like\"):
   the link color, underlined only under the pointer. The contract's arrow after a
   web link waits for rinch: generated content (`::after`) inside the editor puts the
   caret beside the wrong character (joeleaver/rinch#891). */
.pimble-editor__content-wrap > [data-pm-editor] a {
    color: var(--rinch-primary-color-6);
    text-decoration: none;
}
.pimble-editor__content-wrap > [data-pm-editor][data-pm-theme=\"dark\"] a {
    color: var(--rinch-primary-color-4);
}
.pimble-editor__content-wrap > [data-pm-editor] a:hover {
    text-decoration: underline;
}

.pimble-editor__content-wrap > [data-pm-editor][data-pm-theme=\"dark\"] code {
    background: var(--rinch-color-option-hover);
    color: #e06c75;
}
.pimble-editor__content-wrap > [data-pm-editor][data-pm-theme=\"dark\"] pre {
    background: var(--rinch-color-surface);
}
.pimble-editor__content-wrap > [data-pm-editor][data-pm-theme=\"dark\"] pre code {
    background: none;
    color: #abb2bf;
}

.pimble-editor__content-wrap > [data-pm-editor][data-pm-theme=\"dark\"] blockquote {
    border-left-color: var(--rinch-primary-color-7);
    color: var(--rinch-color-dimmed);
}

.pimble-editor__content-wrap > [data-pm-editor][data-pm-theme=\"dark\"] hr {
    border-top-color: var(--rinch-color-border);
}

.pimble-editor__content-wrap > [data-pm-editor][data-pm-theme=\"dark\"] table,
.pimble-editor__content-wrap > [data-pm-editor][data-pm-theme=\"dark\"] td,
.pimble-editor__content-wrap > [data-pm-editor][data-pm-theme=\"dark\"] th {
    border-color: var(--rinch-color-border);
}
.pimble-editor__content-wrap > [data-pm-editor][data-pm-theme=\"dark\"] th {
    background: var(--rinch-color-surface);
}

.pimble-editor__content-wrap > [data-pm-editor][data-pm-theme=\"dark\"] [data-pm-placeholder] {
    color: var(--rinch-color-dimmed);
}

/* The light scheme, the same rules over rinch's GitHub-light palette: the app's
   tokens, the dark scheme's size and leading, and code in a muted red as dark's
   is in a bright one. */
.pimble-editor__content-wrap > [data-pm-editor][data-pm-theme=\"light\"] {
    background: var(--rinch-color-surface);
    color: var(--rinch-color-text);
    border-color: var(--rinch-color-border);
    font-size: 15px;
    line-height: 1.7;
}
.pimble-editor__content-wrap > [data-pm-editor][data-pm-theme=\"light\"] h1,
.pimble-editor__content-wrap > [data-pm-editor][data-pm-theme=\"light\"] h2,
.pimble-editor__content-wrap > [data-pm-editor][data-pm-theme=\"light\"] h3,
.pimble-editor__content-wrap > [data-pm-editor][data-pm-theme=\"light\"] h4 {
    color: var(--rinch-color-text);
}
.pimble-editor__content-wrap > [data-pm-editor][data-pm-theme=\"light\"] h5,
.pimble-editor__content-wrap > [data-pm-editor][data-pm-theme=\"light\"] h6 {
    color: var(--rinch-color-dimmed);
}
.pimble-editor__content-wrap > [data-pm-editor][data-pm-theme=\"light\"] code {
    background: #eef1f4;
    color: #a3404c;
}
.pimble-editor__content-wrap > [data-pm-editor][data-pm-theme=\"light\"] pre {
    background: var(--rinch-color-body);
}
.pimble-editor__content-wrap > [data-pm-editor][data-pm-theme=\"light\"] pre code {
    background: none;
    color: #383a42;
}
.pimble-editor__content-wrap > [data-pm-editor][data-pm-theme=\"light\"] blockquote {
    border-left-color: var(--rinch-primary-color-3);
    color: var(--rinch-color-dimmed);
}
.pimble-editor__content-wrap > [data-pm-editor][data-pm-theme=\"light\"] hr {
    border-top-color: var(--rinch-color-border);
}
.pimble-editor__content-wrap > [data-pm-editor][data-pm-theme=\"light\"] table,
.pimble-editor__content-wrap > [data-pm-editor][data-pm-theme=\"light\"] td,
.pimble-editor__content-wrap > [data-pm-editor][data-pm-theme=\"light\"] th {
    border-color: var(--rinch-color-border);
}
.pimble-editor__content-wrap > [data-pm-editor][data-pm-theme=\"light\"] th {
    background: var(--rinch-color-body);
}
.pimble-editor__content-wrap > [data-pm-editor][data-pm-theme=\"light\"] [data-pm-placeholder] {
    color: var(--rinch-color-dimmed);
}
";

/// Pimble's variables for the dark scheme, added to rinch's theme sheet
/// (`app::theme_css`). Dark keeps rinch's palette; these name what the toolbar
/// draws with, so both schemes style it from the same rules.
pub(crate) const DARK_THEME_VARS: &str = "
:root {
    --pimble-toolbar-icon: #909296;
    --pimble-toolbar-active-bg: var(--rinch-color-dark-4);
    --pimble-toolbar-active-fg: var(--rinch-primary-color-4);
    --pimble-toolbar-active-border: var(--rinch-primary-color-4);
}
";

/// Pimble's light scheme (2026-10-20), added to rinch's theme sheet
/// (`app::theme_css`). rinch's light theme is Mantine's stock grays with a bright
/// blue primary and a blue title bar; this one uses the site's cool neutrals
/// (`site/style.css`) and the brand slate (`assets/brand/README.md`) as the
/// primary, ten shades around `#3f6491` at 6. The chrome (title bar, explorer,
/// pane headers) sits on a cool off-white, and the page being written on is white.
pub(crate) const LIGHT_THEME_VARS: &str = "
:root {
    --rinch-color-body: #f5f7f8;
    --rinch-color-surface: #ffffff;
    --rinch-color-text: #1c2024;
    --rinch-color-dimmed: #5c6570;
    --rinch-color-border: #dde1e5;
    --rinch-color-default-border: #dde1e5;
    --rinch-color-placeholder: #8a929c;
    --rinch-color-default: #eceff2;
    --rinch-color-default-hover: #e2e6ea;
    --rinch-color-filled: #f0f2f4;
    --rinch-color-filled-hover: #e6e9ec;
    --rinch-color-state-disabled: #f0f2f4;
    --rinch-color-option-hover: #edf1f5;
    --rinch-color-option-selected: #e1e8f0;

    --rinch-primary-color: #3f6491;
    --rinch-primary-color-0: #eef2f7;
    --rinch-primary-color-1: #dde5ef;
    --rinch-primary-color-2: #bccbdf;
    --rinch-primary-color-3: #98aecb;
    --rinch-primary-color-4: #7793b8;
    --rinch-primary-color-5: #5a7ba6;
    --rinch-primary-color-6: #3f6491;
    --rinch-primary-color-7: #34557c;
    --rinch-primary-color-8: #2a4665;
    --rinch-primary-color-9: #1f354d;

    --rinch-titlebar-bg: #eaeef1;
    --rinch-titlebar-text: #1c2024;
    --rinch-titlebar-icon: #5c6570;
    --rinch-titlebar-hover: rgba(28, 32, 36, 0.07);
    --rinch-titlebar-active: rgba(28, 32, 36, 0.12);

    --pimble-toolbar-icon: #5c6570;
    --pimble-toolbar-active-bg: #e1e8f0;
    --pimble-toolbar-active-fg: #2a4665;
    --pimble-toolbar-active-border: #bccbdf;
}
";
