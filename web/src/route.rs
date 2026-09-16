//! Which page this URL means, and how to get to another one.
//!
//! The account pages and the app itself are one wasm binary served under
//! `/app/`, so moving between them must never reload the page: the unwrapped
//! account keys live in memory (`crate::session`) and a reload is exactly what
//! throws them away. Every move therefore goes through [`go`], which pushes a
//! history entry and swaps what is on screen.
//!
//! The two halves are kept apart on purpose. An account page is mounted and
//! unmounted freely; the app is mounted **once** and afterwards only hidden and
//! shown, because behind it sit a WebSocket, a set of subscriptions, an editor
//! session and a vault client, none of which should be torn down because
//! somebody looked at their store list.
//!
//! The server side of this is one rule: every path under `/app/` serves the
//! same `index.html`. `trunk serve` does that for unknown paths already, and
//! jkbase's edge routes the whole prefix.

use std::cell::{Cell, RefCell};

use rinch_core::dom::{NodeHandle, RenderScope};
use rinch_web::RootHandle;
use wasm_bindgen::prelude::Closure;
use wasm_bindgen::{JsCast, JsValue};

/// The prefix the app is served under.
const BASE: &str = "/app";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    Signup,
    Login,
    /// "I forgot my password": ask for the address to mail a link to.
    Forgot,
    /// The link's destination, carrying the token in its query.
    Recover,
    Account,
    /// The app itself: the tree, the editor, the stores.
    App,
}

impl Route {
    pub fn path(self) -> &'static str {
        match self {
            Route::Signup => "/app/signup",
            Route::Login => "/app/login",
            Route::Forgot => "/app/forgot",
            Route::Recover => "/app/recover",
            Route::Account => "/app/account",
            Route::App => "/app/",
        }
    }
}

thread_local! {
    /// The account page currently mounted, so the next one can replace it.
    static PAGE_ROOT: RefCell<Option<RootHandle>> = const { RefCell::new(None) };
    static PAGE_HOST: RefCell<Option<web_sys::Element>> = const { RefCell::new(None) };
    static APP_HOST: RefCell<Option<web_sys::Element>> = const { RefCell::new(None) };
    /// Whether the app has been built into its host yet.
    static APP_STARTED: Cell<bool> = const { Cell::new(false) };
    /// Kept alive for the page's lifetime so back and forward keep working.
    static POPSTATE: RefCell<Option<Closure<dyn FnMut()>>> = const { RefCell::new(None) };
}

/// The route this page's URL names. Anything unrecognised under `/app/` is the
/// app, which is what a bookmark or a deep link should get.
pub fn current() -> Route {
    let path = web_sys::window()
        .and_then(|w| w.location().pathname().ok())
        .unwrap_or_else(|| BASE.to_string());
    from_path(&path)
}

fn from_path(path: &str) -> Route {
    let rest = path.strip_prefix(BASE).unwrap_or(path);
    match rest.trim_end_matches('/') {
        "/signup" => Route::Signup,
        "/login" => Route::Login,
        "/forgot" => Route::Forgot,
        "/recover" => Route::Recover,
        "/account" => Route::Account,
        _ => Route::App,
    }
}

/// Go to `route`: push the history entry, then show it.
pub fn go(route: Route) {
    push_url(route.path(), false);
    render(route);
}

/// Go to `route` without leaving the current URL behind — for the redirect a
/// visitor with no session gets, which they should not be able to walk back
/// into.
pub fn replace_with(route: Route) {
    push_url(route.path(), true);
    render(route);
}

fn push_url(path: &str, replace: bool) {
    let Some(history) = web_sys::window().and_then(|w| w.history().ok()) else { return };
    let _ = if replace {
        history.replace_state_with_url(&JsValue::NULL, "", Some(path))
    } else {
        history.push_state_with_url(&JsValue::NULL, "", Some(path))
    };
}

/// Start routing: follow back and forward, then show what the URL names.
pub fn start() {
    let closure = Closure::wrap(Box::new(|| render(current())) as Box<dyn FnMut()>);
    if let Some(window) = web_sys::window() {
        window.set_onpopstate(Some(closure.as_ref().unchecked_ref()));
    }
    POPSTATE.with(|slot| *slot.borrow_mut() = Some(closure));
    render(current());
}

/// Put `route` on screen, replacing whatever is there.
pub fn render(route: Route) {
    match route {
        Route::App => {
            unmount_page();
            show(&page_host(), false);
            show(&app_host(), true);
            if !APP_STARTED.get() {
                APP_STARTED.set(true);
                crate::app_page::start();
            } else {
                // Coming back from the account pages, where a store may have
                // been created or shared. The token this app holds predates
                // that grant and `listStores` answers strictly by the token,
                // so both are redone before the tree is believed again.
                crate::backend::request_refresh();
            }
        }
        Route::Signup => mount_page(crate::pages::signup::signup_page),
        Route::Login => mount_page(crate::pages::login::login_page),
        Route::Forgot => mount_page(crate::pages::forgot::forgot_page),
        Route::Recover => mount_page(crate::pages::recover::recover_page),
        Route::Account => mount_page(crate::pages::account::account_page),
    }
}

/// Build the app into its own host, under its menu bar. Called once, by
/// `app_page`.
///
/// The menus are `pimble_app::menus`, the same values the desktop hands its
/// native bar, rendered here by rinch-web's DOM bar. Every item's accelerator
/// is armed against the document by that call, so Ctrl+K reaches "Focus
/// Search" without this crate binding anything.
pub fn mount_app<F>(store: pimble_app::state::AppStore, build: F)
where
    F: FnOnce(&mut RenderScope) -> NodeHandle,
{
    let host = app_host();
    let theme = pimble_app::app::theme_props(true);
    let menus = pimble_app::menus::build_menus(store);
    let borrowed: Vec<(&str, rinch::menu::Menu)> =
        menus.into_iter().map(|(title, menu)| (title, menu)).collect();
    rinch_web::mount_into_with_menu_bar(&host, theme, borrowed, build);
}

/// The app could not start. Undo the "started" flag so a later attempt (a fresh
/// sign-in, say) tries again.
pub fn app_start_failed() {
    APP_STARTED.set(false);
}

fn mount_page<F>(build: F)
where
    F: FnOnce(&mut RenderScope) -> NodeHandle,
{
    show(&app_host(), false);
    unmount_page();

    let host = page_host();
    show(&host, true);
    let theme = pimble_app::app::theme_props(true);
    let handle = rinch_web::mount_into(&host, theme, build);
    PAGE_ROOT.with(|slot| *slot.borrow_mut() = Some(handle));
}

fn unmount_page() {
    PAGE_ROOT.with(|slot| {
        if let Some(handle) = slot.borrow_mut().take() {
            handle.unmount();
        }
    });
}

fn show(element: &web_sys::Element, visible: bool) {
    let _ = element.set_attribute(
        "style",
        if visible { "height: 100%;" } else { "display: none;" },
    );
}

fn page_host() -> web_sys::Element {
    host(&PAGE_HOST, "pimble-page-root")
}

fn app_host() -> web_sys::Element {
    host(&APP_HOST, "pimble-app-root")
}

/// Put a plain message where a page would have gone.
///
/// Deliberately not a rinch tree: whatever went wrong happened before the app
/// existed, and a failure to start should not depend on the thing that failed.
pub fn show_startup_error(message: &str) {
    tracing::error!("{}", message);
    let host = page_host();
    show(&host, true);
    show(&app_host(), false);
    unmount_page();
    host.set_text_content(Some(message));
    let _ = host.set_attribute(
        "style",
        "font: 15px/1.5 system-ui, sans-serif; color: #c1c2c5; padding: 48px;",
    );
}

fn host(
    slot: &'static std::thread::LocalKey<RefCell<Option<web_sys::Element>>>,
    id: &str,
) -> web_sys::Element {
    if let Some(existing) = slot.with(|s| s.borrow().clone()) {
        return existing;
    }
    let document = web_sys::window().and_then(|w| w.document()).expect("a document");
    let element = document.create_element("div").expect("an element to mount into");
    let _ = element.set_attribute("id", id);
    let _ = element.set_attribute("style", "display: none;");
    if let Some(body) = document.body() {
        let _ = body.append_child(&element);
    }
    slot.with(|s| *s.borrow_mut() = Some(element.clone()));
    element
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_map_to_routes() {
        assert_eq!(from_path("/app/login"), Route::Login);
        assert_eq!(from_path("/app/login/"), Route::Login);
        assert_eq!(from_path("/app/signup"), Route::Signup);
        assert_eq!(from_path("/app/forgot"), Route::Forgot);
        assert_eq!(from_path("/app/recover"), Route::Recover);
        assert_eq!(from_path("/app/account"), Route::Account);
        assert_eq!(from_path("/app/"), Route::App);
        assert_eq!(from_path("/app"), Route::App);
        assert_eq!(from_path("/app/anything-else"), Route::App);
    }
}
