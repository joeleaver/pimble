# File > Import

Joe, 2026-10-09: import options on the File menu, at least RTF, Word and Scrivener; where
an import lands is a choice made at import time; not desktop only. Word means `.docx`
only (legacy `.doc` is not read).

## What a person does

File > "Import RTF...", "Import Word Document...", "Import Scrivener Project...", on the
desktop and in the browser.

1. **Pick.** RTF and Word are a file; a Scrivener project is its `.scriv` directory (a
   folder dialog on the desktop, a directory input in the browser). Only the files the
   import reads are read: a project's `.scrivx` and each item's
   `Files/Data/{uuid}/content.rtf`, never its snapshots, backups or media.
2. **Choose where.** A modal names what was picked and offers both:
   - "Import under "<node>"": the selected node (a store row means its root; a mount
     means its source). Offered only when something is selected that this device may
     write.
   - "Import into a new store" with a name (defaults to the file's name): on the desktop
     a save dialog places the `.pimble` directory, as "New Store..." does; in the browser
     an encrypted store on the account, as the explorer's `+` makes. The import runs when
     that store opens.
3. **It arrives.** One node per file (titled after it), or for Scrivener one folder named
   after the project with the binder beneath. A notice says how many notes; the parent
   opens.

## Scrivener

- The binder comes in under one folder named after the project; its Trash and the
  top-level "Recovered Files" folders Scrivener makes after a crash are left out (Joe,
  2026-10-09).
- A label is a tag with the label's name, and its color goes where the project shows
  it: `Settings/ui-common.xml` `<Labels><Binder>Yes` makes it the row color,
  `<Icons>Yes` the icon and title color (both when both). A project without that file,
  or showing labels nowhere in the binder, gets the icon and title color.
- The RTF reader keeps line breaks, typed rules (`=====`, `-----`, `***`) as rules,
  `=== Title ===` as a level-2 heading, typed bullets (`•`, `- `) and Scrivener for
  Windows' one-list-per-bullet as one list; it leaves out near-black text color and
  white or black highlights (the theme's own colors read better on both themes).

## Background colors

A node may have a row color (`custom_keys::BACKGROUND`, `#rrggbb`) beside its icon and
title color (`COLOR`): the whole row in the tree is tinted, indent included, behind the
chevron, icon and title, which keep the theme's colors. Set from the row's
"Appearance..." ("Background", a palette of soft colors, or "None"; the icon and title color is "Text"), by the Scrivener import,
or by any client writing the key. `appearance::row_background` draws it: nearly as
given on the light theme, a translucent tint on the dark one, a dark color lifted
first; the selected row shows its selection through it. Both apps (Joe, 2026-10-09).

## How it is built

- `pimble-import` parses into an `Imported` tree (title, type, blocks, icon, color,
  tags, children) and touches no store; it builds for wasm with default features off.
  `store` (the CLI's `import-scrivener`) writes a tree into a new store on disk.
- `BackendCommand::Import` carries the picked files to the backend, which parses them
  and writes the tree with `pimble_app::import::write`: per node `CreateNode`, `GetNode`,
  then one `SetNodeContent` carrying the update that sets the node's type, explicit
  title, icon, color, tags and blocks on the document the server made. These are the
  commands the app already writes with, routed as any other: through `process_command`
  on the desktop, and in the browser through the same routing as every command (the
  vault client for an encrypted store, refusals for a reader included). So an import is
  an ordinary edit in a plain store, an encrypted one, a share or a replica, and reaches
  other devices the way typing does. Nothing is hosted as a side effect.
- Answers `Imported { node_id, count }` or `ImportFailed` with a sentence; a failure
  part way says how many notes were made (they stay).

## Limits

- Pictures in RTF and Word are skipped (images wave 5); legacy `.doc` is not read.
- An import is one command, so a large project holds the command queue while it runs.
