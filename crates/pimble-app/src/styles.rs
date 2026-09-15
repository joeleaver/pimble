//! CSS constants for the Pimble application.

/// Application-wide layout styles.
pub(crate) const APP_CSS: &str = "
/* ── Sidebar ────────────────────────────────────────────────── */

.pimble-sidebar {
    width: 260px;
    min-width: 200px;
    display: flex;
    flex-direction: column;
    background: var(--rinch-color-dark-7);
    border-right: 1px solid var(--rinch-color-dark-4);
}

.pimble-sidebar__header {
    display: flex;
    align-items: center;
    padding: 8px 12px 6px;
    gap: 8px;
    border-top: 1px solid var(--rinch-color-dark-4);
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
    background-color: var(--rinch-color-dark-5);
}
.rinch-tree__node-content--selected {
    background-color: var(--rinch-color-dark-5);
    color: var(--rinch-color-text);
}
.rinch-tree__node-content--selected:hover {
    background-color: var(--rinch-color-dark-4);
}
.rinch-tree__node-content--selected .rinch-tree__icon {
    color: var(--rinch-primary-color-4);
}
.rinch-tree__node-content--selected .rinch-tree__chevron {
    color: var(--rinch-color-text);
}

/* ── Context menu (dark theme overrides) ────────────────────── */

.rinch-context-menu__dropdown {
    background-color: var(--rinch-color-dark-6);
    border: 1px solid var(--rinch-color-dark-4);
    border-radius: var(--rinch-radius-default);
    box-shadow: 0 4px 12px rgba(0, 0, 0, 0.4);
}

.rinch-dropdown-menu__item {
    color: var(--rinch-color-text);
    padding: 6px 12px;
    font-size: 13px;
}

.rinch-dropdown-menu__item:hover {
    background-color: var(--rinch-color-dark-4);
}

.rinch-dropdown-menu__divider {
    border-color: var(--rinch-color-dark-4);
}

/* Mount point icon gets a distinct color */
.rinch-tree__icon--mount {
    color: var(--rinch-primary-color-6) !important;
}
.rinch-tree__node-content--selected .rinch-tree__icon--mount {
    color: var(--rinch-primary-color-4) !important;
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
    border-top: 1px solid var(--rinch-color-dark-4);
    border-bottom: 1px solid var(--rinch-color-dark-4);
}

.pimble-editor__content-wrap {
    flex: 1;
    display: flex;
    flex-direction: column;
    overflow-y: auto;
}

/* Size the rinch Editor to fill the content area. Done via a CSS rule (NOT an
   inline `style:` prop on the Editor) so it doesn't clobber the inline
   position/z-index the editor view sets for its caret + selection overlays. */
.pimble-editor__content-wrap > [data-pm-editor] {
    flex: 1 1 auto;
    min-height: 0;
    border: none;
    border-radius: 0;
}

.editor-toolbar {
    background: var(--rinch-color-dark-6) !important;
    border-bottom-color: var(--rinch-color-dark-4) !important;
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
    background: var(--rinch-color-dark-7);
    border-bottom: 1px solid var(--rinch-color-dark-4);
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
    background-color: var(--rinch-color-dark-5);
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
    border-top: 1px solid var(--rinch-color-dark-4);
    background: var(--rinch-color-dark-7);
    font-size: 11px;
    color: var(--rinch-color-dimmed);
    flex-shrink: 0;
}

.pimble-status-bar__addr {
    opacity: 0.5;
    font-size: 11px;
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
    background: var(--rinch-color-dark-7);
    color: var(--rinch-color-text);
    border-color: var(--rinch-color-dark-4);
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
    background: var(--rinch-color-dark-5);
    color: #e06c75;
}
.pimble-editor__content-wrap > [data-pm-editor][data-pm-theme=\"dark\"] pre {
    background: var(--rinch-color-dark-6);
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
    border-top-color: var(--rinch-color-dark-4);
}

.pimble-editor__content-wrap > [data-pm-editor][data-pm-theme=\"dark\"] table,
.pimble-editor__content-wrap > [data-pm-editor][data-pm-theme=\"dark\"] td,
.pimble-editor__content-wrap > [data-pm-editor][data-pm-theme=\"dark\"] th {
    border-color: var(--rinch-color-dark-4);
}
.pimble-editor__content-wrap > [data-pm-editor][data-pm-theme=\"dark\"] th {
    background: var(--rinch-color-dark-6);
}

.pimble-editor__content-wrap > [data-pm-editor][data-pm-theme=\"dark\"] [data-pm-placeholder] {
    color: var(--rinch-color-dimmed);
}
";
