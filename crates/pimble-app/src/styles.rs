//! CSS constants for the Pimble application.

/// Application-wide layout styles.
pub(crate) const APP_CSS: &str = "
/* ── Sidebar ────────────────────────────────────────────────── */

.pimble-sidebar {
    width: 260px;
    min-width: 200px;
    display: flex;
    flex-direction: column;
    background: var(--rinch-color-body);
    border-right: 1px solid var(--rinch-color-border);
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

.pimble-sidebar__tree {
    flex: 1;
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

.pimble-editor__toolbar-wrap {
    border-top: 1px solid var(--rinch-color-border);
    border-bottom: 1px solid var(--rinch-color-border);
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
    padding: 6px 12px;
    background: var(--rinch-color-body);
    border-bottom: 1px solid var(--rinch-color-border);
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

.pimble-editor__content-wrap > [data-pm-editor][data-pm-theme=\"dark\"] a {
    color: var(--rinch-primary-color-4);
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
";
