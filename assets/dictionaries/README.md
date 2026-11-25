# Bundled spelling dictionaries

The note editor's spellchecker (rinch's, rinch #2862) reads Hunspell
`.aff`/`.dic` pairs. Where they are used (`crates/pimble-app/src/spelling.rs`):

- **Browser**: a page has no dictionaries of its own to lend rinch, so the web
  app fetches `en_US` from `/app/dictionaries/` (trunk copies this directory,
  `web/index.html`) when the browser's first language is English. The files
  are fetched, not built into the `.wasm`.
- **Linux desktop**: the system's Hunspell dictionaries for the user's locale
  come first (`/usr/share/hunspell`, `~/.local/share/hunspell`, `$DICPATH`).
  When none is installed and the locale is English, the binary's own copy of
  `en_US` is used.
- **Windows and macOS desktop**: the operating system's spellchecker; these
  files are not built in there.

`en_US` is SCOWL's (Kevin Atkinson's Spell Checker Oriented Word Lists),
version 2020.12.07, as Ubuntu's `hunspell-en-us` package
(`1:2020.12.07-4build1`) installs it, unmodified. Its license is permissive;
`en_US.COPYRIGHT` is that package's copyright file, which must travel with the
files (the UK Advanced Cryptics Dictionary's notice asks for it verbatim).

| File | Bytes | SHA-256 |
| --- | --- | --- |
| `en_US.aff` | 3131 | `70fe5778717d097ce2f3326baaa5c1e4d2206d81a5a81d3ea8e11c4770806dd5` |
| `en_US.dic` | 860381 | `829a043cf078d1e80e886289a13823454977f442a239a859d2133ea61944aa60` |
