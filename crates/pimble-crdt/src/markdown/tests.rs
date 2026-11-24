//! Markdown in and out: the refusals, rendering and parsing, canvases and embeds.

use std::rc::Rc;

use rinch_editor_core::{Fragment, Schema};

use super::*;

fn schema() -> Rc<Schema> {
    Rc::new(Schema::starter_kit())
}

#[test]
fn check_names_the_construct_and_its_line() {
    let err = check("# Title\n\nsome text\n\n![cat](cat.png)\n").unwrap_err();
    assert!(err.to_string().starts_with("Line 5 is a picture from outside Pimble"), "{err}");
    assert!(check("a\n\n- [ ] task").unwrap_err().to_string().starts_with("Line 3 is a task list item"));
    assert!(check("a\n\nsee <kbd>this</kbd>").unwrap_err().to_string().starts_with("Line 3 is HTML other than"));
    assert!(check("<div>x</div>").unwrap_err().to_string().starts_with("Line 1 is HTML other than"));
    assert!(check("a <u>b").unwrap_err().to_string().contains("without its partner"));
    assert!(check("a[^1]\n\n[^1]: note").unwrap_err().to_string().contains("a footnote"));
    let in_table = "<table><tr><td><img src=\"https://x.y/c.png\"></td></tr></table>";
    assert!(check(in_table).unwrap_err().to_string().contains("a picture from outside Pimble inside an HTML table"));
    assert!(check("![cat](https://x.y/cat.png)").unwrap_err().to_string().contains("from outside Pimble"));
    assert!(check("[x](javascript:alert(1))").unwrap_err().to_string().contains("unsafe"));
    check("# H\n\n**b** *i* ~~s~~ `c` [l](pimble:a/b)\n\n- one\n  - two\n\n1. x\n\n```rust\nfn x() {}\n```\n\n---\n")
        .unwrap();
}

#[test]
fn a_canvas_in_text_writes_its_id_after_the_picture_url() {
    use rinch_editor_core::Attrs;
    let s = schema();
    let image = |src: &str, alt: &str, canvas: Option<&str>| {
        let attrs = Attrs::new().with("src", src).with("alt", alt);
        let attrs = match canvas {
            Some(id) => attrs.with("data-board", id),
            None => attrs,
        };
        s.create_node("image", attrs, Fragment::empty()).unwrap()
    };
    let p = s
        .create_node(
            "paragraph",
            Attrs::new(),
            Fragment::from_children(vec![image("pimble-blob:a/b", "shot", Some("c1")), image("pimble-blob:a/d", "", None)]),
        )
        .unwrap();
    let doc = s.create_node("doc", Attrs::new(), Fragment::from_children(vec![p])).unwrap();
    let md = render(&doc);
    assert!(md.contains("![shot](pimble-blob:a/b#canvas=c1)"), "{md}");
    assert!(md.contains("![](pimble-blob:a/d)"), "a picture from before canvases is as it was: {md}");
}

#[test]
fn pimble_pictures_go_in_and_take_their_canvas_back() {
    let md = "Before\n\n![shot](pimble-blob:a/b#canvas=c1) and ![](pimble-board:in-text#canvas=c2)\n\n![old](pimble-blob:a/d)";
    let doc = parse(&schema(), md).unwrap();
    assert_eq!(canvases_named(&doc), vec!["c1".to_string(), "c2".to_string()]);
    let mut srcs = Vec::new();
    doc.nodes_between(0, doc.content_size(), &mut |n, _, _| {
        if n.type_name() == "image" {
            srcs.push(n.attrs().get_str("src").unwrap_or_default().to_string());
        }
        true
    });
    assert_eq!(srcs, vec!["pimble-blob:a/b", IN_TEXT_SRC, "pimble-blob:a/d"]);
    assert_eq!(render(&doc), md, "it reads back as it went in");
    // The fragment from before canvases is still read.
    assert_eq!(canvases_named(&parse(&schema(), "![x](pimble-blob:a/b#board=c9)").unwrap()), vec!["c9".to_string()]);
    assert!(check("![](pimble-board:in-text)").unwrap_err().to_string().contains("names none"));
}

#[test]
fn a_picture_names_only_a_canvas_the_note_holds_once() {
    let s = schema();
    let before = parse(&s, "text").unwrap();
    let held: std::collections::HashSet<String> = ["c1".to_string()].into();
    let one = parse(&s, "![](pimble-blob:a/b#canvas=c1)").unwrap();
    check_canvases(&before, &one, &held).unwrap();
    let missing = parse(&s, "![](pimble-blob:a/b#canvas=zz)").unwrap();
    assert!(check_canvases(&before, &missing, &held).unwrap_err().to_string().contains("no canvas zz"));
    let twice = parse(&s, "![](pimble-blob:a/b#canvas=c1)\n\n![](pimble-blob:a/b#canvas=c1)").unwrap();
    assert!(check_canvases(&one, &twice, &held).unwrap_err().to_string().contains("shown twice"));
    // A picture already there whose canvas has not arrived is not the edit's doing.
    check_canvases(&missing, &missing, &held).unwrap();
}

#[test]
fn dollar_signs_are_text_not_math() {
    check("it cost $5 and then $10").unwrap();
}

#[test]
fn parse_and_render_round_trip_what_is_in_scope() {
    let md = "# Plan\n\nSome **bold** and *italic* text with a [link](pimble:s/n).\n\n- one\n- two\n\n```\ncode\n```";
    let doc = parse(&schema(), md).unwrap();
    assert_eq!(render(&doc), md);
}

#[test]
fn quotes_tables_and_html_marks_round_trip() {
    for md in [
        "> quoted **words**\n>\n> - and a list",
        "| a | b |\n| --- | --- |\n| 1 | 2 |",
        "<u>under</u> <mark>lit</mark> <span style=\"color:#ff0000\">red</span> H<sub>2</sub>O x<sup>2</sup>",
    ] {
        let doc = parse(&schema(), md).unwrap();
        assert_eq!(render(&parse(&schema(), &render(&doc)).unwrap()), render(&doc), "{md}");
    }
    let doc = parse(&schema(), "> q\n\n| a |\n| --- |\n| 1 |").unwrap();
    assert_eq!(doc.child(0).type_name(), "blockquote");
    assert_eq!(doc.child(1).type_name(), "table");
}

#[test]
fn empty_markdown_is_refused() {
    assert!(parse(&schema(), "  \n").unwrap_err().to_string().contains("no Markdown"));
}

#[test]
fn inline_code_inside_a_mark_is_refused() {
    for md in ["**`make`**", "**Run `make` now**", "[`foo`](https://x.y)", "~~`x`~~", "*a `b`*"] {
        let err = check(md).unwrap_err().to_string();
        assert!(err.contains("inline code inside"), "{md}: {err}");
    }
    check("**Run** `make`, see [the docs](https://x.y): `foo`").unwrap();
}

/// The embed atoms write exactly what the MCP wrote by hand before they were
/// typed (rinch's attribute order), and read back as what they were.
#[test]
fn embeds_write_rinchs_element_and_read_back() {
    let url = pimble_core::PimbleUrl::node(pimble_core::StoreId::new(), pimble_core::NodeId::new());
    let table = Embed::Table { id: "t1".to_string() };
    assert_eq!(table.to_markdown(), "<div data-pm-type=\"embed\" data-sheet=\"t1\"></div>\n");
    let talk = Embed::discussion(url.clone(), Some("Say \"hi\"".to_string()));
    assert_eq!(
        talk.to_markdown(),
        format!("<div data-pm-type=\"embed\" data-kind=\"discussion\" data-node=\"{url}\" data-title=\"Say &quot;hi&quot;\"></div>\n")
    );
    let sheet = Embed::Node { kind: embed_attrs::KIND_SHEET.to_string(), node: url.clone(), title: Some("A & <b>".into()), table: Some("t2".into()) };
    for embed in [table, talk, sheet, Embed::sheet_node(url.clone(), None)] {
        let line = embed.to_markdown();
        assert_eq!(Embed::from_markdown_line(&line), Some(embed.clone()), "{line}");
        // A node's text renders the atom as that line, and takes it back as one atom.
        let s = schema();
        let d = parse(&s, &format!("before\n\n{line}\nafter")).unwrap();
        assert_eq!(d.child_count(), 3);
        let rendered = render(&d);
        let lines: Vec<Embed> = rendered.lines().filter_map(Embed::from_markdown_line).collect();
        assert_eq!(lines, vec![embed.clone()], "{rendered}");
        let attrs = embed.attrs();
        assert_eq!(Embed::from_attrs(|name| attrs.iter().find(|(k, _)| *k == name).map(|(_, v)| v.as_str())), Some(embed));
    }
    assert_eq!(Embed::sheet_node(url.clone(), None).node_of_kind(embed_attrs::KIND_SHEET), Some(&url));
    assert_eq!(Embed::discussion(url.clone(), None).node_of_kind(embed_attrs::KIND_SHEET), None);
    // Not an atom: text, other HTML, an embed with nothing Pimble's in it.
    assert_eq!(Embed::from_markdown_line("some text"), None);
    assert_eq!(Embed::from_markdown_line("<div>x</div>"), None);
    assert_eq!(Embed::from_markdown_line("<div data-pm-type=\"embed\" data-sheet=\"\"></div>"), None);
}
