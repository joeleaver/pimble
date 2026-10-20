# Pimble brand

Approved by Joe on 2026-10-20.

- **Mark**: a smooth pebble with a hole through it (`pimble-mark.svg`), drawn on a
  64-unit grid. The hole winds against the outline, so it stays open under either
  fill rule.
- **Icon**: the pebble in `#f3f5f6` on a slate tile, 60 of 64 units with a 15-unit
  corner (`pimble-icon.svg`); `png/` holds 16 to 1024 px and `pimble.ico` holds
  16, 24, 32, 48, 64 and 256.
- **Wordmark**: lowercase `pimble` in Manrope Medium (weight 500), tracked
  -0.02 em, outlined so no font file ships for it (`pimble-wordmark.svg`).
- **Lockup**: the pebble 0.875 em tall, 0.28 em before the word, centred a little
  above the x-height's middle (`pimble-lockup.svg`, and `pimble-lockup-on-dark.svg`
  for dark grounds).
- **Colors**: slate `#3f6491` for the mark, ink `#1c2024` for the word; on dark
  grounds `#a9b9cd` and `#e6e8eb` in the files, and the site's `#7ba1d6` for the
  mark in the app's dark title bar.

Where it is used: the desktop window icon and title bar (`crates/pimble-app/src/
brand.rs`, which carries the same paths), the site's favicon and header
(`site/brand/`, copies), the web app's favicon (`web/index.html` copies from here).

Manrope is under the SIL Open Font License 1.1 (The Manrope Project Authors).
