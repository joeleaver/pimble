# Bundled fonts

The core set of docs/THEMES_CONTRACT.md section 5 (decision 8b): enough for
every built-in document theme. The desktop app carries these files in its
binary (`crates/pimble-app/src/fonts.rs`, rinch `App::fonts`); the web app
serves them from `/app/fonts/` (trunk copies this directory, `web/index.html`)
and loads each through `@font-face` the first time a theme uses it. Which
family a theme may name, and which file is which face, is
`crates/pimble-themes/src/fonts.rs`.

Every family is under the SIL Open Font License 1.1, which allows bundling and
redistributing the fonts with software; each directory carries its `OFL.txt`,
which must travel with the files. The files are unmodified: only renamed
(`Inter[opsz,wght].ttf` became `Inter-Variable.ttf`), which the license
allows, since the fonts' own names are unchanged.

All were taken on 2026-11-24 from the Google Fonts repository,
`https://github.com/google/fonts`, branch `main`, directory `ofl/<family>/`.

| Directory | Family | License | Copyright | Upstream file |
| --- | --- | --- | --- | --- |
| `inter/` | Inter (variable: weight 100-900, optical size) | OFL 1.1 | 2020 The Inter Project Authors | `ofl/inter/Inter[opsz,wght].ttf`, `Inter-Italic[opsz,wght].ttf` |
| `source-serif-4/` | Source Serif 4 (variable: weight 200-900, optical size) | OFL 1.1 | 2014 The Source Serif 4 Project Authors | `ofl/sourceserif4/SourceSerif4[opsz,wght].ttf`, `SourceSerif4-Italic[opsz,wght].ttf` |
| `eb-garamond/` | EB Garamond (variable: weight 400-800) | OFL 1.1 | 2017 The EB Garamond Project Authors | `ofl/ebgaramond/EBGaramond[wght].ttf`, `EBGaramond-Italic[wght].ttf` |
| `jetbrains-mono/` | JetBrains Mono (variable: weight 100-800) | OFL 1.1 | 2020 The JetBrains Mono Project Authors | `ofl/jetbrainsmono/JetBrainsMono[wght].ttf`, `JetBrainsMono-Italic[wght].ttf` |
| `atkinson-hyperlegible/` | Atkinson Hyperlegible (Regular, Bold, Italic, Bold Italic) | OFL 1.1 | 2020 Braille Institute of America, Inc. | `ofl/atkinsonhyperlegible/AtkinsonHyperlegible-{Regular,Bold,Italic,BoldItalic}.ttf` |

SHA-256 of each file as bundled:

```
29160a80ff49ddcab2c97711247e08b1fab27a484a329ce8b813d820dc559031  inter/Inter-Variable.ttf
acd98e64795781b2058f07b18475e0ecee2a0fe2b42a49e2f9e37d0d6bf66ce6  inter/Inter-Italic-Variable.ttf
97b2d4da6e3cb494b5a1e66ae176914d852ccabef49e0c02c0df25f3e39aca0b  source-serif-4/SourceSerif4-Variable.ttf
15fbc7e4679489a501998c3669272637a6646388ef7e4bd77eebb5bf967a1f42  source-serif-4/SourceSerif4-Italic-Variable.ttf
ef9512f92f6d579e5dc75af59a5a4b1b8b47d2eda89e00b954d44520e5369027  eb-garamond/EBGaramond-Variable.ttf
bba2c4499c93c9612b90b9825d32b07da52fce2fe57562a1eb6b833553f93c4e  eb-garamond/EBGaramond-Italic-Variable.ttf
48715a42ec242c21e9f02692891e147d022299a52e48d5e413e1a942193ffeda  jetbrains-mono/JetBrainsMono-Variable.ttf
85ae2a5cd3f56baf1ce1c21a851322c58e3d8fbe8e8ad4a4d090a820dd7fe558  jetbrains-mono/JetBrainsMono-Italic-Variable.ttf
7fb917c89019896d0b52ee84b7cbb3304c18cb90b19a62f5e32712bd23e97669  atkinson-hyperlegible/AtkinsonHyperlegible-Regular.ttf
5a3b0c8cc8ca545155150b4512a1fa248298df121c50d6557e651e61fbdab92f  atkinson-hyperlegible/AtkinsonHyperlegible-Bold.ttf
021beda4d3c6edfc78872e436d74009f9a1bcb294331908fe5747c61d3dcc514  atkinson-hyperlegible/AtkinsonHyperlegible-Italic.ttf
7a46f1b0ef2f64b61065c1a6b5a11fcb9b019b44062d88ba121a083cc39e86bf  atkinson-hyperlegible/AtkinsonHyperlegible-BoldItalic.ttf
```

The rest of section 5's list is the font library on pimble.app (decision 8b),
fetched on first use; it is not built yet, so a theme may name only these
families and System.
