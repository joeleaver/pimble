//! The panel's fields: each of the node's fields edited in place, a new
//! field's name and value boxes, and Edit > "Add Field". Every write is
//! `fields::write`.

use std::cell::RefCell;

use rinch::prelude::*;

use super::{panel_node, panel_writable, shown};
use crate::state::AppStore;

thread_local! {
    /// The new field's name box, to give it the keyboard (Edit > "Add Field").
    static NEW_FIELD_BOX: RefCell<Option<(u64, u64)>> = const { RefCell::new(None) };
}

/// Edit > "Add Field": the panel opens with a new field's name box taking
/// the keyboard.
pub fn add_field(store: AppStore) {
    let Some((s, n)) = untracked(|| shown(store)) else {
        crate::events::show_notice(store, "Open a note to give it a field.".to_string());
        return;
    };
    if !store.node_access(s, n).allows_write() {
        crate::events::show_notice(store, store.node_access(s, n).refusal().to_string());
        return;
    }
    store.properties_open.set(true);
    store.properties_adding_field.set(true);
    focus_new_field();
}

/// Give the new field's name box the keyboard once it shows.
fn focus_new_field() {
    rinch::core::reactive::unowned(|| {
        set_timeout(0, move || {
            if let Some((doc_key, node)) = NEW_FIELD_BOX.with(|b| *b.borrow()) {
                rinch::core::request_focus(doc_key, node);
            }
        })
    });
    // The browser owns its own focus: the box itself, once laid out.
    #[cfg(not(feature = "native"))]
    {
        use wasm_bindgen::JsCast;
        rinch::core::reactive::unowned(|| {
            set_timeout(40, move || {
                let Some(document) = web_sys::window().and_then(|w| w.document()) else { return };
                let Ok(Some(el)) = document.query_selector("input.pimble-properties__new-name") else { return };
                if let Ok(el) = el.dyn_into::<web_sys::HtmlElement>() {
                    let _ = el.focus();
                }
            })
        });
    }
}

/// The panel's "Fields" section.
pub(super) fn render_fields(__scope: &mut RenderScope, store: AppStore) -> NodeHandle {
    let node = move || panel_node(store);
    let writable = move || panel_writable(store);
    let new_name = Signal::new(String::new());
    let new_value = Signal::new(String::new());
    let add = move || {
        let Some((s, n)) = untracked(|| shown(store)) else { return };
        let (name, value) = untracked(|| (new_name.get(), new_value.get()));
        if name.trim().is_empty() {
            return;
        }
        crate::fields::write(store, s, n, &name, Some(value.trim()));
        new_name.set(String::new());
        new_value.set(String::new());
        store.properties_adding_field.set(false);
    };
    let cancel = move || {
        new_name.set(String::new());
        new_value.set(String::new());
        store.properties_adding_field.set(false);
    };
    let new_name_box = rsx! {
        input {
            class: "pimble-properties__input pimble-properties__new-name",
            placeholder: "name",
            value: move || new_name.get(),
            oninput: move |v: String| new_name.set(v),
            onsubmit: add,
            onkeydown: move |k: &rinch::core::KeyEventData| {
                if k.key == "Escape" {
                    cancel();
                    return true;
                }
                false
            },
        }
    };
    NEW_FIELD_BOX.with(|b| *b.borrow_mut() = Some((new_name_box.doc_key(), new_name_box.node_id().0)));
    rsx! {
        div {
            class: "pimble-properties__section",
            div { class: "pimble-properties__heading", "Fields" }
            for (name, value) in node().map(|n| n.fields).unwrap_or_default() {
                div {
                    // Drawn again when a value changes anywhere.
                    key: format!("{name}={value}"),
                    class: "pimble-properties__field",
                    {field_row(__scope, store, name.clone(), value.clone(), writable)}
                }
            }
            div {
                class: "pimble-properties__empty",
                style: {move || if !writable() && node().is_none_or(|n| n.fields.is_empty()) { "" } else { "display: none;" }},
                "No fields"
            }
            div {
                class: "pimble-properties__field pimble-properties__field--new",
                style: {move || if writable() && store.properties_adding_field.get() { "" } else { "display: none;" }},
                {new_name_box}
                input {
                    class: "pimble-properties__input",
                    placeholder: "value",
                    value: move || new_value.get(),
                    oninput: move |v: String| new_value.set(v),
                    onsubmit: add,
                    onkeydown: move |k: &rinch::core::KeyEventData| {
                        if k.key == "Escape" {
                            cancel();
                            return true;
                        }
                        false
                    },
                }
                span { class: "pimble-properties__remove", title: "Cancel", onclick: cancel, "×" }
            }
            div {
                class: "pimble-properties__add",
                style: {move || if writable() && !store.properties_adding_field.get() { "" } else { "display: none;" }},
                onclick: move || {
                    store.properties_adding_field.set(true);
                    focus_new_field();
                },
                "+ Add field"
            }
        }
    }
}

/// One field's row: its name and value, each edited in place (Enter or
/// leaving the box writes it), and a remove button; text only on a node
/// this device may only read.
fn field_row(__scope: &mut RenderScope, store: AppStore, name: String, value: String, writable: impl Fn() -> bool + Copy + 'static) -> NodeHandle {
    let name_text = Signal::new(name.clone());
    let value_text = Signal::new(value.clone());
    let rename = {
        let old = name.clone();
        move |typed: String| {
            let Some((s, n)) = untracked(|| shown(store)) else { return };
            let typed = typed.trim().to_string();
            // A name emptied, or the same name, changes nothing.
            if typed.is_empty() || typed.eq_ignore_ascii_case(&old) {
                name_text.set(old.clone());
                return;
            }
            let value = untracked(|| value_text.get());
            crate::fields::write(store, s, n, &typed, Some(value.trim()));
            crate::fields::write(store, s, n, &old, None);
        }
    };
    let set_value = {
        let name = name.clone();
        move |typed: String| {
            if let Some((s, n)) = untracked(|| shown(store)) {
                crate::fields::write(store, s, n, &name, Some(typed.trim()));
            }
        }
    };
    let remove = {
        let name = name.clone();
        move || {
            if let Some((s, n)) = untracked(|| shown(store)) {
                crate::fields::write(store, s, n, &name, None);
            }
        }
    };
    rsx! {
        div {
            class: "pimble-properties__field-inner",
            div {
                class: "pimble-properties__field-edit",
                style: {move || if writable() { "" } else { "display: none;" }},
                input {
                    class: "pimble-properties__input pimble-properties__field-name",
                    value: move || name_text.get(),
                    oninput: move |v: String| name_text.set(v),
                    onchange: rename,
                }
                input {
                    class: "pimble-properties__input",
                    value: move || value_text.get(),
                    oninput: move |v: String| value_text.set(v),
                    onchange: set_value,
                }
                span { class: "pimble-properties__remove", title: "Remove this field", onclick: remove, "×" }
            }
            div {
                class: "pimble-properties__field-read",
                style: {move || if writable() { "display: none;" } else { "" }},
                span { class: "pimble-properties__field-name-text", {name.clone()} }
                span { {value.clone()} }
            }
        }
    }
}
