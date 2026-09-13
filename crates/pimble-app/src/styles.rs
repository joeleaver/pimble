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
/// Designed for dark mode. Scoped to `.editor-content` so they don't leak
/// into the rest of the UI.
pub(crate) const EDITOR_CSS: &str = "
.editor-content {
    font-family: -apple-system, BlinkMacSystemFont, Segoe UI, Roboto, Helvetica, Arial, sans-serif;
    font-size: 15px;
    line-height: 1.7;
    color: var(--rinch-color-text);
    cursor: text;
}

/* --- Block elements --- */

.editor-content p { margin: 0 0 4px 0; }

.editor-content h1 {
    font-size: 1.75em;
    font-weight: 700;
    margin: 24px 0 8px 0;
    color: var(--rinch-color-text);
}

.editor-content h2 {
    font-size: 1.4em;
    font-weight: 700;
    margin: 20px 0 6px 0;
    color: var(--rinch-color-text);
}

.editor-content h3 {
    font-size: 1.15em;
    font-weight: 600;
    margin: 16px 0 4px 0;
    color: var(--rinch-color-text);
}

.editor-content h4 {
    font-size: 1em;
    font-weight: 600;
    margin: 14px 0 4px 0;
    color: var(--rinch-color-text);
}

.editor-content h5 {
    font-size: 0.95em;
    font-weight: 600;
    margin: 12px 0 2px 0;
    color: var(--rinch-color-dimmed);
}

.editor-content h6 {
    font-size: 0.85em;
    font-weight: 600;
    margin: 12px 0 2px 0;
    color: var(--rinch-color-dimmed);
}

/* --- Blockquotes --- */

.editor-content blockquote {
    border-left: 3px solid var(--rinch-primary-color-7);
    padding-left: 14px;
    margin: 12px 0;
    color: var(--rinch-color-dimmed);
}

.editor-content blockquote p { margin: 0 0 4px 0; }

/* --- Code --- */

.editor-content code {
    background: var(--rinch-color-dark-5);
    padding: 1px 5px;
    border-radius: 3px;
    color: #e06c75;
    font-size: 0.88em;
}

.editor-content pre {
    background: var(--rinch-color-dark-5);
    border-radius: 6px;
    padding: 12px 14px;
    margin: 12px 0;
    font-size: 13px;
    line-height: 1.5;
}

.editor-content pre code {
    background: none;
    padding: 0;
    color: #abb2bf;
    font-size: inherit;
}

/* --- Lists --- */

.editor-content ul,
.editor-content ol {
    margin: 6px 0;
    padding-left: 8px;
}

.editor-content li {
    margin: 2px 0;
    padding-left: 4px;
}

.editor-content ul > li::before {
    content: \"\\2022  \";
    color: var(--rinch-color-dimmed);
}

.editor-content ol > li::before {
    content: \"\\2013  \";
    color: var(--rinch-color-dimmed);
}

.editor-content ul ul > li::before {
    content: \"\\25E6  \";
}

.editor-content ul ul ul > li::before {
    content: \"\\25AA  \";
}

/* --- Horizontal rule --- */

.editor-content hr {
    border: none;
    border-top: 1px solid var(--rinch-color-dark-4);
    margin: 20px 0;
}

/* --- Inline formatting --- */

.editor-content strong { font-weight: 700; color: var(--rinch-color-text); }
.editor-content em { font-style: italic; }
.editor-content u { text-decoration: underline; }
.editor-content s { text-decoration: line-through; color: var(--rinch-color-dimmed); }
.editor-content a { color: var(--rinch-primary-color-4); text-decoration: none; }
.editor-content a:hover { text-decoration: underline; }
.editor-content sub { font-size: 0.8em; }
.editor-content sup { font-size: 0.8em; }
";
