//! The keyboard shortcuts a person may change, in Settings > Keyboard
//! (docs/SPLIT_CONTRACT.md "Keys"). Each menu item that carries one names it
//! here by id; a changed key is saved (`state.json` `shortcuts`, `localStorage`
//! `pimble.shortcuts` in the browser) and the menus take it when they are next
//! built, at the next start.

use std::collections::HashMap;

/// One shortcut: its id, the menu item it runs, and its key unless changed.
#[derive(Debug, PartialEq, Eq)]
pub struct Binding {
    pub id: &'static str,
    pub label: &'static str,
    pub default: &'static str,
}

pub const SPLIT: &str = "split";
pub const JOIN: &str = "join";
pub const SEARCH: &str = "search";
pub const SETTINGS: &str = "settings";
pub const NEW_STORE: &str = "new_store";
pub const OPEN_STORE: &str = "open_store";

/// Every shortcut that can be changed, as Settings lists them.
pub const BINDINGS: &[Binding] = &[
    // Scrivener's keys for these two.
    Binding { id: SPLIT, label: "Split Note at Caret", default: "Ctrl+K" },
    Binding { id: JOIN, label: "Join Note Below", default: "Ctrl+M" },
    Binding { id: SEARCH, label: "Focus Search", default: "Ctrl+Shift+F" },
    Binding { id: SETTINGS, label: "Settings...", default: "Ctrl+," },
    Binding { id: NEW_STORE, label: "New Store...", default: "Ctrl+N" },
    Binding { id: OPEN_STORE, label: "Open Store...", default: "Ctrl+O" },
];

fn binding(id: &str) -> Option<&'static Binding> {
    BINDINGS.iter().find(|b| b.id == id)
}

/// The keys chosen in place of the defaults, by id.
pub fn saved() -> HashMap<String, String> {
    crate::persistence::load_shortcuts()
}

/// The key `id` runs on now, as rinch spells it: the one chosen, else the
/// default. Kept for the menus' lifetime, which is the app's.
pub fn chord(id: &str) -> &'static str {
    let default = binding(id).map_or("", |b| b.default);
    match saved().get(id) {
        Some(chosen) if chosen != default => Box::leak(chosen.clone().into_boxed_str()),
        _ => default,
    }
}

/// Choose `key` for `id`, or its default again with `None`. Answers the key
/// as it is saved, or why it cannot be.
pub fn choose(id: &str, key: Option<&str>) -> Result<Option<String>, String> {
    let mut all = saved();
    let Some(key) = key else {
        all.remove(id);
        crate::persistence::save_shortcuts(&all);
        return Ok(None);
    };
    let key = normalize(key)?;
    for other in BINDINGS.iter().filter(|b| b.id != id) {
        let theirs = all.get(other.id).map(String::as_str).unwrap_or(other.default);
        if theirs.eq_ignore_ascii_case(&key) {
            return Err(format!("{key} is already {}.", other.label));
        }
    }
    if binding(id).is_some_and(|b| b.default == key) {
        all.remove(id);
    } else {
        all.insert(id.to_string(), key.clone());
    }
    crate::persistence::save_shortcuts(&all);
    Ok(Some(key))
}

/// A typed shortcut ("ctrl + shift + k") as rinch spells one ("Ctrl+Shift+K"),
/// or why it is not one: at least one of Ctrl, Alt or Cmd, then one key (a
/// letter, a digit, F1 to F12, or one of `, . / ; ' [ ] - = \`` `).
pub fn normalize(typed: &str) -> Result<String, String> {
    let parts: Vec<&str> = typed.split('+').map(str::trim).collect();
    let (key, mods) = match parts.split_last() {
        Some((key, mods)) if !key.is_empty() => (*key, mods),
        // "Ctrl++" names the plus key, which no layout types without Shift.
        _ => return Err("Type the key after the modifiers, as Ctrl+Shift+K.".into()),
    };
    let mut out = Vec::new();
    for m in mods {
        let m = match m.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => "Ctrl",
            "shift" => "Shift",
            "alt" | "option" => "Alt",
            "cmd" | "command" | "meta" | "super" => "Cmd",
            _ => return Err(format!("{m} is not Ctrl, Shift, Alt or Cmd.")),
        };
        if !out.contains(&m) {
            out.push(m);
        }
    }
    if !out.iter().any(|m| matches!(*m, "Ctrl" | "Alt" | "Cmd")) {
        return Err("A shortcut needs Ctrl, Alt or Cmd, so typing never sets it off.".into());
    }
    let order = |m: &&str| ["Ctrl", "Alt", "Shift", "Cmd"].iter().position(|o| o == m);
    out.sort_by_key(order);
    let key = if key.len() == 1 && key.chars().all(|c| c.is_ascii_alphanumeric()) {
        key.to_ascii_uppercase()
    } else if key.len() == 1 && ",./;'[]-=\\`".contains(key) {
        key.to_string()
    } else if let Some(n) = key.strip_prefix(['F', 'f']).and_then(|n| n.parse::<u8>().ok()).filter(|n| (1..=12).contains(n)) {
        format!("F{n}")
    } else {
        return Err(format!("{key} is not a key Pimble can bind: a letter, a digit, F1 to F12, or punctuation."));
    };
    Ok(format!("{}+{key}", out.join("+")))
}

#[cfg(test)]
mod tests {
    use super::normalize;

    #[test]
    fn typed_shortcuts_are_spelled_as_rinch_reads_them() {
        assert_eq!(normalize("ctrl + shift + k").unwrap(), "Ctrl+Shift+K");
        assert_eq!(normalize("Shift+Ctrl+k").unwrap(), "Ctrl+Shift+K");
        assert_eq!(normalize("Alt+F5").unwrap(), "Alt+F5");
        assert_eq!(normalize("Ctrl+,").unwrap(), "Ctrl+,");
        assert!(normalize("Shift+K").unwrap_err().contains("needs Ctrl"));
        assert!(normalize("Ctrl+").is_err());
        assert!(normalize("Hyper+K").unwrap_err().contains("Hyper"));
        assert!(normalize("Ctrl+Space").unwrap_err().contains("Space"));
    }
}
