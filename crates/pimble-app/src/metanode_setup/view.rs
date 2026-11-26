//! The setup drawn in its pane: the sentences with their fields, where the
//! metanode finds nodes, their order and how many, and what it finds now.

use super::*;

fn option(value: &str, label: &str) -> SelectOption {
    SelectOption::new(value, label)
}

fn type_options() -> Vec<SelectOption> {
    TYPES.iter().map(|(v, l)| option(v, l)).collect()
}

fn op_options() -> Vec<SelectOption> {
    FIELD_OPS.iter().map(|(v, l)| option(v, l)).collect()
}

fn order_options() -> Vec<SelectOption> {
    vec![option("title", "Title"), option("modified", "Last changed"), option("created", "Created"), option("field", "A field")]
}

fn direction_options() -> Vec<SelectOption> {
    vec![option("asc", "Ascending"), option("desc", "Descending")]
}

/// "From"'s choices: this store, under a node, everywhere, and another store
/// when the source names one.
fn from_options(store: AppStore, pane: PaneId) -> Vec<SelectOption> {
    let ui = ui(store, pane);
    let mut options = vec![option("store", "This store"), option("under", "Under a node"), option("everywhere", "Everywhere")];
    if let Some(Within::Store(other)) = current(ui).within {
        let own = metanode_of(store, pane).map(|(s, _)| s);
        if own != Some(other) {
            let name = store.get_store_signal(other).map(|sig| untracked(|| sig.with(|s| s.name.clone()))).unwrap_or_else(|| "another store".to_string());
            options.push(option(&format!("store:{other}"), &format!("In {name}")));
        }
    }
    options
}

fn placeholder_of(kind: Option<Kind>) -> &'static str {
    if kind == Some(Kind::Text) {
        "words"
    } else {
        "a tag"
    }
}

fn shown(on: bool) -> &'static str {
    if on {
        ""
    } else {
        "display: none;"
    }
}

/// The setup a pane shows for a query metanode. Rendered once per pane, shown
/// while the pane holds one (`MetanodeUi::setup`).
pub(crate) fn render_setup(__scope: &mut RenderScope, store: AppStore, pane: PaneId) -> NodeHandle {
    let state = store.pane(pane);
    let mui = state.metanode_ui;
    let ui = mui.setup_ui;
    let own_store = move || metanode_of(store, pane).map(|(s, _)| s);
    let found_drawn = move || {
        let dark = store.dark_mode.get();
        ui.found_rows.get().into_iter().map(|row| {
            let color = row.color.as_deref().map(|c| format!("color: {};", display_color(c, dark))).unwrap_or_default();
            (row, color.clone(), color)
        }).collect::<Vec<_>>()
    };
    rsx! {
        div {
            class: "pimble-setup",
            style: {move || shown(state.show_metanode.get() && mui.setup.get())},

            div { class: "pimble-setup__heading", "Show nodes" }
            for line in lines(ui) {
                div {
                    key: line.key(),
                    class: "pimble-setup__line",
                    span { class: "pimble-setup__words", {line.kind.map_or("with a condition this Pimble doesn't know", Kind::words)} }
                    // tagged, not tagged, containing: one field.
                    div {
                        class: "pimble-setup__field",
                        style: shown(matches!(line.kind, Some(Kind::Tag | Kind::NoTag | Kind::Text))),
                        TextInput {
                            size: "xs",
                            placeholder: placeholder_of(line.kind),
                            value_fn: {
                                let i = line.index;
                                move || condition_at(ui, i).map(|c| text_of(&c)).unwrap_or_default()
                            },
                            oninput: {
                                let i = line.index;
                                move |v: String| change(store, pane, TYPING_MS, move |p| {
                                    if let Some(c) = p.all.get_mut(i) {
                                        *c = with_text(c, v);
                                    }
                                })
                            },
                        }
                    }
                    // with [field] [op] [value]
                    div {
                        class: "pimble-setup__field",
                        style: shown(line.kind == Some(Kind::Field)),
                        TextInput {
                            size: "xs",
                            placeholder: "a field",
                            value_fn: {
                                let i = line.index;
                                move || match condition_at(ui, i) { Some(Condition::Field { field, .. }) => field, _ => String::new() }
                            },
                            oninput: {
                                let i = line.index;
                                move |v: String| change(store, pane, TYPING_MS, move |p| {
                                    if let Some(Condition::Field { field, .. }) = p.all.get_mut(i) {
                                        *field = v;
                                    }
                                })
                            },
                        }
                    }
                    div {
                        class: "pimble-setup__field",
                        style: shown(line.kind == Some(Kind::Field)),
                        Select {
                            size: "xs",
                            value_fn: {
                                let i = line.index;
                                move || match condition_at(ui, i) { Some(Condition::Field { is, .. }) => op_key(is).to_string(), _ => "=".to_string() }
                            },
                            onchange: {
                                let i = line.index;
                                move |v: String| change(store, pane, 0, move |p| {
                                    if let Some(Condition::Field { is, .. }) = p.all.get_mut(i) {
                                        *is = op_of(&v);
                                    }
                                })
                            },
                            data: op_options(),
                        }
                    }
                    div {
                        class: "pimble-setup__field",
                        style: {
                            let i = line.index;
                            let field = line.kind == Some(Kind::Field);
                            move || shown(field && !matches!(condition_at(ui, i), Some(Condition::Field { is: FieldOp::Set | FieldOp::Empty, .. })))
                        },
                        TextInput {
                            size: "xs",
                            placeholder: "a value",
                            value_fn: {
                                let i = line.index;
                                move || match condition_at(ui, i) { Some(Condition::Field { value, .. }) => value.unwrap_or_default(), _ => String::new() }
                            },
                            oninput: {
                                let i = line.index;
                                move |v: String| change(store, pane, TYPING_MS, move |p| {
                                    if let Some(Condition::Field { value, .. }) = p.all.get_mut(i) {
                                        *value = Some(v);
                                    }
                                })
                            },
                        }
                    }
                    // linking to [a node]
                    span {
                        class: "pimble-setup__node",
                        style: shown(line.kind == Some(Kind::LinksTo)),
                        {move || linked_name(store, ui, line.index)}
                    }
                    div {
                        style: shown(line.kind == Some(Kind::LinksTo)),
                        Button {
                            variant: "default",
                            size: "xs",
                            onclick: {
                                let i = line.index;
                                move || open_picker(store, pane, PickFor::Condition(i))
                            },
                            "Pick a node..."
                        }
                    }
                    // that are [kind]
                    div {
                        class: "pimble-setup__field",
                        style: shown(line.kind == Some(Kind::Type)),
                        Select {
                            size: "xs",
                            value_fn: {
                                let i = line.index;
                                move || match condition_at(ui, i) { Some(Condition::Type { node_type }) => node_type, _ => String::new() }
                            },
                            onchange: {
                                let i = line.index;
                                move |v: String| change(store, pane, 0, move |p| {
                                    if let Some(c) = p.all.get_mut(i) {
                                        *c = Condition::Type { node_type: v };
                                    }
                                })
                            },
                            data: type_options(),
                        }
                    }
                    div {
                        class: "pimble-setup__remove",
                        title: "Remove this condition",
                        ActionIcon {
                            icon: TablerIcon::X,
                            variant: "subtle",
                            size: "xs",
                            onclick: {
                                let i = line.index;
                                move || change(store, pane, 0, move |p| {
                                    if i < p.all.len() {
                                        p.all.remove(i);
                                    }
                                })
                            },
                        }
                    }
                }
            }
            div {
                class: "pimble-setup__add",
                DropdownMenu {
                    opened_fn: move || ui.adding.get(),
                    on_close: move || ui.adding.set(false),
                    close_on_click_outside: true,
                    close_on_item_click: true,
                    position: "bottom-start",
                    DropdownMenuTarget {
                        div {
                            style: "display: inline-flex;",
                            Button {
                                variant: "subtle",
                                size: "xs",
                                onclick: move || ui.adding.set(!untracked(|| ui.adding.get())),
                                "+ Add a condition"
                            }
                        }
                    }
                    DropdownMenuDropdown {
                        for kind in Kind::ALL {
                            DropdownMenuItem {
                                key: kind.menu_label(),
                                onclick: move || {
                                    ui.adding.set(false);
                                    change(store, pane, 0, move |p| p.all.push(kind.blank()));
                                },
                                {kind.menu_label()}
                            }
                        }
                    }
                }
            }

            div { class: "pimble-setup__heading", "From" }
            div {
                class: "pimble-setup__line",
                div {
                    class: "pimble-setup__field",
                    Select {
                        size: "xs",
                        value_fn: move || own_store().map(|own| from_key(&current(ui).within, own)).unwrap_or_default(),
                        onchange: move |v: String| change(store, pane, 0, move |p| p.within = within_of(&v, &p.within)),
                        data: {move || from_options(store, pane)},
                    }
                }
                span {
                    class: "pimble-setup__node",
                    style: {move || shown(matches!(current(ui).within, Some(Within::Under(_))))},
                    {move || match current(ui).within { Some(Within::Under(n)) => node_name(store, &n), _ => String::new() }}
                }
                div {
                    style: {move || shown(matches!(current(ui).within, Some(Within::Under(_))))},
                    Button {
                        variant: "default",
                        size: "xs",
                        onclick: move || open_picker(store, pane, PickFor::Under),
                        "Pick a node..."
                    }
                }
            }

            // The node picker, under whatever asked for it.
            div {
                class: "pimble-setup__picker",
                style: {move || shown(ui.picking.get().is_some())},
                div {
                    class: "pimble-setup__line",
                    div {
                        class: "pimble-setup__field pimble-setup__field--wide",
                        TextInput {
                            size: "xs",
                            placeholder: "Type to find a node",
                            value_fn: move || ui.pick_query.get(),
                            oninput: move |v: String| set_pick_query(store, pane, v),
                        }
                    }
                    Button { variant: "subtle", size: "xs", onclick: move || ui.picking.set(None), "Cancel" }
                }
                for row in ui.picks.get() {
                    div {
                        key: format!("{}/{}", row.store_id, row.node_id),
                        class: "pimble-metanode__row",
                        onclick: {
                            let row = row.clone();
                            move || pick(store, pane, &row)
                        },
                        span { class: "pimble-metanode__label", {row.title.clone()} }
                        span { class: "pimble-setup__dim", {row.store_name.clone()} }
                    }
                }
            }

            div { class: "pimble-setup__heading", "Order" }
            div {
                class: "pimble-setup__line",
                span { class: "pimble-setup__words", "by" }
                div {
                    class: "pimble-setup__field",
                    Select {
                        size: "xs",
                        value_fn: move || order_key(&current(ui).order).to_string(),
                        onchange: move |v: String| change(store, pane, 0, move |p| p.order = order_of(&v, &p.order)),
                        data: order_options(),
                    }
                }
                div {
                    class: "pimble-setup__field",
                    style: {move || shown(matches!(current(ui).order, Order::Field(_)))},
                    TextInput {
                        size: "xs",
                        placeholder: "a field",
                        value_fn: move || match current(ui).order { Order::Field(f) => f, _ => String::new() },
                        oninput: move |v: String| change(store, pane, TYPING_MS, move |p| p.order = Order::Field(v)),
                    }
                }
                div {
                    class: "pimble-setup__field",
                    Select {
                        size: "xs",
                        value_fn: move || if current(ui).descending { "desc".to_string() } else { "asc".to_string() },
                        onchange: move |v: String| change(store, pane, 0, move |p| p.descending = v == "desc"),
                        data: direction_options(),
                    }
                }
                span { class: "pimble-setup__words", "at most" }
                div {
                    class: "pimble-setup__field pimble-setup__field--narrow",
                    TextInput {
                        size: "xs",
                        value_fn: move || current(ui).limit.to_string(),
                        oninput: move |v: String| change(store, pane, TYPING_MS, move |p| p.limit = limit_of(&v, p.limit)),
                    }
                }
            }

            div {
                class: "pimble-setup__heading",
                {move || count_line(ui.found_count.get(), &current(ui).all)}
            }
            for item in found_drawn() {
                div {
                    key: format!("{}/{} {} {} {}", item.0.store_id, item.0.node_id, item.0.label, item.0.icon, item.1),
                    class: "pimble-metanode__row pimble-setup__found",
                    onclick: {
                        let (store_id, node_id) = (item.0.store_id, item.0.node_id);
                        move || store.link_open.set(Some(LinkOpen {
                            value: format!("node_{store_id}_{node_id}"),
                            store_id,
                            node_id,
                            anchor: None,
                            pane: Some(pane),
                        }))
                    },
                    span {
                        class: "pimble-metanode__icon",
                        style: item.1.clone(),
                        IconGlyph { name: item.0.icon.clone() }
                    }
                    span {
                        class: "pimble-metanode__label",
                        style: item.2.clone(),
                        {item.0.label.clone()}
                    }
                }
            }
            div {
                class: "pimble-setup__dim pimble-setup__more",
                style: {move || shown(ui.found_count.get() > FIRST_ROWS)},
                {move || format!("and {} more, in the explorer under this metanode", ui.found_count.get().saturating_sub(FIRST_ROWS))}
            }
        }
    }
}

