//! The account pages, inside the app.
//!
//! `/app/signup`, `/app/login` and `/app/account` are rinch views in the same
//! wasm binary as the tree and the editor, for one reason: signing in derives
//! keys that must stay in this page's memory, and a static HTML page could not
//! hold them (docs/CRYPTO_CONTRACT.md, "Web app"). Moving between these and the
//! app is `crate::route::go`, never a link that reloads.

pub mod account;
pub mod forgot;
pub mod login;
pub mod recover;
pub mod signup;

/// Layout for the three account pages. Everything else comes from rinch's
/// theme, so both colour schemes work with no extra rules here.
pub const PAGE_CSS: &str = "
.pimble-page {
    min-height: 100vh;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    padding: 40px 16px;
    background: var(--rinch-color-body);
    color: var(--rinch-color-text);
    font-family: system-ui, -apple-system, 'Segoe UI', sans-serif;
    box-sizing: border-box;
}

.pimble-page--wide {
    justify-content: flex-start;
}

.pimble-page__card {
    width: 100%;
    max-width: 380px;
    display: flex;
    flex-direction: column;
    gap: 14px;
}

.pimble-page__card--wide {
    max-width: 720px;
}

.pimble-page__brand {
    font-size: 22px;
    font-weight: 700;
    letter-spacing: -0.01em;
    margin: 0 0 2px;
}

.pimble-page__lede {
    font-size: 13px;
    color: var(--rinch-color-dimmed);
    margin: 0;
}

.pimble-page__footer {
    font-size: 13px;
    color: var(--rinch-color-dimmed);
    display: flex;
    gap: 6px;
    flex-wrap: wrap;
    align-items: baseline;
}

.pimble-link {
    color: var(--rinch-primary-color-4);
    cursor: pointer;
    text-decoration: none;
}

.pimble-link:hover {
    text-decoration: underline;
}

.pimble-page__error {
    font-size: 13px;
    color: var(--rinch-color-error, #ff6b6b);
}

/* The recovery code, shown exactly once. Monospaced and large enough to copy
   off the screen by hand, which is the point of it. */
.pimble-recovery {
    font-family: 'SFMono-Regular', ui-monospace, Menlo, Consolas, monospace;
    font-size: 17px;
    letter-spacing: 0.06em;
    text-align: center;
    padding: 14px 10px;
    border-radius: 6px;
    border: 1px solid var(--rinch-color-border);
    background: var(--rinch-color-surface, rgba(128,128,128,0.08));
    word-break: break-all;
}

.pimble-confirm {
    display: flex;
    align-items: flex-start;
    gap: 8px;
    font-size: 13px;
    line-height: 1.4;
}

/* ── Account page ─────────────────────────────────────────────── */

.pimble-account__bar {
    width: 100%;
    max-width: 720px;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    margin-bottom: 18px;
}

.pimble-account__who {
    font-size: 13px;
    color: var(--rinch-color-dimmed);
}

.pimble-store {
    border: 1px solid var(--rinch-color-border);
    border-radius: 6px;
    padding: 12px 14px;
    display: flex;
    flex-direction: column;
    gap: 10px;
}

.pimble-store__head {
    display: flex;
    align-items: center;
    gap: 10px;
    justify-content: space-between;
}

.pimble-store__name {
    font-weight: 600;
}

.pimble-store__meta {
    font-size: 12px;
    color: var(--rinch-color-dimmed);
    display: flex;
    gap: 8px;
}

.pimble-member {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 10px;
    font-size: 13px;
    padding: 3px 0;
}

.pimble-member__role {
    color: var(--rinch-color-dimmed);
    font-size: 12px;
}

.pimble-row {
    display: flex;
    gap: 8px;
    align-items: flex-end;
    flex-wrap: wrap;
}

.pimble-row > * {
    flex: 1 1 140px;
}

.pimble-row > .pimble-row__fixed {
    flex: 0 0 auto;
}
";
