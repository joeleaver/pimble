//! Spellchecking in the note editor (Joe, 2026-11-24: rinch's spellcheck,
//! "which we totally want"; rinch #2862).
//!
//! rinch marks misspelled words with a wavy underline, and on the desktop a
//! right press on one offers up to three corrections and "Add to dictionary"
//! in the text menu. What Pimble decides here:
//!
//! - **Where the words come from.** On the desktop, the system's own
//!   spellchecking as rinch's default picks it: the operating system's
//!   spellchecker on Windows and macOS, the system's Hunspell dictionaries for
//!   the user's locale on Linux. A Linux machine with none installed and an
//!   English locale gets the `en_US` built into the binary
//!   (`assets/dictionaries/`). In the browser, which lends a page no
//!   dictionaries, the web app fetches that same `en_US` when the browser's
//!   first language is English ([`start_web`]).
//! - **Where a person's added words live: on this device.** The personal
//!   dictionary is `<data dir>/pimble/dictionary.txt` (one word per line) on
//!   the desktop and `localStorage` `pimble.dictionary` in the browser. It is
//!   not account data and does not sync (CLAUDE.md: nothing is put on Pimble
//!   Cloud that the person did not ask for). With the platform's
//!   spellchecker (Windows, macOS) an added word also goes to the system's
//!   user dictionary, as Chrome does, so other apps accept it too.
//! - **What is not checked**: code, in a code block or marked inline
//!   ([`note_plugin`]); and, as rinch decides, URLs, addresses, words with
//!   digits and words in capitals.
//!
//! A note this device may only read is not marked: rinch checks only an
//! editable editor, so the text menu there never offers a correction that
//! would change the note.

use std::rc::Rc;
use std::sync::{Arc, OnceLock};

use rinch::spellcheck::PersonalDictionary;

use crate::rinch_editor::{EditorHandle, SpellcheckPlugin, SPELLCHECK_KEY};

/// The language of the dictionary Pimble carries. The Windows and macOS
/// desktops use the platform's spellchecker and have no use for it (nor for
/// [`prefers_english`]).
#[cfg(any(test, feature = "web", all(feature = "native", not(any(target_os = "windows", target_os = "macos")))))]
const BUNDLED_LANGUAGE: &str = "en_US";

/// The person's own words, loaded once from where this device keeps them,
/// and saved again after every change ("Add to dictionary" included).
pub(crate) fn personal() -> Arc<PersonalDictionary> {
    static PERSONAL: OnceLock<Arc<PersonalDictionary>> = OnceLock::new();
    PERSONAL
        .get_or_init(|| {
            let dictionary = Arc::new(PersonalDictionary::with_words(words::load()));
            let held = Arc::downgrade(&dictionary);
            dictionary.add_listener(move |_| {
                if let Some(dictionary) = held.upgrade() {
                    words::save(&dictionary);
                }
            });
            dictionary
        })
        .clone()
}

/// The note editor's spellchecker: rinch's, leaving code out.
pub(crate) fn note_plugin() -> SpellcheckPlugin {
    SpellcheckPlugin::new().skip_nodes(["code_block"]).skip_marks(["code"])
}

/// Puts [`note_plugin`] in `handle` in place of the one `create_editor` adds.
/// A construction-time call, as `add_plugin` is. It is installed even before
/// the app has dictionaries (the browser fetches them): rinch's own would
/// otherwise join the editor when they arrive, and check code too.
pub(crate) fn install(handle: &EditorHandle) {
    handle.remove_plugin(SPELLCHECK_KEY);
    handle.add_plugin(Rc::new(note_plugin()));
}

/// Whether the person's most preferred language, as a locale (`en_US`,
/// `en-GB`, `en`), is English: the one the bundled dictionary checks. With no
/// locale at all (`C`), English is what is left.
#[cfg(any(test, feature = "web", all(feature = "native", not(any(target_os = "windows", target_os = "macos")))))]
fn prefers_english<S: AsRef<str>>(locales: &[S]) -> bool {
    locales.first().is_none_or(|first| {
        let first = first.as_ref();
        let language = first.split(['_', '-', '.', '@']).next().unwrap_or(first);
        language.eq_ignore_ascii_case("en")
    })
}

/// The desktop's spellcheck settings, for `App::spellcheck`.
#[cfg(feature = "native")]
pub(crate) fn desktop_config() -> rinch::spellcheck::SpellcheckConfig {
    let config = rinch::spellcheck::SpellcheckConfig::new().personal_dictionary(personal());
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    if let Some(checker) = linux::bundled_when_needed() {
        return config.checker(checker);
    }
    config
}

/// Linux (and every other desktop without a platform spellchecker).
#[cfg(all(feature = "native", not(any(target_os = "windows", target_os = "macos"))))]
mod linux {
    use rinch::spellcheck::{system, HunspellChecker};

    const AFF: &[u8] = include_bytes!("../../../assets/dictionaries/en_US.aff");
    const DIC: &[u8] = include_bytes!("../../../assets/dictionaries/en_US.dic");

    /// The bundled `en_US`, when the system has no dictionary for any of the
    /// user's locales and the most preferred one is English. Otherwise
    /// `None`: the system's dictionaries are used, as rinch's default does.
    pub(super) fn bundled_when_needed() -> Option<HunspellChecker> {
        let locales = system::user_locales();
        if locales.iter().any(|locale| system::find(locale).is_some()) || !super::prefers_english(&locales) {
            return None;
        }
        match HunspellChecker::from_bytes(super::BUNDLED_LANGUAGE, AFF, DIC) {
            Ok(checker) => {
                tracing::info!("No system spelling dictionary for {locales:?}; checking with the bundled en_US");
                Some(checker.with_personal_dictionary(super::personal()))
            }
            Err(e) => {
                tracing::warn!("The bundled en_US dictionary did not load: {e}");
                None
            }
        }
    }
}

/// Starts checking the note editor in the browser: fetches the bundled
/// dictionary when the browser's first language is English, then hands rinch
/// a checker over it. An editor made before it arrives is checked from its
/// next press or edit on (rinch keeps its undo history): rinch re-checks at
/// once only an editor its checker has already started on.
#[cfg(feature = "web")]
pub fn start_web() {
    /// Where web/index.html has trunk copy `assets/dictionaries/`.
    const URL: &str = "/app/dictionaries/";
    let languages: Vec<String> = web_sys::window()
        .map(|w| w.navigator().languages().iter().filter_map(|l| l.as_string()).collect())
        .unwrap_or_default();
    if !prefers_english(&languages) {
        tracing::info!("No spelling dictionary for {languages:?}; the note editor is not spellchecked");
        return;
    }
    wasm_bindgen_futures::spawn_local(async move {
        let aff = web::fetch_bytes(&format!("{URL}{BUNDLED_LANGUAGE}.aff")).await;
        let dic = web::fetch_bytes(&format!("{URL}{BUNDLED_LANGUAGE}.dic")).await;
        let (Some(aff), Some(dic)) = (aff, dic) else {
            tracing::warn!("The spelling dictionary could not be fetched; the note editor is not spellchecked");
            return;
        };
        match rinch::spellcheck::HunspellChecker::from_bytes(BUNDLED_LANGUAGE, &aff, &dic) {
            Ok(checker) => rinch::spellcheck::configure(
                rinch::spellcheck::SpellcheckConfig::new().checker(checker.with_personal_dictionary(personal())),
            ),
            Err(e) => tracing::warn!("The spelling dictionary did not load: {e}"),
        }
    });
}

#[cfg(feature = "web")]
mod web {
    use wasm_bindgen::JsCast;

    /// The bytes at `url` on this origin, or `None` when it cannot be read.
    pub(super) async fn fetch_bytes(url: &str) -> Option<Vec<u8>> {
        let window = web_sys::window()?;
        let response: web_sys::Response =
            wasm_bindgen_futures::JsFuture::from(window.fetch_with_str(url)).await.ok()?.dyn_into().ok()?;
        if !response.ok() {
            return None;
        }
        let buffer = wasm_bindgen_futures::JsFuture::from(response.array_buffer().ok()?).await.ok()?;
        Some(js_sys::Uint8Array::new(&buffer).to_vec())
    }
}

/// Where this device keeps the person's own words.
#[cfg(feature = "native")]
mod words {
    use std::path::PathBuf;

    use rinch::spellcheck::PersonalDictionary;

    /// `<data dir>/pimble/dictionary.txt`.
    fn path() -> PathBuf {
        dirs::data_dir().unwrap_or_else(|| PathBuf::from(".")).join("pimble").join("dictionary.txt")
    }

    pub(super) fn load() -> Vec<String> {
        std::fs::read_to_string(path()).map(|text| super::parse(&text)).unwrap_or_default()
    }

    /// Written beside and renamed, so a crash never leaves half a file. The
    /// words are read under the lock, so of two threads' saves (rinch tells
    /// the listener on the thread that changed the list) the later one
    /// writes the later list, and neither shares the other's temporary file.
    #[cfg(not(test))]
    pub(super) fn save(dictionary: &PersonalDictionary) {
        static SAVING: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _saving = SAVING.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let words = dictionary.words();
        let path = path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let tmp = path.with_extension("txt.tmp");
        if std::fs::write(&tmp, super::render(&words)).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }

    /// Tests never write the real file of whoever runs them.
    #[cfg(test)]
    pub(super) fn save(_dictionary: &PersonalDictionary) {}
}

#[cfg(not(feature = "native"))]
mod words {
    use rinch::spellcheck::PersonalDictionary;

    /// Where the person's own words live in the browser.
    const KEY: &str = "pimble.dictionary";

    fn storage() -> Option<web_sys::Storage> {
        if !cfg!(target_arch = "wasm32") {
            return None;
        }
        web_sys::window()?.local_storage().ok().flatten()
    }

    pub(super) fn load() -> Vec<String> {
        storage().and_then(|s| s.get_item(KEY).ok().flatten()).map(|text| super::parse(&text)).unwrap_or_default()
    }

    /// The browser runs one thread, so a save always writes the latest list.
    pub(super) fn save(dictionary: &PersonalDictionary) {
        if let Some(storage) = storage() {
            let _ = storage.set_item(KEY, &super::render(&dictionary.words()));
        }
    }
}

/// The saved list: one word per line, blank lines ignored.
fn parse(text: &str) -> Vec<String> {
    text.lines().map(str::trim).filter(|w| !w.is_empty()).map(str::to_owned).collect()
}

/// The list as saved: sorted, one word per line.
fn render(words: &[String]) -> String {
    let mut words = words.to_vec();
    words.sort();
    words.dedup();
    let mut text = words.join("\n");
    text.push('\n');
    text
}

#[cfg(test)]
#[path = "spelling_tests.rs"]
mod tests;
