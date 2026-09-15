//! Pimble Desktop Application
//!
//! Entry point for the Rinch-based desktop application. Everything it draws and
//! everything it talks to lives in the `pimble_app` library beside it; this file
//! only starts the logger and opens the window. The library's `native` feature
//! (which this binary requires) is what brings in the embedded server, the tokio
//! backend thread and the desktop shell.

fn main() {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into()),
        )
        .init();

    tracing::info!("Starting Pimble with Rinch...");

    // Run the application
    pimble_app::app::run();
}

#[cfg(test)]
mod collab_sync_test {
    //! Proves the new rinch M9 collaboration (`EditorHandle` + `CollabSession`)
    //! converges through the exact relay shape pimble uses — base64 yrs
    //! deltas over `BroadcastChanges`/`RemoteChanges` — and that pimble's server
    //! `apply_edit` (a `pimble_crdt::ContentDoc::apply_update`) accepts those deltas,
    //! which is the integration assumption the whole migration rests on.

    use rinch::prelude::*;
    use rinch_editor_core::{Node, Pos, Selection};

    /// The document's text, blocks joined by '\n'.
    fn doc_text(h: &EditorHandle) -> String {
        fn collect(n: &Node, out: &mut String) {
            if let Some(t) = n.text() {
                out.push_str(t);
                return;
            }
            for i in 0..n.child_count() {
                collect(n.child(i), out);
            }
        }
        let doc = h.doc();
        let mut s = String::new();
        for i in 0..doc.child_count() {
            if i > 0 {
                s.push('\n');
            }
            collect(doc.child(i), &mut s);
        }
        s
    }

    #[test]
    fn collab_converges_through_a_server_relay() {
        use std::cell::RefCell;
        use std::rc::Rc;

        // Buffers standing in for the network: A's outbound → B, B's outbound → A.
        let to_b: Rc<RefCell<Vec<Vec<u8>>>> = Rc::new(RefCell::new(Vec::new()));
        let to_a: Rc<RefCell<Vec<Vec<u8>>>> = Rc::new(RefCell::new(Vec::new()));

        // Client A hosts a fresh document and shares its snapshot.
        let a = create_editor();
        assert!(a.load_html("<p>Hello</p>"));
        let tb = to_b.clone();
        let snapshot = a
            .start_collaboration_host(move |d| tb.borrow_mut().push(d))
            .expect("host projects the flat document");

        // The pimble server loads that snapshot into its per-node `ContentDoc`
        // (exactly what `update_node_content` does) and will apply deltas to it
        // (exactly what `apply_edit` does).
        let mut server_doc = pimble_crdt::ContentDoc::load(&snapshot).unwrap();

        // Client B joins from the snapshot and adopts the document.
        let b = create_editor();
        let ta = to_a.clone();
        b.start_collaboration_guest(&snapshot, move |d| ta.borrow_mut().push(d))
            .expect("guest joins");
        assert_eq!(doc_text(&b), "Hello", "guest adopts the host document");

        // A types " World" → delta relays to B; the server applies the same delta.
        a.set_selection(Selection::cursor(Pos(6)));
        assert!(a.insert_text(" World"));
        for d in to_b.borrow_mut().drain(..) {
            server_doc
                .apply_update(&d)
                .expect("server apply_edit accepts the collab delta");
            b.collab_receive(&d);
        }
        assert_eq!(doc_text(&a), "Hello World");
        assert_eq!(doc_text(&b), "Hello World", "A's edit reached B via the relay");

        // B prepends "! " → converges back on A.
        b.set_selection(Selection::cursor(Pos(1)));
        assert!(b.insert_text("! "));
        for d in to_a.borrow_mut().drain(..) {
            server_doc.apply_update(&d).expect("server accepts B's delta");
            a.collab_receive(&d);
        }
        assert_eq!(doc_text(&a), doc_text(&b), "both clients converge");
        assert!(doc_text(&a).contains("World") && doc_text(&a).contains('!'));
        assert_eq!(server_doc.text(), doc_text(&a), "server converges with the clients");
    }
}
