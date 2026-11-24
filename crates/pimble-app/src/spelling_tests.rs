use super::*;

#[test]
fn english_is_the_most_preferred_language_or_none() {
    assert!(prefers_english(&["en_US"]));
    assert!(prefers_english(&["en-GB", "de"]));
    assert!(prefers_english(&["en"]));
    assert!(prefers_english(&["en_US.UTF-8"]));
    assert!(prefers_english::<&str>(&[]));
    assert!(!prefers_english(&["de_DE", "en_US"]));
    assert!(!prefers_english(&["eng"]));
}

#[test]
fn the_saved_list_is_one_word_per_line_sorted() {
    let words = vec!["Pimble".to_owned(), "rinch".to_owned(), "Pimble".to_owned(), "Atkinson".to_owned()];
    let text = render(&words);
    assert_eq!(text, "Atkinson\nPimble\nrinch\n");
    assert_eq!(parse(&text), ["Atkinson", "Pimble", "rinch"]);
    assert_eq!(parse("  one \n\n two\r\n"), ["one", "two"]);
    assert!(parse("").is_empty());
}

/// The bundled dictionary loads, and the note editor marks what it rejects,
/// except code: in a code block or marked inline.
#[cfg(feature = "native")]
#[test]
fn the_note_editor_marks_misspellings_but_not_code() {
    use rinch::spellcheck::runtime::set_inline_jobs;
    use rinch::spellcheck::{HunspellChecker, SpellcheckConfig};

    const AFF: &[u8] = include_bytes!("../../../assets/dictionaries/en_US.aff");
    const DIC: &[u8] = include_bytes!("../../../assets/dictionaries/en_US.dic");
    set_inline_jobs(true);
    let checker = HunspellChecker::from_bytes(BUNDLED_LANGUAGE, AFF, DIC).expect("the bundled en_US loads");
    rinch::spellcheck::configure(SpellcheckConfig::new().checker(checker).debounce_ms(0));

    let handle = crate::rinch_editor::create_editor();
    install(&handle);
    handle.load_html("<p>The quick bruwn fox</p><pre><code>let wrld = 1;</code></pre><p>an <code>fmtt</code> tset</p>");
    handle.spell_user_focus();
    let marked: Vec<String> = handle.misspellings().into_iter().map(|m| m.word).collect();
    assert_eq!(marked, ["bruwn", "tset"]);

    rinch::spellcheck::clear_config();
    set_inline_jobs(false);
}

/// `install` leaves exactly one spellchecker in the editor, the note's.
#[cfg(feature = "native")]
#[test]
fn install_replaces_rinchs_spellchecker() {
    let handle = crate::rinch_editor::create_editor();
    install(&handle);
    // Installed already: a second one is refused.
    assert!(!handle.add_plugin(Rc::new(SpellcheckPlugin::new())));
    assert!(handle.remove_plugin(SPELLCHECK_KEY));
    assert!(!handle.remove_plugin(SPELLCHECK_KEY));
}
