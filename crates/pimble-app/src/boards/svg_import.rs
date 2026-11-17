//! SVG in (docs/DRAWING_CONTRACT.md section 2, "SVG in"; "Built in wave 3
//! (UI)"): SVG markup pasted while a canvas has the focus, an `.svg` file
//! dropped on one, and "Import SVG..." in the zoom corner's menu, all through
//! `pimble_board::svg_in::parse` and `BoardTxn::import_svg`: centered at the
//! pointer (a drop) or the view's middle, scaled down to fit the view, in the
//! layer the tools draw in (a new "Vector 1" when there is none), selected.
//!
//! The pure parts (is this SVG, the notice of what was left out) are tested
//! here.

use pimble_board::svg_in;
use pimble_board::SvgPlacement;

use super::*;

/// How much of the view an imported drawing may take before it is scaled
/// down to fit.
const FILL: f32 = 0.9;

/// Whether pasted text is SVG markup: it starts with `<svg`, or with an XML
/// declaration, a doctype or a comment and holds an `<svg` element.
pub fn is_svg_markup(text: &str) -> bool {
    let t = text.trim_start_matches('\u{feff}').trim_start();
    if t.starts_with("<svg") {
        return true;
    }
    (t.starts_with("<?xml") || t.starts_with("<!DOCTYPE") || t.starts_with("<!--")) && t.contains("<svg")
}

/// Whether a file is SVG: its name ends in `.svg`, or what it holds is SVG
/// markup. (The browser judges by the file's name and type instead.)
#[cfg_attr(not(feature = "native"), allow(dead_code))]
pub fn is_svg_file(name: Option<&str>, bytes: &[u8]) -> bool {
    if name.is_some_and(|n| n.to_ascii_lowercase().ends_with(".svg")) {
        return true;
    }
    let head = &bytes[..bytes.len().min(4096)];
    std::str::from_utf8(head).map(is_svg_markup).unwrap_or_else(|e| std::str::from_utf8(&head[..e.valid_up_to()]).is_ok_and(is_svg_markup))
}

/// What a warning is about, in a few words: "Clip paths are not imported:
/// ..." is "clip paths".
fn subject(warning: &str) -> String {
    let cut = [" are ", " is ", " keeps "].iter().filter_map(|w| warning.find(w)).min().unwrap_or(warning.len());
    let mut words = warning[..cut].to_string();
    if let Some(open) = words.find(" (") {
        words.truncate(open);
    }
    let mut chars = words.trim().chars();
    match chars.next() {
        Some(first) => first.to_lowercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// The one short notice an import says when something was left out or
/// changed: "Imported 12 shapes; skipped: clip paths, masks." (`None` when
/// nothing was).
pub fn notice(shapes: usize, warnings: &[String]) -> Option<String> {
    if warnings.is_empty() {
        return None;
    }
    let (skipped, changed): (Vec<&String>, Vec<&String>) = warnings.iter().partition(|w| w.contains("not imported"));
    let list = |ws: Vec<&String>| ws.into_iter().map(|w| subject(w)).filter(|w| !w.is_empty()).collect::<Vec<_>>().join(", ");
    let mut out = format!("Imported {shapes} {}", if shapes == 1 { "shape" } else { "shapes" });
    if !skipped.is_empty() {
        out.push_str(&format!("; skipped: {}", list(skipped)));
    }
    if !changed.is_empty() {
        out.push_str(&format!("; simplified: {}", list(changed)));
    }
    out.push('.');
    Some(out)
}

/// Import SVG markup onto pane `pane`'s board, centered at `at` (physical
/// pixels over its surface) or the view's middle. Refusals and what was left
/// out are said.
pub(crate) fn import_text(store: AppStore, pane: PaneId, text: &str, at: Option<(f32, f32)>) {
    if with_session(pane, |_| ()).is_none() || !writable_or_say(store, pane) {
        return;
    }
    let Some((center, view)) = with_session(pane, |s| {
        let (w, h) = s.view.size();
        let center = match at {
            Some((x, y)) => s.view.screen_to_board(x, y),
            None => s.view.center(),
        };
        let a = s.view.screen_to_board(0.0, 0.0);
        let b = s.view.screen_to_board(w as f32, h as f32);
        (center, Rect::new(a.x.min(b.x), a.y.min(b.y), (b.x - a.x).abs(), (b.y - a.y).abs()))
    }) else {
        return;
    };
    let parse = |fit: Option<f32>| svg_in::parse(text, &SvgPlacement { at: center, center: true, fit });
    let mut import = match parse(None) {
        Ok(i) => i,
        Err(e) => {
            crate::events::show_notice(store, e.to_string());
            return;
        }
    };
    // Bigger than the view: scaled down until it fits.
    if let Some(b) = import.bounds {
        if b.w > 0.0 && b.h > 0.0 && view.w > 0.0 && view.h > 0.0 && (b.w > view.w * FILL || b.h > view.h * FILL) {
            let factor = (view.w * FILL / b.w).min(view.h * FILL / b.h);
            if let Ok(fitted) = parse(Some(b.w.max(b.h) * factor)) {
                import = fitted;
            }
        }
    }
    let Some(layer) = tools::target_layer(store, pane) else { return };
    new_undo_step(pane);
    let Some(ids) = local_edit_said(store, pane, |t| t.import_svg(&import, Some(&layer))) else { return };
    // The drawing's top level is selected (a group's members go with it).
    let top: Vec<String> = import.items.iter().zip(&ids).filter(|(item, _)| item.group.is_none()).map(|(_, id)| id.clone()).collect();
    let shapes = import.items.iter().filter(|i| i.element.kind != Kind::Group).count();
    if untracked(|| store.focused_pane.get()) == pane {
        store.board_tool.set(Tool::Select);
    }
    tools::select(store, pane, top);
    if let Some(sentence) = notice(shapes, &import.warnings) {
        crate::events::show_notice(store, sentence);
    }
}

/// Import SVG files (their bytes) onto pane `pane`'s board, at `at`.
pub(crate) fn import_files(store: AppStore, pane: PaneId, at: Option<(f32, f32)>, files: Vec<(Option<String>, Vec<u8>)>) {
    for (name, bytes) in files {
        match String::from_utf8(bytes) {
            Ok(text) => import_text(store, pane, &text, at),
            Err(_) => crate::events::show_notice(store, format!("\"{}\" is not SVG that can be read.", name.unwrap_or_else(|| "The file".to_string()))),
        }
    }
}

/// Ctrl+V on a board on the desktop: SVG markup on the clipboard is
/// imported, else the picture on it goes on the board.
#[cfg(feature = "native")]
pub(crate) fn paste(store: AppStore, pane: PaneId) {
    if let Ok(text) = rinch::clipboard::paste_text() {
        if is_svg_markup(&text) {
            import_text(store, pane, &text, None);
            return;
        }
    }
    crate::boards::paste_image(store, Dest::Board(pane, None));
}

/// "Import SVG...": a file picked (the desktop's dialog, the browser's file
/// input), imported in the view's middle.
pub(super) fn pick(store: AppStore, pane: PaneId) {
    tools::end_text(store, pane);
    #[cfg(feature = "native")]
    {
        // As `PIMBLE_DEBUG_IMAGES`: a file to take instead of the dialog,
        // for driving the app over rinch's debug port. Never set in
        // ordinary use.
        let path = match std::env::var("PIMBLE_DEBUG_SVG") {
            Ok(p) => Some(std::path::PathBuf::from(p)),
            Err(_) => rinch::dialogs::open_file().set_title("Import SVG").add_filter("SVG", &["svg"]).pick_file(),
        };
        let Some(path) = path else { return };
        match std::fs::read(&path) {
            Ok(bytes) => import_files(store, pane, None, vec![(path.file_name().map(|n| n.to_string_lossy().to_string()), bytes)]),
            Err(e) => crate::events::show_notice(store, format!("\"{}\" could not be read: {e}.", path.display())),
        }
    }
    #[cfg(not(feature = "native"))]
    pick_in_browser(store, pane);
}

/// The browser's picker for "Import SVG...": a hidden file input.
#[cfg(not(feature = "native"))]
fn pick_in_browser(store: AppStore, pane: PaneId) {
    use wasm_bindgen::JsCast;

    const INPUT_ID: &str = "pimble-svg-input";
    let Some(document) = web_sys::window().and_then(|w| w.document()) else { return };
    if let Some(old) = document.get_element_by_id(INPUT_ID) {
        old.remove();
    }
    let Some(input) = document.create_element("input").ok().and_then(|el| el.dyn_into::<web_sys::HtmlInputElement>().ok()) else {
        return;
    };
    input.set_id(INPUT_ID);
    input.set_type("file");
    input.set_accept(".svg,image/svg+xml");
    input.set_hidden(true);
    let Some(body) = document.body() else { return };
    if body.append_child(&input).is_err() {
        return;
    }
    let picked = input.clone();
    let taken = std::rc::Rc::new(Cell::new(false));
    let on_change = wasm_bindgen::closure::Closure::<dyn FnMut(web_sys::Event)>::new(move |_: web_sys::Event| {
        if taken.replace(true) {
            return;
        }
        let files = picked.files();
        picked.remove();
        if let Some(files) = files {
            let files: Vec<web_sys::File> = (0..files.length()).filter_map(|i| files.get(i)).collect();
            read_files(store, pane, None, files);
        }
    });
    if input.add_event_listener_with_callback("change", on_change.as_ref().unchecked_ref()).is_err() {
        return;
    }
    on_change.forget();
    input.click();
}

/// Read browser files in order and import each onto pane `pane`'s board.
#[cfg(not(feature = "native"))]
pub(crate) fn read_files(store: AppStore, pane: PaneId, at: Option<(f32, f32)>, files: Vec<web_sys::File>) {
    if files.is_empty() {
        return;
    }
    wasm_bindgen_futures::spawn_local(async move {
        let mut read = Vec::new();
        for file in files {
            let name = file.name();
            match wasm_bindgen_futures::JsFuture::from(file.array_buffer()).await {
                Ok(buffer) => read.push((Some(name), js_sys::Uint8Array::new(&buffer).to_vec())),
                Err(_) => crate::events::show_notice(store, format!("\"{name}\" could not be read.")),
            }
        }
        import_files(store, pane, at, read);
    });
}

/// Whether a browser file is SVG by its name or type.
#[cfg(not(feature = "native"))]
pub(crate) fn is_svg_web_file(file: &web_sys::File) -> bool {
    file.type_() == "image/svg+xml" || file.name().to_ascii_lowercase().ends_with(".svg")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pasted_svg_is_recognised() {
        assert!(is_svg_markup("<svg xmlns=\"http://www.w3.org/2000/svg\"/>"));
        assert!(is_svg_markup("  \n<svg viewBox=\"0 0 10 10\"></svg>"));
        assert!(is_svg_markup("<?xml version=\"1.0\"?>\n<svg></svg>"));
        assert!(is_svg_markup("\u{feff}<?xml version=\"1.0\"?><!DOCTYPE svg><svg/>"));
        assert!(is_svg_markup("<!-- Made by hand -->\n<svg/>"));
        assert!(!is_svg_markup("<?xml version=\"1.0\"?><html></html>"));
        assert!(!is_svg_markup("Some text about <svg> elements"));
        assert!(!is_svg_markup("<div><svg/></div>"));
        assert!(!is_svg_markup(""));
    }

    #[test]
    fn svg_files_by_name_or_content() {
        assert!(is_svg_file(Some("Logo.SVG"), b"anything"));
        assert!(is_svg_file(None, b"<svg/>"));
        assert!(is_svg_file(Some("drawing"), b"<?xml version=\"1.0\"?><svg/>"));
        assert!(!is_svg_file(Some("photo.png"), &[0x89, b'P', b'N', b'G']));
    }

    #[test]
    fn the_notice_names_what_was_left_out() {
        assert_eq!(notice(3, &[]), None);
        let warnings = vec![
            "Masks are not imported.".to_string(),
            "Clip paths are not imported: the shapes they clip come in whole.".to_string(),
            "Filters (blurs, shadows) are not imported.".to_string(),
        ];
        assert_eq!(notice(12, &warnings).unwrap(), "Imported 12 shapes; skipped: masks, clip paths, filters.");
        let changed = vec!["Text on a path is imported as plain text.".to_string(), "Text styled by a stylesheet class keeps its words, not its look.".to_string()];
        assert_eq!(notice(1, &changed).unwrap(), "Imported 1 shape; simplified: text on a path, text styled by a stylesheet class.");
    }
}
