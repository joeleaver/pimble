//! The Markdown edits (docs/MCP_CONTRACT.md "Edits that touch only what they name")
//! against a concurrent edit on another replica: merged both ways, the two replicas
//! end identical, and an edit outside the other's range survives it.

use pimble_crdt::{CrdtError, NodeDoc};

/// Two replicas of one node whose content is `md`.
fn replicas(md: &str) -> (NodeDoc, NodeDoc) {
    let mut a = NodeDoc::new();
    a.init("document", "Doc", None, "2026-10-01T00:00:00Z").unwrap();
    a.write_markdown(md).unwrap();
    let b = NodeDoc::load(&a.save()).unwrap();
    (a, b)
}

/// Exchange what each lacks, both ways, and answer the converged content.
fn merge(a: &mut NodeDoc, b: &mut NodeDoc) -> String {
    let (sv_a, sv_b) = (a.state_vector(), b.state_vector());
    let to_b = a.diff_since(&sv_b).unwrap();
    let to_a = b.diff_since(&sv_a).unwrap();
    a.apply_update(&to_a).unwrap();
    b.apply_update(&to_b).unwrap();
    let (ma, mb) = (a.markdown().unwrap(), b.markdown().unwrap());
    assert_eq!(ma, mb, "the replicas diverged");
    ma
}

const DOC: &str = "# Groceries\n\nthe quick brown fox\n\n- milk\n- eggs\n\n# Errands\n\npost office\n\nbank";

#[test]
fn replace_section_beside_a_text_edit_elsewhere() {
    let (mut a, mut b) = replicas(DOC);
    a.replace_section_markdown("Errands", "- pharmacy\n- library").unwrap();
    b.replace_text("brown", "red").unwrap();
    let merged = merge(&mut a, &mut b);
    assert!(merged.contains("the quick red fox"), "{merged}");
    assert!(merged.contains("- pharmacy\n- library"), "{merged}");
    assert!(!merged.contains("post office"), "{merged}");
}

#[test]
fn two_text_edits_in_one_paragraph_both_survive() {
    let (mut a, mut b) = replicas(DOC);
    a.replace_text("brown", "white").unwrap();
    b.replace_text("fox", "fox jumps").unwrap();
    let merged = merge(&mut a, &mut b);
    assert!(merged.contains("the quick white fox jumps"), "{merged}");
}

#[test]
fn insert_after_and_append_both_land() {
    let (mut a, mut b) = replicas(DOC);
    a.insert_markdown_after("Groceries", true, "bread").unwrap();
    b.append_markdown("call mum").unwrap();
    let merged = merge(&mut a, &mut b);
    let bread = merged.find("bread").expect("bread");
    let errands = merged.find("# Errands").unwrap();
    assert!(bread < errands, "inserted at the end of the Groceries section: {merged}");
    assert!(merged.ends_with("call mum"), "{merged}");
}

#[test]
fn an_edit_inside_a_replaced_section_still_converges() {
    let (mut a, mut b) = replicas(DOC);
    a.replace_section_markdown("Groceries", "nothing needed").unwrap();
    b.replace_text("eggs", "a dozen eggs").unwrap();
    let merged = merge(&mut a, &mut b);
    assert!(merged.contains("nothing needed"), "{merged}");
    assert!(merged.contains("# Errands"), "{merged}");
}

#[test]
fn concurrent_section_replacements_converge() {
    let (mut a, mut b) = replicas(DOC);
    a.replace_section_markdown("Errands", "a's errands").unwrap();
    b.replace_section_markdown("Errands", "b's errands").unwrap();
    let merged = merge(&mut a, &mut b);
    assert!(merged.contains("# Groceries") && merged.contains("# Errands"), "{merged}");
}

#[test]
fn a_refusal_writes_nothing() {
    let (mut a, _) = replicas(DOC);
    let before = a.save();
    let err = a.append_markdown("| a | b |\n|---|---|").unwrap_err();
    assert!(matches!(err, CrdtError::Refused(_)), "{err}");
    assert!(matches!(a.replace_text("milk", "oat milk\nand more"), Err(CrdtError::Refused(_))));
    assert!(matches!(a.write_markdown("again"), Err(CrdtError::Refused(_))));
    assert_eq!(a.save(), before);
}

#[test]
fn a_new_node_reads_as_empty_and_takes_its_first_write() {
    let mut doc = NodeDoc::new();
    doc.init("document", "New", None, "2026-10-01T00:00:00Z").unwrap();
    assert_eq!(doc.markdown().unwrap(), "");
    let delta = doc.write_markdown("# Hello\n\nworld").unwrap();
    let mut other = NodeDoc::new();
    other.apply_update(&delta).unwrap();
    assert_eq!(doc.markdown().unwrap(), "# Hello\n\nworld");
    assert_eq!(doc.text(), "Hello\nworld");
}

#[test]
fn an_edit_that_changes_nothing_is_refused() {
    let (mut a, _) = replicas(DOC);
    let before = a.save();
    assert!(matches!(a.replace_text("brown", "brown"), Err(CrdtError::Refused(_))));
    assert_eq!(a.save(), before);
}

/// Reported 2026-10-01: a replacement that only deletes (a shorter phrase next to a
/// link or inline code) was refused as changing nothing, because the check compared
/// state vectors and a deletion does not move one. It must apply and reach a replica.
#[test]
fn a_deletion_only_replacement_applies_and_merges() {
    let (mut a, mut b) = replicas("See [the docs](https://example.com) for more details.\n\nRun `make` and then wait.");
    let delta = a.replace_text("for more details", "for details").unwrap();
    assert!(b.apply_update(&delta).unwrap().changed, "the deletion changes the other replica");
    a.replace_text("and then wait", "and wait").unwrap();
    let merged = merge(&mut a, &mut b);
    assert_eq!(merged, "See [the docs](https://example.com) for details.\n\nRun `make` and wait.");
}

#[test]
fn replace_content_leaves_a_paragraph_someone_is_typing_in() {
    let (mut a, mut b) = replicas("# Plan\n\nkeep this paragraph\n\nold step one\n\nold step two");
    a.replace_content_markdown("# Plan\n\nkeep this paragraph\n\n## Steps\n\n- new step").unwrap();
    b.replace_text("keep this paragraph", "keep this paragraph, typed meanwhile").unwrap();
    let merged = merge(&mut a, &mut b);
    assert_eq!(merged, "# Plan\n\nkeep this paragraph, typed meanwhile\n\n## Steps\n\n- new step");
}

#[test]
fn remove_section_and_replace_block_merge_beside_other_edits() {
    let (mut a, mut b) = replicas(DOC);
    a.remove_section("Errands").unwrap();
    b.replace_block_markdown("Groceries", "## Shopping").unwrap();
    let merged = merge(&mut a, &mut b);
    assert!(merged.starts_with("## Shopping\n\nthe quick brown fox"), "{merged}");
    assert!(!merged.contains("Errands") && !merged.contains("post office"), "{merged}");
}

/// A tiny deterministic generator, so a failing seed replays.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 33
    }
    fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
        items[(self.next() as usize) % items.len()]
    }
}

/// One random edit; refusals (a quote that no longer names one place) are part of
/// the game and change nothing.
fn random_edit(doc: &mut NodeDoc, rng: &mut Rng, step: usize) {
    let words = ["quick", "fox", "milk", "eggs", "post", "bank", "Errands", "Groceries", "new"];
    let quote = rng.pick(&words);
    let text = format!("w{step}");
    let _ = match rng.next() % 9 {
        0 => doc.replace_text(quote, &format!("{quote} {text}")),
        1 => doc.replace_text(quote, &text),
        2 => doc.insert_markdown_after(quote, rng.next().is_multiple_of(2), &format!("new {text}")),
        3 => doc.replace_section_markdown(quote, &format!("- {text}\n- more")),
        4 => doc.replace_text(quote, ""),
        5 => doc.replace_block_markdown(quote, if rng.next().is_multiple_of(2) { "" } else { "## Groceries" }),
        6 => doc.remove_section(quote),
        7 => {
            let current = doc.markdown().unwrap_or_default();
            doc.replace_content_markdown(&format!("{current}\n\n{text}").replace("milk", "oat milk"))
        }
        _ => doc.append_markdown(&format!("**{text}** end")),
    };
}

#[test]
fn random_edits_on_three_replicas_converge() {
    let seeds: Vec<u64> = match std::env::var("PIMBLE_MARKDOWN_SEEDS") {
        Ok(list) => list.split(',').map(|s| s.trim().parse().unwrap()).collect(),
        Err(_) => (1..=150).collect(),
    };
    for seed in seeds {
        let mut rng = Rng(seed);
        let (mut a, b) = replicas(DOC);
        let mut c = NodeDoc::load(&b.save()).unwrap();
        let mut b = b;
        for round in 0..4 {
            for (i, doc) in [&mut a, &mut b, &mut c].into_iter().enumerate() {
                for k in 0..3 {
                    random_edit(doc, &mut rng, round * 100 + i * 10 + k);
                }
            }
            // Merge in a seed-chosen order.
            match rng.next() % 3 {
                0 => { merge(&mut a, &mut b); merge(&mut b, &mut c); merge(&mut a, &mut b); }
                1 => { merge(&mut c, &mut a); merge(&mut b, &mut c); merge(&mut a, &mut c); }
                _ => { merge(&mut b, &mut c); merge(&mut a, &mut c); merge(&mut a, &mut b); }
            }
            let (ma, mb, mc) = (a.markdown().unwrap(), b.markdown().unwrap(), c.markdown().unwrap());
            assert!(ma == mb && mb == mc, "seed {seed} round {round} diverged:\n{ma}\n---\n{mb}\n---\n{mc}");
        }
    }
}
