//! Pimble Desktop Application
//!
//! Entry point for the Rinch-based desktop application. Everything it draws and
//! everything it talks to lives in the `pimble_app` library beside it; this file
//! only reads the command line, starts the logger and opens the window. The
//! library's `native` feature (which this binary requires) is what brings in the
//! embedded server, the tokio backend thread and the desktop shell.

use rinch::prelude::Renderer;

const USAGE: &str = "\
Usage: pimble [--cpu | --renderer <auto|gpu|cpu>]

  --renderer auto   draw on the GPU, and with the CPU if the GPU will not start (the default)
  --renderer gpu    draw on the GPU, and stop if it will not start
  --renderer cpu    draw with the CPU (software rendering)
  --cpu             the same as --renderer cpu

The RINCH_RENDERER environment variable (auto, gpu, cpu) overrides the flag.";

/// The renderer the command line asks for, or the message to exit with.
fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Renderer, String> {
    let mut renderer = Renderer::Auto;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let value = match arg.as_str() {
            "--cpu" => {
                renderer = Renderer::Software;
                continue;
            }
            "--renderer" => args.next().ok_or("--renderer needs a value")?,
            "-h" | "--help" => return Err(String::new()),
            other => match other.strip_prefix("--renderer=") {
                Some(value) => value.to_string(),
                None => return Err(format!("unrecognised argument: {other}")),
            },
        };
        renderer = Renderer::parse(&value)
            .ok_or_else(|| format!("unknown renderer {value:?}; use auto, gpu or cpu"))?;
    }
    Ok(renderer)
}

fn main() {
    let renderer = match parse_args(std::env::args().skip(1)) {
        Ok(renderer) => renderer,
        Err(message) => {
            if message.is_empty() {
                println!("{USAGE}");
                return;
            }
            eprintln!("pimble: {message}\n\n{USAGE}");
            std::process::exit(2);
        }
    };

    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into()),
        )
        .init();

    tracing::info!("Starting Pimble with Rinch...");

    // Run the application
    pimble_app::app::run(renderer);
}

#[cfg(test)]
mod args_tests {
    use super::{Renderer, parse_args};

    fn parse(args: &[&str]) -> Result<Renderer, String> {
        parse_args(args.iter().map(|a| a.to_string()))
    }

    #[test]
    fn no_arguments_mean_auto() {
        assert_eq!(parse(&[]), Ok(Renderer::Auto));
    }

    #[test]
    fn cpu_and_renderer_choose_the_renderer() {
        assert_eq!(parse(&["--cpu"]), Ok(Renderer::Software));
        assert_eq!(parse(&["--renderer", "gpu"]), Ok(Renderer::Gpu));
        assert_eq!(parse(&["--renderer=cpu"]), Ok(Renderer::Software));
        assert_eq!(parse(&["--renderer=auto"]), Ok(Renderer::Auto));
    }

    #[test]
    fn a_bad_argument_is_an_error_with_a_reason() {
        assert!(parse(&["--renderer"]).unwrap_err().contains("needs a value"));
        assert!(parse(&["--renderer", "vulkan"]).unwrap_err().contains("unknown renderer"));
        assert!(parse(&["--fast"]).unwrap_err().contains("unrecognised argument"));
        assert_eq!(parse(&["--help"]), Err(String::new()));
    }
}

#[cfg(test)]
mod collab_sync_test {
    //! Proves the new rinch M9 collaboration (`EditorHandle` + `CollabSession`)
    //! converges through the exact relay shape pimble uses — base64 yrs
    //! deltas over `BroadcastChanges`/`RemoteChanges` — and that pimble's server
    //! `apply_edit` (a `pimble_crdt::NodeDoc::apply_update`) accepts those deltas,
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

        // The pimble server loads that snapshot into its per-node `NodeDoc`
        // (exactly what `update_node_content` does) and will apply deltas to it
        // (exactly what `apply_edit` does).
        let mut server_doc = pimble_crdt::NodeDoc::load(&snapshot).unwrap();

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
