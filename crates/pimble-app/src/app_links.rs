//! Links from outside the app (docs/SETTINGS_CONTRACT.md section 12): a
//! `pimble:` link the operating system started `pimble <link>` with (handed to
//! the window already open), the one this process was started with, the web
//! app's `?open=` parameter, and the plain launch that only brings the running
//! window to the front (docs/NOTIFICATIONS_CONTRACT.md "Built in wave 4").

use crate::links::follow_href;

/// A `pimble:` link this app was asked to open from outside it: a click in a
/// browser or a mail app (the operating system starts `pimble <link>`, which
/// hands it to the window already open), or the web app's `?open=` parameter.
#[derive(Debug, Clone, PartialEq)]
pub enum AppLink {
    /// A link to a node, as Copy Link makes one.
    Node(pimble_core::PimbleUrl),
    /// `pimble:settings`, `pimble:settings/<section>`, and
    /// `pimble:settings/plugins?install=<id>`: Settings at that section, and
    /// at that plugin's row in the library with Install ready. Nothing is
    /// installed until the person presses it.
    Settings { section: Option<crate::settings::Section>, install: Option<String> },
    /// `pimble:` alone ([`COME_TO_FRONT`]): the window, brought to the front
    /// with nothing opened in it.
    Window,
}

/// What a plain launch hands to the Pimble already running (`main.rs`, docs/NOTIFICATIONS_CONTRACT.md
/// "Built in wave 4"): come to the front, out of the tray if it is there.
pub const COME_TO_FRONT: &str = "pimble:";

impl AppLink {
    pub fn parse(text: &str) -> Option<AppLink> {
        let text = text.trim();
        if let Some(url) = pimble_core::PimbleUrl::parse(text) {
            return Some(AppLink::Node(url));
        }
        let rest = text.strip_prefix("pimble:")?.trim_start_matches("//");
        if rest.is_empty() {
            return Some(AppLink::Window);
        }
        let (path, query) = rest.split_once('?').unwrap_or((rest, ""));
        let mut parts = path.trim_end_matches('/').split('/');
        if parts.next()? != "settings" {
            return None;
        }
        let section = match parts.next() {
            None | Some("") => None,
            Some(key) => Some(crate::settings::Section::from_key(key)?),
        };
        if parts.next().is_some() {
            return None;
        }
        let install = query.split('&').find_map(|pair| pair.strip_prefix("install=")).filter(|id| {
            !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        });
        let install = match (section, install) {
            (Some(crate::settings::Section::Plugins), Some(id)) => Some(id.to_string()),
            _ => None,
        };
        Some(AppLink::Settings { section, install })
    }
}

/// Open a link that came from outside the app; anything else is said and
/// ignored.
pub fn open_app_link(store: crate::state::AppStore, text: &str) {
    match AppLink::parse(text) {
        Some(AppLink::Node(_)) => {
            follow_href(store, text.trim());
        }
        // The window came to the front already (`BackendEvent::AppLink`).
        Some(AppLink::Window) => {}
        Some(AppLink::Settings { section, install }) => {
            match section {
                Some(section) => crate::settings::open_at(store, section),
                None => crate::settings::open(store),
            }
            if let Some(id) = install {
                crate::plugins::offer_install(&id);
            }
        }
        None => crate::events::show_notice(store, format!("Pimble does not know how to open {text}.")),
    }
}

/// The link this process was started with, opened once the app has
/// connected (`set_launch_link`, `take_launch_link`).
static LAUNCH_LINK: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

pub fn set_launch_link(link: String) {
    *LAUNCH_LINK.lock().unwrap_or_else(|e| e.into_inner()) = Some(link);
}

pub(crate) fn take_launch_link() -> Option<String> {
    LAUNCH_LINK.lock().unwrap_or_else(|e| e.into_inner()).take()
}

#[cfg(test)]
mod tests {
    use super::AppLink;
    use crate::settings::Section;

    #[test]
    fn settings_links_read_and_nothing_else_does() {
        assert_eq!(AppLink::parse("pimble:settings"), Some(AppLink::Settings { section: None, install: None }));
        assert_eq!(AppLink::parse("pimble:settings/keyboard"), Some(AppLink::Settings { section: Some(Section::Keyboard), install: None }));
        assert_eq!(
            AppLink::parse("pimble:settings/plugins?install=kanban-viewer"),
            Some(AppLink::Settings { section: Some(Section::Plugins), install: Some("kanban-viewer".into()) })
        );
        assert_eq!(AppLink::parse("pimble://settings/plugins/?install=todo-viewer").map(|l| matches!(l, AppLink::Settings { install: Some(_), .. })), Some(true));
        // An install only means something at Plugins, and an id is a plain one.
        assert_eq!(AppLink::parse("pimble:settings/general?install=kanban-viewer"), Some(AppLink::Settings { section: Some(Section::General), install: None }));
        assert_eq!(AppLink::parse("pimble:settings/plugins?install=../x"), Some(AppLink::Settings { section: Some(Section::Plugins), install: None }));
        assert_eq!(AppLink::parse("pimble:settings/nowhere"), None);
        assert_eq!(AppLink::parse(super::COME_TO_FRONT), Some(AppLink::Window));
        assert_eq!(AppLink::parse("pimble://"), Some(AppLink::Window));
        assert_eq!(AppLink::parse("pimble:nowhere"), None);
        assert_eq!(AppLink::parse("pimble:settings/plugins/more"), None);
        assert_eq!(AppLink::parse("https://pimble.app"), None);
        assert!(matches!(
            AppLink::parse("pimble:6f1c2b1e-8d3a-4f63-9a54-2c0f5a1e7b90/0b7e7c4d-1f2a-4c1b-8e3d-5a6b7c8d9e0f"),
            Some(AppLink::Node(_))
        ));
    }
}
