//! The layers panel (docs/DRAWING_CONTRACT.md section 1, "Layer groups", and
//! section 4): the board's layers as a tree, top first, a group right before
//! what it holds, indented by depth. Groups fold and unfold (each viewer's
//! own state, never the document's); each row has its visibility, lock, kind,
//! name (renamed in place) and element count; the picked layer's blend mode
//! and opacity are below the list; rows drag between, into and out of groups.
//!
//! Flatten Layer, and Merge Down or Merge Group where the result is pixels,
//! go through the blob flow (docs/DRAWING_CONTRACT.md, "Built in wave 5
//! (model)" and "(UI)"): the layer is drawn as a picture (off the UI thread
//! on the desktop), the picture stored as a blob of the node's store
//! (`PutBlob`), and one edit lays it down once its URL comes back. The
//! panel says so meanwhile and takes no other such job.
//!
//! The judgements are pure and tested here: which rows show
//! ([`visible_rows`]), where a drop lands ([`zone_at`], [`drop_move`]).

use super::*;
use crate::appearance::IconGlyph;
use pimble_board::{Blend, LayerKind, LayerRaster, RasterJob};

/// Where a dragged layer row lands on the row under it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropZone {
    Above,
    /// Inside the group (on top of what it holds).
    Into,
    Below,
}

/// The panel's rows from the snapshot: every layer entry top first (the
/// snapshot's draw order reversed puts a group right before its layers).
pub(super) fn rows_of(snapshot: &BoardSnapshot) -> Vec<LayerRow> {
    let mut rows: Vec<LayerRow> = snapshot
        .layers
        .iter()
        .rev()
        .map(|l| LayerRow {
            id: l.id.clone(),
            name: l.name.clone(),
            visible: l.visible,
            locked: l.locked,
            opacity: l.opacity,
            count: l.count,
            kind: l.kind.clone(),
            blend: l.blend,
            parent: l.parent.clone(),
            depth: l.depth,
            shown: l.effective_visible,
            held: l.effective_locked,
            children: 0,
        })
        .collect();
    count_children(&mut rows);
    rows
}

/// Fill each row's `children`: the rows naming it as their parent.
pub(super) fn count_children(rows: &mut [LayerRow]) {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for r in rows.iter() {
        if let Some(p) = &r.parent {
            *counts.entry(p.clone()).or_default() += 1;
        }
    }
    for r in rows.iter_mut() {
        r.children = counts.get(&r.id).copied().unwrap_or(0);
    }
}

/// Whether `id` is `ancestor` or inside it.
fn within(rows: &[LayerRow], id: &str, ancestor: &str) -> bool {
    let parent_of: HashMap<&str, Option<&str>> = rows.iter().map(|r| (r.id.as_str(), r.parent.as_deref())).collect();
    let mut at = Some(id);
    let mut steps = 0;
    while let Some(cur) = at {
        if cur == ancestor {
            return true;
        }
        steps += 1;
        if steps > rows.len() {
            return false;
        }
        at = parent_of.get(cur).copied().flatten();
    }
    false
}

/// The rows the panel shows: every row but those inside a folded group.
pub(super) fn visible_rows(rows: &[LayerRow], folded: &[String]) -> Vec<LayerRow> {
    rows.iter()
        .filter(|r| {
            let mut at = r.parent.as_deref();
            let mut steps = 0;
            while let Some(p) = at {
                if folded.iter().any(|f| f == p) {
                    return false;
                }
                steps += 1;
                if steps > rows.len() {
                    break;
                }
                at = rows.iter().find(|x| x.id == p).and_then(|x| x.parent.as_deref());
            }
            true
        })
        .cloned()
        .collect()
}

/// Where a drop at `fraction` (0 at the row's top, 1 at its bottom) of a
/// row lands: a group's middle takes the layer in, any other row's halves
/// put it above or below.
pub(super) fn zone_at(fraction: f32, group: bool) -> DropZone {
    if group {
        if fraction < 0.3 {
            DropZone::Above
        } else if fraction > 0.7 {
            DropZone::Below
        } else {
            DropZone::Into
        }
    } else if fraction < 0.5 {
        DropZone::Above
    } else {
        DropZone::Below
    }
}

/// The move a drop makes: the group the dragged layer goes into (`None`:
/// top level) and its place there, for `BoardTxn::move_layer_into`. `None`
/// when the drop goes nowhere: onto itself, or into itself or a group it
/// holds. Below an unfolded group that holds layers is the top of that
/// group, since its first layer is the row shown just below it.
pub(super) fn drop_move(rows: &[LayerRow], folded: &[String], dragged: &str, target: &str, zone: DropZone) -> Option<(Option<String>, Placement)> {
    if dragged == target || within(rows, target, dragged) {
        return None;
    }
    let t = rows.iter().find(|r| r.id == target)?;
    rows.iter().find(|r| r.id == dragged)?;
    let open_group = t.is_group() && t.children > 0 && !folded.iter().any(|f| f == target);
    Some(match zone {
        DropZone::Above => (t.parent.clone(), Placement::Above(target.to_string())),
        DropZone::Into => (Some(target.to_string()), Placement::Top),
        DropZone::Below if open_group => (Some(target.to_string()), Placement::Top),
        DropZone::Below => (t.parent.clone(), Placement::Below(target.to_string())),
    })
}

/// The picked layers for Group Layers, top first: the extra picks and the
/// picked one, in the panel's order.
pub(super) fn picked_for_group(rows: &[LayerRow], picked: Option<&str>, extra: &[String]) -> Vec<String> {
    rows.iter()
        .filter(|r| picked == Some(r.id.as_str()) || extra.iter().any(|e| e == &r.id))
        .map(|r| r.id.clone())
        .collect()
}

/// A layer edit whose refusal is said (the model's sentence), not only
/// logged as `local_edit` does with refusals that are not locks.
fn layer_edit<R>(store: AppStore, pane: PaneId, f: impl FnOnce(&mut BoardTxn<'_, '_>) -> pimble_board::doc::Result<R>) -> Option<R> {
    let mut refused: Option<String> = None;
    new_undo_step(pane);
    let result = local_edit(store, pane, |t| {
        f(t).map_err(|e| {
            if !e.is_lock() {
                refused = Some(e.to_string());
            }
            e
        })
    });
    if let Some(sentence) = refused {
        say(store, sentence);
    }
    result
}

/// Show a refusal as a sentence: a capital first, a full stop last.
fn say(store: AppStore, mut sentence: String) {
    if let Some(first) = sentence.get(0..1) {
        sentence = format!("{}{}", first.to_uppercase(), &sentence[1..]);
    }
    if !sentence.ends_with('.') {
        sentence.push('.');
    }
    crate::events::show_notice(store, sentence);
}

// ── Flatten, and merges as pixels ───────────────────────────────────────────

/// `PutBlob` request ids of flattened layers start here, clear of the
/// pictures' (`pictures.rs`, and image nodes' from `1 << 40`).
const RASTER_BASE: u64 = 1 << 41;

/// A drawn layer whose pictures (one per tile) are being stored. Its tiles'
/// `PutBlob` request ids are `first .. first + tiles`.
struct PendingRaster {
    pane: PaneId,
    held: (StoreId, NodeId),
    raster: LayerRaster,
    first: u64,
    /// Each tile's URL once stored.
    srcs: Vec<Option<String>>,
    /// A single tile's picture decoded already, so this window shows it at
    /// once (a grid of tiles is fetched as any picture is: decoding every
    /// tile here would hold them all at once).
    picture: Option<Arc<Picture>>,
}

thread_local! {
    static RASTERS: RefCell<Vec<PendingRaster>> = const { RefCell::new(Vec::new()) };
    static NEXT_RASTER: Cell<u64> = const { Cell::new(RASTER_BASE) };
}

/// What the panel says while `job` is under way.
pub fn busy_words(job: &RasterJob) -> &'static str {
    match job {
        RasterJob::Flatten(_) => "Flattening...",
        RasterJob::MergeDown(_) | RasterJob::MergeGroup(_) => "Merging...",
    }
}

/// Flatten Layer, or a merge whose result is pixels: draw it (off the UI
/// thread on the desktop), tile by tile, then store each tile's picture and
/// lay them all down in one edit ([`raster_stored`]). Refusals are said as
/// sentences (a hidden layer, a picture not here yet, too large).
pub(super) fn rasterize_layer(store: AppStore, pane: PaneId, job: RasterJob) {
    let ui = store.pane(pane).board_ui;
    if untracked(|| ui.rasterizing.get()).is_some() {
        crate::events::show_notice(store, "A flatten or merge is still being stored; try again in a moment.".to_string());
        return;
    }
    if !writable_or_say(store, pane) {
        return;
    }
    let Some((held, snapshot, pictures)) = with_session(pane, |s| {
        let (pictures, _) = pictures_of(s.snapshot());
        ((s.store_id, s.node_id), s.board.snapshot().with_paint_cache(super::paint::shared_cache()), pictures)
    }) else {
        return;
    };
    ui.rasterizing.set(Some(busy_words(&job)));
    let work = move || -> Result<(LayerRaster, Option<Arc<Picture>>), String> {
        let raster = pimble_board::rasterize(&snapshot, &pictures, job).map_err(|e| e.to_string())?;
        let picture = match raster.tiles.as_slice() {
            [one] => Picture::decode(&one.png).ok().map(Arc::new),
            _ => None,
        };
        Ok((raster, picture))
    };
    #[cfg(all(feature = "native", not(test)))]
    std::thread::spawn(move || {
        let result = work();
        run_on_main_thread(move || rasterized(store, pane, held, result));
    });
    #[cfg(any(not(feature = "native"), test))]
    rasterized(store, pane, held, work());
}

/// The layer was drawn: its tiles go to be stored, nothing drawn is laid
/// down at once.
fn rasterized(store: AppStore, pane: PaneId, held: (StoreId, NodeId), result: Result<(LayerRaster, Option<Arc<Picture>>), String>) {
    let ui = store.pane(pane).board_ui;
    let (raster, picture) = match result {
        Ok(drawn) => drawn,
        Err(sentence) => {
            ui.rasterizing.set(None);
            say(store, sentence);
            return;
        }
    };
    if with_session(pane, |s| (s.store_id, s.node_id)) != Some(held) {
        ui.rasterizing.set(None);
        return;
    }
    if raster.tiles.is_empty() {
        ui.rasterizing.set(None);
        lay_down(store, pane, &raster, &[]);
        return;
    }
    let tiles = raster.tiles.len() as u64;
    let first = NEXT_RASTER.with(|n| n.replace(n.get() + tiles));
    let pngs: Vec<Vec<u8>> = raster.tiles.iter().map(|t| t.png.clone()).collect();
    RASTERS.with(|r| r.borrow_mut().push(PendingRaster { pane, held, srcs: vec![None; pngs.len()], raster, first, picture }));
    for (i, png) in pngs.into_iter().enumerate() {
        store.send(BackendCommand::PutBlob { store_id: held.0, node_id: held.1, bytes: png, request_id: first + i as u64 });
    }
}

/// The answer to a flattened layer's tile's `PutBlob`; answers whether it
/// was one. Once every tile is stored the pixels are laid down in one edit
/// (one undo step); a tile that could not be stored ends the job with its
/// sentence and nothing written. This window's picture cache gets a single
/// tile's picture already decoded when it was stored as it was drawn.
pub(super) fn raster_stored(store: AppStore, request_id: u64, result: &Result<StoredBlob, String>) -> bool {
    let at = RASTERS.with(|r| r.borrow().iter().position(|p| (p.first..p.first + p.srcs.len() as u64).contains(&request_id)));
    let Some(at) = at else { return false };
    let index = RASTERS.with(|r| (request_id - r.borrow()[at].first) as usize);
    let blob = match result {
        Err(sentence) => {
            let pending = RASTERS.with(|r| r.borrow_mut().remove(at));
            store.pane(pending.pane).board_ui.rasterizing.set(None);
            say(store, sentence.clone());
            return true;
        }
        Ok(blob) => blob,
    };
    let done = RASTERS.with(|r| {
        let mut r = r.borrow_mut();
        r[at].srcs[index] = Some(blob.url.to_string());
        r[at].srcs.iter().all(Option::is_some)
    });
    if !done {
        return true;
    }
    let PendingRaster { pane, held, raster, srcs, picture, .. } = RASTERS.with(|r| r.borrow_mut().remove(at));
    store.pane(pane).board_ui.rasterizing.set(None);
    if with_session(pane, |s| (s.store_id, s.node_id)) != Some(held) {
        crate::events::show_notice(store, "The canvas was closed before its flattened pictures were stored, so nothing changed.".to_string());
        return true;
    }
    let srcs: Vec<String> = srcs.into_iter().flatten().collect();
    if let (Some(picture), [tile], [src]) = (picture, raster.tiles.as_slice(), srcs.as_slice()) {
        if blob.size == Some(tile.pixel_size) {
            super::seed_picture(src, picture, tile.png.clone());
        }
    }
    lay_down(store, pane, &raster, &srcs);
    true
}

/// One edit laying the drawn pixels down (`apply_raster`): a Merge Down
/// picks the layer it went into.
fn lay_down(store: AppStore, pane: PaneId, raster: &LayerRaster, srcs: &[String]) {
    let laid = layer_edit(store, pane, |t| t.apply_raster(raster, srcs));
    if laid.is_some() && matches!(raster.job, RasterJob::MergeDown(_)) {
        store.pane(pane).board_ui.layer.set(raster.onto.clone());
    }
}

/// Merge Down (a group: Merge Group), as pixels when the result is pixels
/// (`merge_needs_pixels`), else as before.
pub(super) fn merge(store: AppStore, pane: PaneId, row: &LayerRow) {
    let pixels = with_session(pane, |s| pimble_board::merge_needs_pixels(s.snapshot(), &row.id)).unwrap_or(false);
    match (row.is_group(), pixels) {
        (true, true) => rasterize_layer(store, pane, RasterJob::MergeGroup(row.id.clone())),
        (false, true) => rasterize_layer(store, pane, RasterJob::MergeDown(row.id.clone())),
        (true, false) => {
            layer_edit(store, pane, |t| t.merge_group(&row.id));
        }
        (false, false) => {
            layer_edit(store, pane, |t| t.merge_layer_down(&row.id));
            store.pane(pane).board_ui.layer.set(None);
        }
    }
}

/// "Group Layers" (Ctrl+G while the layers panel has the focus): the picked
/// layers in a new group at the topmost one's place.
pub(super) fn group_picked(store: AppStore, pane: PaneId) {
    let ui = store.pane(pane).board_ui;
    let rows = untracked(|| ui.layers.get());
    let ids = picked_for_group(&rows, untracked(|| ui.layer.get()).as_deref(), &untracked(|| ui.picked_layers.get()));
    if ids.is_empty() {
        crate::events::show_notice(store, "Pick the layers to group.".to_string());
        return;
    }
    let name = format!("Group {}", rows.iter().filter(|r| r.is_group()).count() + 1);
    if let Some(gid) = layer_edit(store, pane, |t| t.group_layers(&ids, &name)) {
        ui.layer.set(Some(gid));
        ui.picked_layers.set(Vec::new());
    }
}

/// A small icon button for the panel.
fn small_button(__scope: &mut RenderScope, icon: TablerIcon, tip: &'static str, run: impl Fn() + 'static) -> NodeHandle {
    rsx! {
        div {
            title: tip,
            class: "pimble-board-tool pimble-board-tool--small",
            onclick: move || run(),
            IconGlyph { name: icon.name().to_string() }
        }
    }
}

/// The icon a layer's kind shows.
fn kind_icon(kind: &LayerKind, folded: bool) -> TablerIcon {
    match kind {
        LayerKind::Group if folded => TablerIcon::Folder,
        LayerKind::Group => TablerIcon::FolderOpen,
        LayerKind::Paint => TablerIcon::Brush,
        _ => TablerIcon::VectorBezier2,
    }
}

fn kind_name(kind: &LayerKind) -> &'static str {
    match kind {
        LayerKind::Group => "Group",
        LayerKind::Paint => "Paint layer",
        LayerKind::Vector => "Vector layer",
        LayerKind::Other(_) => "A layer this version does not know",
    }
}

/// The layers panel's body (its title row is the panel stack's).
pub(super) fn render_layers(__scope: &mut RenderScope, store: AppStore, pane: PaneId) -> NodeHandle {
    let ui = store.pane(pane).board_ui;
    let drag: DragContext<String> = DragContext::new();
    let layer_opacity = Signal::new(1.0f64);
    // The picked layer's opacity, shown on the slider.
    let _ = rinch::Effect::new(move || {
        let layers = ui.layers.get();
        let picked = ui.layer.get();
        let opacity = picked.and_then(|id| layers.iter().find(|l| l.id == id).map(|l| l.opacity)).unwrap_or(1.0);
        untracked(|| layer_opacity.set_if_changed(opacity as f64));
    });
    let picked = move || ui.layer.get().or_else(|| ui.layers.get().iter().find(|l| !l.held && !l.is_group()).map(|l| l.id.clone()));
    let picked_row = move || {
        let id = picked()?;
        ui.layers.get().into_iter().find(|l| l.id == id)
    };

    let new_layer = small_button(__scope, TablerIcon::Plus, "New Layer (above the picked one)", move || {
        let rows = untracked(|| ui.layers.get());
        let count = rows.iter().filter(|r| !r.is_group()).count();
        let above = untracked(|| ui.layer.get());
        let made = layer_edit(store, pane, |t| {
            let placement = match &above {
                Some(id) => Placement::Above(id.clone()),
                None => Placement::Top,
            };
            t.add_layer(&format!("Layer {}", count + 1), placement)
        });
        if let Some(id) = made {
            ui.layer.set(Some(id));
        }
    });
    let new_paint = small_button(__scope, TablerIcon::Brush, "New Paint Layer (above the picked one)", move || {
        let rows = untracked(|| ui.layers.get());
        let above = untracked(|| ui.layer.get());
        let parent = above.as_ref().and_then(|id| rows.iter().find(|r| &r.id == id)).and_then(|r| r.parent.clone());
        let name = (1..).map(|n| format!("Paint {n}")).find(|name| !rows.iter().any(|r| &r.name == name)).unwrap_or_default();
        let made = layer_edit(store, pane, |t| {
            let placement = match &above {
                Some(id) => Placement::Above(id.clone()),
                None => Placement::Top,
            };
            t.add_layer_in(&name, LayerKind::Paint, parent.as_deref(), placement)
        });
        if let Some(id) = made {
            ui.layer.set(Some(id));
        }
    });
    let new_group = small_button(__scope, TablerIcon::FolderPlus, "New Group (above the picked layer)", move || {
        let rows = untracked(|| ui.layers.get());
        let count = rows.iter().filter(|r| r.is_group()).count();
        let above = untracked(|| ui.layer.get());
        let parent = above.as_ref().and_then(|id| rows.iter().find(|r| &r.id == id)).and_then(|r| r.parent.clone());
        let made = layer_edit(store, pane, |t| {
            let placement = match &above {
                Some(id) => Placement::Above(id.clone()),
                None => Placement::Top,
            };
            t.add_group(&format!("Group {}", count + 1), parent.as_deref(), placement)
        });
        if let Some(id) = made {
            ui.layer.set(Some(id));
        }
    });
    let group_btn = small_button(__scope, TablerIcon::Folders, "Group Layers (Ctrl+G here): Ctrl+click rows to pick several", move || group_picked(store, pane));
    let ungroup_btn = small_button(__scope, TablerIcon::FolderMinus, "Ungroup (its layers move up to its place)", move || {
        let Some(row) = untracked(|| picked_row()) else { return };
        if !row.is_group() {
            crate::events::show_notice(store, "Pick a group to ungroup.".to_string());
            return;
        }
        layer_edit(store, pane, |t| t.ungroup(&row.id));
        ui.layer.set(None);
    });
    let delete_btn = small_button(__scope, TablerIcon::Trash, "Delete Layer (with its elements; a group with everything in it)", move || {
        let Some(id) = untracked(|| picked()) else { return };
        layer_edit(store, pane, |t| t.delete_layer(&id));
        ui.layer.set(None);
    });
    let merge_btn = small_button(__scope, TablerIcon::ArrowMerge, "Merge Down (into the layer below; a group into one layer)", move || {
        let Some(row) = untracked(|| picked_row()) else { return };
        merge(store, pane, &row);
    });
    // Flatten Layer: shown for a paint layer.
    let flatten_btn = rsx! {
        div {
            title: "Flatten Layer (its whole history as one picture)",
            class: "pimble-board-tool pimble-board-tool--small",
            style: {move || if picked_row().is_some_and(|r| r.kind == LayerKind::Paint) { "" } else { "display: none;" }},
            onclick: move || {
                let Some(row) = untracked(|| picked_row()) else { return };
                if row.kind != LayerKind::Paint {
                    crate::events::show_notice(store, "Pick a paint layer to flatten.".to_string());
                    return;
                }
                rasterize_layer(store, pane, RasterJob::Flatten(row.id.clone()));
            },
            IconGlyph { name: TablerIcon::Stack2.name().to_string() }
        }
    };
    let move_here = small_button(__scope, TablerIcon::TransferIn, "Move Selection to Layer (the picked one)", move || {
        let Some(id) = untracked(|| picked()) else { return };
        let ids = with_session(pane, |s| s.selected.clone()).unwrap_or_default();
        if ids.is_empty() {
            crate::events::show_notice(store, "Select something to move to this layer.".to_string());
            return;
        }
        layer_edit(store, pane, |t| t.move_to_layer(&ids, &id));
    });

    // The blend modes, a list that opens under the picked layer's.
    let blends: Vec<NodeHandle> = Blend::ALL
        .into_iter()
        .map(|blend| {
            rsx! {
                div {
                    class: {move || {
                        let current = picked_row().map(|r| r.blend);
                        let group = picked_row().is_some_and(|r| r.is_group());
                        match (current == Some(blend), blend == Blend::PassThrough && !group) {
                            (_, true) => "pimble-board-chip pimble-board-chip--hidden",
                            (true, _) => "pimble-board-chip pimble-board-chip--active",
                            _ => "pimble-board-chip",
                        }
                    }},
                    onclick: move || {
                        ui.popup.set(Popup::None);
                        let Some(id) = untracked(|| picked()) else { return };
                        layer_edit(store, pane, |t| t.set_layer_blend(&id, blend));
                    },
                    {blend.label()}
                }
            }
        })
        .collect();

    rsx! {
        div {
            class: "pimble-board__layers-body",
            onmousedown: move || {
                crate::pane_view::press_in_view();
                super::rail::keys_to(store, pane, super::rail::Keys::Layers);
            },
            div {
                class: "pimble-board__layers",
                for (row, v, class_id, thumb_id) in row_views(&ui.layers.get(), &ui.folded_groups.get()).into_iter().map(|(r, v)| { let c = r.id.clone(); let t = r.id.clone(); (r, v, c, t) }) {
                    div {
                        // A keyed row is drawn once: everything it shows is in its key.
                        key: format!(
                            "{} {} {} {} {} {} {} {} {} {} {} {} {}",
                            row.id, row.name, row.visible, row.locked, row.count, row.depth, row.shown, row.held, row.children,
                            row.kind, row.blend.as_str(),
                            ui.renaming.get().as_deref() == Some(row.id.as_str()),
                            ui.folded_groups.get().contains(&row.id),
                        ),
                        class: {|| row_class(ui, &class_id)},
                        style: format!("padding-left: {}px;", indent_px(row.depth)),
                        draggable: "true",
                        ondragstart: {
                            let id = row.id.clone();
                            move || drag.set(id.clone())
                        },
                        ondragend: move || {
                            drag.clear();
                            ui.drop_at.set(None);
                        },
                        ondragover: {
                            let id = row.id.clone();
                            let group = row.is_group();
                            move || {
                                let ctx = rinch::core::get_click_context();
                                let fraction = if ctx.element_height > 0.0 { (ctx.mouse_y - ctx.element_y) / ctx.element_height } else { 0.5 };
                                ui.drop_at.set_if_changed(Some((id.clone(), zone_at(fraction, group))));
                            }
                        },
                        ondragleave: {
                            let id = row.id.clone();
                            move || {
                                if untracked(|| ui.drop_at.get()).is_some_and(|(at, _)| at == id) {
                                    ui.drop_at.set(None);
                                }
                            }
                        },
                        ondrop: {
                            let target = row.id.clone();
                            let group = row.is_group();
                            move || {
                                let zone = untracked(|| ui.drop_at.get())
                                    .filter(|(at, _)| *at == target)
                                    .map(|(_, z)| z)
                                    .unwrap_or_else(|| zone_at(0.5, group));
                                ui.drop_at.set(None);
                                let Some(dragged) = drag.take() else { return };
                                let rows = untracked(|| ui.layers.get());
                                let folded = untracked(|| ui.folded_groups.get());
                                let Some((parent, placement)) = drop_move(&rows, &folded, &dragged, &target, zone) else { return };
                                layer_edit(store, pane, |t| t.move_layer_into(&dragged, parent.as_deref(), placement));
                            }
                        },
                        onclick: {
                            let id = row.id.clone();
                            move || pick_row(ui, &id)
                        },
                        if v.group {
                            div {
                                class: "pimble-board__layer-fold",
                                title: "Fold or unfold this group (only here)",
                                onclick: {
                                    let id = v.fold_id.clone();
                                    move || ui.folded_groups.update(|f| {
                                        if let Some(at) = f.iter().position(|x| *x == id) {
                                            f.remove(at);
                                        } else {
                                            f.push(id.clone());
                                        }
                                    })
                                },
                                if v.folded {
                                    IconGlyph { name: TablerIcon::ChevronRight.name().to_string() }
                                } else {
                                    IconGlyph { name: TablerIcon::ChevronDown.name().to_string() }
                                }
                            }
                        } else {
                            div { class: "pimble-board__layer-fold" }
                        }
                        div {
                            title: if row.visible { "Hide this layer, for everyone" } else { "Show this layer" },
                            class: if row.visible && !row.shown { "pimble-board__layer-icon pimble-board__layer-icon--inherited" } else { "pimble-board__layer-icon" },
                            onclick: {
                                let id = row.id.clone();
                                let visible = row.visible;
                                move || {
                                    layer_edit(store, pane, |t| t.set_layer_visible(&id, !visible));
                                }
                            },
                            IconGlyph { name: if row.visible { TablerIcon::Eye.name().to_string() } else { TablerIcon::EyeOff.name().to_string() } }
                        }
                        div {
                            title: if row.locked { "Unlock this layer" } else if row.held { "Locked by its group" } else { "Lock this layer against accidents" },
                            class: if !row.locked && row.held { "pimble-board__layer-icon pimble-board__layer-icon--inherited" } else { "pimble-board__layer-icon" },
                            onclick: {
                                let id = row.id.clone();
                                let locked = row.locked;
                                move || {
                                    layer_edit(store, pane, |t| t.set_layer_locked(&id, !locked));
                                }
                            },
                            IconGlyph { name: if row.locked || row.held { TablerIcon::Lock.name().to_string() } else { TablerIcon::LockOpen.name().to_string() } }
                        }
                        div {
                            class: "pimble-board__layer-icon pimble-board__layer-kind",
                            title: v.kind_title,
                            IconGlyph { name: v.kind_glyph }
                        }
                        // A thumbnail of what the layer holds (drawn again a
                        // moment after it changes, never per frame).
                        if !v.group {
                            img {
                                class: "pimble-board__layer-thumb",
                                src: move || ui.thumbs.get().get(&thumb_id).cloned().unwrap_or_else(super::paint::blank_thumb),
                            }
                        }
                        {name_cell(__scope, pane, ui, &row)}
                        span {
                            class: "pimble-board__layer-count",
                            title: v.count_title,
                            {v.count}
                        }
                    }
                }
            }
            div {
                class: "pimble-board__layer-props",
                style: {move || if picked_row().is_some() { "" } else { "display: none;" }},
                div {
                    class: "pimble-board__row",
                    div {
                        class: "pimble-board-chip pimble-board__blend",
                        title: "How this layer is laid over what is below it",
                        onclick: move || ui.popup.update(|p| *p = if *p == Popup::Blend { Popup::None } else { Popup::Blend }),
                        {move || format!("{} ▾", picked_row().map(|r| r.blend.label()).unwrap_or("Normal"))}
                    }
                    div { class: "pimble-board__label pimble-board__grow", {move || format!("{}%", (layer_opacity.get() * 100.0).round())} }
                }
                div {
                    class: "pimble-board__blends",
                    style: {move || if ui.popup.get() == Popup::Blend { "" } else { "display: none;" }},
                    {blends}
                }
                Slider {
                    min: 0.0,
                    max: 1.0,
                    step: 0.05,
                    size: "sm",
                    value_signal: Some(layer_opacity),
                    onchange: move |v: f64| {
                        let Some(id) = untracked(|| picked()) else { return };
                        layer_edit(store, pane, |t| t.set_layer_opacity(&id, v as f32));
                    },
                }
            }
            div {
                class: "pimble-board__row pimble-board__label",
                style: {move || if ui.rasterizing.get().is_some() { "" } else { "display: none;" }},
                {move || ui.rasterizing.get().unwrap_or("")}
            }
            div {
                class: "pimble-board__row pimble-board__row--wrap",
                style: {move || if ui.rasterizing.get().is_some() { "opacity: 0.45; pointer-events: none;" } else { "" }},
                {new_layer}
                {new_paint}
                {new_group}
                {group_btn}
                {ungroup_btn}
                {merge_btn}
                {flatten_btn}
                {move_here}
                {delete_btn}
            }
        }
    }
}

/// What a shown row draws, worked out before the row is built (each
/// reactive attribute of a keyed row takes its own value).
#[derive(Debug, Clone, PartialEq)]
struct RowView {
    fold_id: String,
    group: bool,
    folded: bool,
    kind_title: &'static str,
    kind_glyph: String,
    count_title: &'static str,
    /// A group's layers, a layer's elements.
    count: String,
}

/// A row's classes: picked, and where a dragged row would land on it.
fn row_class(ui: PaneUi, id: &str) -> String {
    let picked = ui.layer.get().as_deref() == Some(id);
    let extra = ui.picked_layers.get().iter().any(|p| p == id);
    let over = match ui.drop_at.get() {
        Some((at, DropZone::Above)) if at == id => " pimble-board__layer--drop-above",
        Some((at, DropZone::Into)) if at == id => " pimble-board__layer--drop-into",
        Some((at, DropZone::Below)) if at == id => " pimble-board__layer--drop-below",
        _ => "",
    };
    let base = if picked || extra { "pimble-board__layer pimble-board__layer--picked" } else { "pimble-board__layer" };
    format!("{base}{over}")
}

fn row_views(rows: &[LayerRow], folded: &[String]) -> Vec<(LayerRow, RowView)> {
    visible_rows(rows, folded)
        .into_iter()
        .map(|r| {
            let is_folded = folded.contains(&r.id);
            let v = RowView {
                fold_id: r.id.clone(),
                group: r.is_group(),
                folded: is_folded,
                kind_title: kind_name(&r.kind),
                kind_glyph: kind_icon(&r.kind, is_folded).name().to_string(),
                count_title: if r.is_group() { "Layers in this group" } else { "Elements in this layer" },
                count: if r.is_group() { r.children.to_string() } else { r.count.to_string() },
            };
            (r, v)
        })
        .collect()
}

/// A click on a row: Ctrl or Shift adds it to (or takes it from) the picks
/// Group Layers uses; a plain click picks it alone.
fn pick_row(ui: PaneUi, id: &str) {
    let mods = rinch::core::get_click_context().modifiers;
    if mods.ctrl || mods.meta || mods.shift {
        let current = untracked(|| ui.layer.get());
        if current.is_none() {
            ui.layer.set(Some(id.to_string()));
            return;
        }
        if current.as_deref() == Some(id) {
            return;
        }
        let id = id.to_string();
        ui.picked_layers.update(|p| {
            if let Some(at) = p.iter().position(|x| *x == id) {
                p.remove(at);
            } else {
                p.push(id);
            }
        });
    } else {
        ui.picked_layers.set(Vec::new());
        ui.layer.set(Some(id.to_string()));
    }
}

/// A row's name, or the input renaming it in place.
fn name_cell(__scope: &mut RenderScope, pane: PaneId, ui: PaneUi, row: &LayerRow) -> NodeHandle {
    if untracked(|| ui.renaming.get()).as_deref() == Some(row.id.as_str()) {
        let input = rsx! {
            input {
                r#type: "text",
                class: "pimble-board__layer-name-input",
                value: move || ui.rename_text.get(),
                oninput: move |v: String| ui.rename_text.set(v),
                onsubmit: move || super::ui::finish_rename(pane, ui, true),
            }
        };
        super::ui::rename_input_is(pane, &input);
        return input;
    }
    let id = row.id.clone();
    let name = row.name.clone();
    rsx! {
        span {
            class: "pimble-board__layer-name",
            title: "Click to pick this layer; click its name again to rename it",
            onclick: move || {
                let mods = rinch::core::get_click_context().modifiers;
                // The row's own click does not reach past this one.
                if !(mods.ctrl || mods.meta || mods.shift) && untracked(|| ui.layer.get()).as_deref() == Some(id.as_str()) {
                    super::ui::begin_rename(pane, ui, &id, &name);
                } else {
                    pick_row(ui, &id);
                }
            },
            {row.name.clone()}
        }
    }
}

/// A row's indent at `depth`, pixels: 14 a level for the first three, 6 a
/// level after, never more than 64, so a deep group's names keep their
/// room beside the icons and the thumbnail.
pub(super) fn indent_px(depth: usize) -> usize {
    let deep = depth.saturating_sub(3);
    (4 + depth.min(3) * 14 + deep * 6).min(64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deep_rows_indent_less_and_never_past_a_bound() {
        assert_eq!(indent_px(0), 4);
        assert_eq!(indent_px(1), 18);
        assert_eq!(indent_px(3), 46);
        assert_eq!(indent_px(4), 52);
        assert_eq!(indent_px(9), 64);
        assert_eq!(indent_px(30), 64);
    }

    fn row(id: &str, parent: Option<&str>, kind: LayerKind, depth: usize) -> LayerRow {
        LayerRow {
            id: id.to_string(),
            name: id.to_uppercase(),
            visible: true,
            locked: false,
            opacity: 1.0,
            count: 0,
            kind,
            blend: Blend::Normal,
            parent: parent.map(str::to_string),
            depth,
            shown: true,
            held: false,
            children: 0,
        }
    }

    /// g (group) holding h (group) holding a; then b at top level; c inside g.
    fn tree() -> Vec<LayerRow> {
        let mut rows = vec![
            row("g", None, LayerKind::Group, 0),
            row("h", Some("g"), LayerKind::Group, 1),
            row("a", Some("h"), LayerKind::Vector, 2),
            row("c", Some("g"), LayerKind::Vector, 1),
            row("b", None, LayerKind::Vector, 0),
            row("e", None, LayerKind::Group, 0),
        ];
        count_children(&mut rows);
        rows
    }

    fn ids(rows: &[LayerRow]) -> Vec<&str> {
        rows.iter().map(|r| r.id.as_str()).collect()
    }

    #[test]
    fn children_are_counted() {
        let rows = tree();
        let count = |id: &str| rows.iter().find(|r| r.id == id).unwrap().children;
        assert_eq!((count("g"), count("h"), count("a"), count("e")), (2, 1, 0, 0));
    }

    #[test]
    fn folded_groups_hide_everything_inside_them() {
        let rows = tree();
        assert_eq!(ids(&visible_rows(&rows, &[])), ["g", "h", "a", "c", "b", "e"]);
        assert_eq!(ids(&visible_rows(&rows, &["h".to_string()])), ["g", "h", "c", "b", "e"]);
        assert_eq!(ids(&visible_rows(&rows, &["g".to_string()])), ["g", "b", "e"]);
        // A fold naming no row changes nothing.
        assert_eq!(visible_rows(&rows, &["zzz".to_string()]).len(), 6);
    }

    #[test]
    fn zones_split_rows_and_groups() {
        assert_eq!(zone_at(0.1, false), DropZone::Above);
        assert_eq!(zone_at(0.6, false), DropZone::Below);
        assert_eq!(zone_at(0.1, true), DropZone::Above);
        assert_eq!(zone_at(0.5, true), DropZone::Into);
        assert_eq!(zone_at(0.9, true), DropZone::Below);
    }

    #[test]
    fn drops_place_rows_between_into_and_out_of_groups() {
        let rows = tree();
        // Above a layer inside a group: into that group, above it.
        assert_eq!(drop_move(&rows, &[], "b", "c", DropZone::Above), Some((Some("g".into()), Placement::Above("c".into()))));
        // Below a top-level layer: top level.
        assert_eq!(drop_move(&rows, &[], "a", "b", DropZone::Below), Some((None, Placement::Below("b".into()))));
        // Into a group: on top of what it holds.
        assert_eq!(drop_move(&rows, &[], "b", "e", DropZone::Into), Some((Some("e".into()), Placement::Top)));
        // Below an open group that holds layers: the top of that group.
        assert_eq!(drop_move(&rows, &[], "b", "g", DropZone::Below), Some((Some("g".into()), Placement::Top)));
        // Below a folded group, or an empty one: after it, beside it.
        assert_eq!(drop_move(&rows, &["g".to_string()], "b", "g", DropZone::Below), Some((None, Placement::Below("g".into()))));
        assert_eq!(drop_move(&rows, &[], "b", "e", DropZone::Below), Some((None, Placement::Below("e".into()))));
    }

    #[test]
    fn drops_never_put_a_group_inside_itself() {
        let rows = tree();
        assert_eq!(drop_move(&rows, &[], "g", "g", DropZone::Into), None);
        assert_eq!(drop_move(&rows, &[], "g", "h", DropZone::Into), None);
        assert_eq!(drop_move(&rows, &[], "g", "a", DropZone::Above), None);
        assert_eq!(drop_move(&rows, &[], "a", "zzz", DropZone::Above), None);
        // Out of its own group onto its grandparent is fine.
        assert_eq!(drop_move(&rows, &[], "a", "g", DropZone::Above), Some((None, Placement::Above("g".into()))));
    }

    #[test]
    fn group_layers_takes_the_picks_in_panel_order() {
        let rows = tree();
        assert_eq!(picked_for_group(&rows, Some("b"), &["a".to_string()]), ["a", "b"]);
        assert_eq!(picked_for_group(&rows, None, &[]), Vec::<String>::new());
        assert_eq!(picked_for_group(&rows, Some("c"), &[]), ["c"]);
    }
}
