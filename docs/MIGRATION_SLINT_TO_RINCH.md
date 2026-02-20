# Pimble Migration Plan: Slint → Rinch

## Overview

Port the Pimble desktop application from Slint to Rinch, leveraging Rinch's CRDT-backed rich-text editor which will replace the custom cosmic-text editor integration.

## Key Changes

| Component | Slint (Current) | Rinch (Target) |
|-----------|-----------------|----------------|
| UI Framework | slint 1.9 | rinch (git) |
| Text Editor | cosmic-text (custom impl) | rinch-editor (built-in CRDT) |
| Window | winit + i-slint-backend-winit | rinch (includes winit) |
| File Dialogs | rfd | rinch-platform dialogs |
| Event Loop | slint::Timer | rinch signal/effect system |

## Phases

### Phase 1: Update Workspace Dependencies

**Files:**
- `Cargo.toml` - Replace slint with rinch
- `crates/pimble-app/Cargo.toml` - Update dependencies

**Changes:**
1. Remove slint workspace dependency
2. Add rinch: `{ git = "https://github.com/joeleaver/rinch.git", features = ["desktop", "components", "theme", "editor"] }`
2. Remove slint-build from pimble-app build-dependencies
3. Remove i-slint-backend-winit, winit, rfd, cosmic-text from pimble-app dependencies
4. Keep: pimble-core, pimble-client, pimble-rpc, pimble-crdt, tokio, tracing, crossbeam-channel, etc.

### Phase 2: Create New Rinch Application Structure

**New/Delete Files:**
- Delete: `crates/pimble-app/ui/` (all .slint files)
- Delete: `crates/pimble-app/src/cosmic_editor.rs` (replaced by rinch-editor)
- Create: `crates/pimble-app/src/ui.rs` (new Rinch UI components)

**Changes to `main.rs`:**
- Change entry point to use `rinch::run()` instead of slint
- Import rinch prelude and components

### Phase 3: Rewrite App Logic with Rinch

**`crates/pimble-app/src/app.rs`:**

Convert all Slint patterns to Rinch:

| Slint Pattern | Rinch Equivalent |
|---------------|------------------|
| `slint::include_modules!()` | N/A - pure Rust RSX |
| `ModelRc<VecModel<T>>` | `Signal<Vec<T>>` |
| `SharedString` | `String` |
| `window.set_foo(x)` | `foo.set(x)` |
| `Callback` | `use_signal` + `use_effect` |
| `on_foo_clicked({...})` | ` move \|_\| {...}` |

onclick:**Key Rinch patterns:**
```rust
use rinch::prelude::*;

#[component]
fn app() -> NodeHandle {
    let count = use_signal(|| 0);

    rsx! {
        div {
            button { onclick: move || count.update(|n| *n += 1), "Click" }
            span { {|| count.get().to_string()} }
        }
    }
}

fn main() {
    run("Pimble", 1200, 800, app);
}
```

### Phase 4: Rich-Text Editor Integration

**Replace cosmic_editor.rs with Rinch's built-in editor:**

Rinch'srinch-editable` crate ` provides CRDT-backed rich text:
- Automerge integration (already used in pimble-crdt)
- 22 formatting extensions (bold, italic, etc.)
- Markdown shortcuts
- Built-in toolbar

**Pattern:**
```rust
use rinch_editable::prelude::*;

#[component]
fn editor() -> NodeHandle {
    let editor = use_crdt_editor(|| Editor::new());

    rsx! {
        div {
            EditorComponent { editor: editor.clone() }
        }
    }
}
```

### Phase 5: Backend Integration

**Keep existing backend communication:**
- `BackendCommand` / `BackendEvent` enums (already in `backend.rs`)
- `AppState` / `TreeItem` (refactor to use Rinch signals)
- Channel communication with server unchanged

**Changes:**
- Replace `slint::Timer` polling with Rinch's reactive system
- `use_effect` for backend event processing

### Phase 6: Menu System

**Replace Slint menu generation with Rinch:**
- Rinch has built-in menu support via `rinch-platform`
- Native menus work on desktop

```rust
use rinch_platform::menu::*;

// Main menu via winit or native menu bars
```

### Phase 7: Window Management

**Replace Slint window API:**
- Rinch handles window internally via winit
- Window controls (minimize, maximize, close) via `rinch-platform`
- Drag/resize via CSS or native

## Files to Modify

### Delete (Slint artifacts)
```
crates/pimble-app/ui/app.slint
crates/pimble-app/ui/editor/cosmic_text_editor.slint
crates/pimble-app/ui/test.slint
crates/pimble-app/ui/theme.slint
crates/pimble-app/src/cosmic_editor.rs
crates/pimble-app/build.rs
```

### Modify
```
Cargo.toml                                    # Workspace deps
crates/pimble-app/Cargo.toml                 # Crate deps
crates/pimble-app/src/main.rs                 # Entry point
crates/pimble-app/src/app.rs                 # Main UI (rewrite)
crates/pimble-app/src/state.rs                # Adapt to Rinch signals
crates/pimble-app/src/backend.rs              # Keep, minor tweaks
```

### Create
```
crates/pimble-app/src/ui/components.rs        # Reusable components
crates/pimble-app/src/ui/editor.rs            # Rinch editor wrapper
```

## Dependencies After Migration

```toml
# Workspace
rinch = { git = "https://github.com/joeleaver/rinch.git", features = ["desktop", "components", "theme", "editor"] }

# pimble-app
pimble-core = { workspace = true }
pimble-client = { workspace = true }
pimble-rpc = { workspace = true }
pimble-crdt = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
tokio = { workspace = true }
tracing = { workspace = true }
tracing-subscriber = { workspace = true }
uuid = { workspace = true }
futures = { workspace = true }
crossbeam-channel = { workspace = true }
chrono = { workspace = true }
```

## Testing Strategy

1. **Build check**: `cargo check -p pimble-app`
2. **Run app**: `cargo run -p pimble-app`
3. **Verify**:
   - Window opens with Rinch UI
   - Connection to backend works
   - Tree panel displays stores/nodes
   - Rich-text editor loads and accepts input
   - Menus work

## Risks & Notes

- **Rinch API**: Based on the GitHub info, Rinch uses RSX macro similar to JSX
- **Editor**: Rinch's CRDT editor uses Automerge - same as pimble-crdt, so integration should be straightforward
- **No backwards compatibility**: User explicitly requested removing all Slint code
