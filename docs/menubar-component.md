---
planStatus:
  planId: plan-menubar-component
  title: Reusable MenuBar Component
  status: in-development
  planType: feature
  priority: high
  owner: developer
  tags:
    - ui
    - component
    - makepad
    - menubar
    - material-design
  created: "2026-01-16"
  updated: "2026-01-16T22:30:00.000Z"
  progress: 70
---
# Reusable MenuBar Component

## Goals
- Create a reusable, Material Design-style menu bar component for Makepad
- Support dropdown menus with items, separators, and submenus
- Display keyboard shortcuts alongside menu items
- Implement proper hover states and click-outside-to-close behavior
- Make it declarative and easy to configure
- Follow Material Design 3 menu patterns

## Overview

Pimble needs a proper menu bar that behaves like standard desktop applications (VS Code, Electron apps, native menus). The current implementation uses simple Labels which don't provide dropdown functionality. We need a complete menu system that:

1. Renders top-level menu items (File, Edit, View, etc.) in the caption bar
2. Opens dropdown menus on click
3. Supports nested submenus with arrow indicators
4. Shows keyboard shortcuts right-aligned
5. Has proper visual feedback (hover, active states)
6. Closes when clicking outside or pressing Escape

## Architecture

### Data Structures

```rust
// Menu definition structures
#[derive(Clone, Debug)]
pub enum MenuItemKind {
    Action {
        label: String,
        shortcut: Option<String>,  // e.g., "Ctrl+S"
        action_id: LiveId,
        enabled: bool,
    },
    Separator,
    Submenu {
        label: String,
        items: Vec<MenuItemKind>,
    },
}

#[derive(Clone, Debug)]
pub struct MenuDefinition {
    pub label: String,
    pub items: Vec<MenuItemKind>,
}
```

### Component Hierarchy

```
MenuBar (horizontal bar with menu triggers)
├── MenuBarItem (clickable "File", "Edit", etc.)
│   └── MenuDropdown (popup overlay)
│       ├── MenuItem (clickable action item)
│       ├── MenuSeparator (horizontal line)
│       └── MenuSubmenuItem (has arrow, opens submenu)
│           └── MenuDropdown (nested)
```

### Widget Structure

```rust
#[derive(Live, LiveHook, Widget)]
pub struct MenuBar {
    #[deref] view: View,
    #[rust] menus: Vec<MenuDefinition>,
    #[rust] open_menu: Option<usize>,  // Which top-level menu is open
    #[rust] open_submenu_path: Vec<usize>,  // Path to open submenus
}
```

## Implementation Details

### Phase 1: Basic MenuBar and Dropdown

**File: \****`crates/pimble-app/src/ui/menu_bar.rs`**

1. Create `MenuBar` widget that renders horizontal menu items
2. Track which menu is currently open (if any)
3. Render dropdown as overlay positioned below the clicked item
4. Handle click-outside to close

**DSL Usage:**
```rust
menu_bar = <MenuBar> {
    height: 32,
    draw_bg: { color: #181825 }
}

// Configure in Rust:
fn handle_startup(&mut self, cx: &mut Cx) {
    self.ui.menu_bar(id!(menu_bar)).set_menus(cx, vec![
        MenuDefinition {
            label: "File".into(),
            items: vec![
                MenuItemKind::Action { label: "New".into(), shortcut: Some("Ctrl+N".into()), .. },
                MenuItemKind::Action { label: "Open...".into(), shortcut: Some("Ctrl+O".into()), .. },
                MenuItemKind::Separator,
                MenuItemKind::Action { label: "Save".into(), shortcut: Some("Ctrl+S".into()), .. },
            ],
        },
        // ... more menus
    ]);
}
```

### Phase 2: Dropdown Rendering (Material Design 3)

**Dropdown Container (MD3 Menu Surface):**
- Background: Use theme `COLOR_MENU_BG` (#252526)
- Border: 1px `COLOR_BORDER` (#454545)
- Border radius: 4px (MD3 uses subtle rounding)
- Elevation: Level 2 shadow (0 3px 6px rgba(0,0,0,0.3))
- Min width: 112dp (MD3 spec)
- Max width: 280dp
- Padding: 8dp vertical (MD3 spec)

**MenuItem Styling (MD3 Menu Item):**
- Height: 48dp (MD3 standard) or 32dp (dense)
- Padding: 12dp horizontal (MD3 spec)
- /Focus: `COLOR_HOVER` (#2a2d2e)
- Selected: `COLOR_SELECTED` (#094771)
- Text: `COLOR_TEXT` (#cccccc)
- Shortcut text: `COLOR_TEXT_MUTED` (#858585)
- Disabled: `COLOR_TEXT_INACTIVE` (#6e6e6e)
- Leading icon: 24dp, 12dp trailing margin
- Trailing icon (submenu arrow): 24dp

**Separator Styling (MD3 Divider):**
- Height: 1px
- : `COLOR_BORDER` (#454545)
- Margin: 8dp vertical
- Full width (no horizontal margin in MD3)

**Icons:**
- Use Material Design Icons (Pictogrammers)
- Size: 24x24 (MD3 standard)
- Submenu arrow: `chevron-right` icon

### Phase 3: Submenus

- Submenu items show `>` arrow on right side
- Hovering opens submenu after 200ms delay
- Submenu positioned to the right of parent item
- If no room on right, position on left

### Phase 4: Keyboard Navigation

- Arrow keys navigate within dropdown
- Enter/Space activates item
- Escape closes menu
- Left/Right arrows navigate between top-level menus
- Type-ahead: typing letters jumps to matching items

### Phase 5: Actions

```rust
#[derive(Clone, Debug, DefaultNone)]
pub enum MenuBarAction {
    None,
    ItemClicked(LiveId),  // The action_id of the clicked item
}

// In App:
fn handle_actions(&mut self, cx: &mut Cx, actions: &Actions) {
    for action in actions {
        if let MenuBarAction::ItemClicked(action_id) = action.cast() {
            match action_id {
                live_id!(file_new) => self.new_file(cx),
                live_id!(file_open) => self.open_file(cx),
                live_id!(file_save) => self.save_file(cx),
                // ...
            }
        }
    }
}
```

## Visual Reference

```
┌─────────────────────────────────────────────────────────────┐
│ Pimble   File  Edit  View  Window  Help           — □ ✕    │
├─────────┬───────────────────────────────────────────────────┤
│         │ New Window              ⇧⌘N │                     │
│         │ New File                 ⌘N │                     │
│         │ Open...                  ⌘O │                     │
│         │ Open Recent              ▸  │┌───────────────────┐│
│         │ Reopen Project           ▸  ││ darwin.cson       ││
│         │───────────────────────────│ │ Clear Menu        ││
│         │ Save                     ⌘S │└───────────────────┘│
│         │ Save As...              ⇧⌘S │                     │
│         │───────────────────────────│                       │
│         │ Close Tab                ⌘W │                     │
│         │ Close Window            ⇧⌘W │                     │
│         └─────────────────────────────┘                     │
```

## File Structure

```
crates/pimble-app/src/
├── ui/
│   ├── mod.rs              # Add menu_bar module
│   ├── menu_bar.rs         # MenuBar widget
│   └── menu_dropdown.rs    # Dropdown popup widget
└── app.rs                  # Use MenuBar in caption_bar
```

## Acceptance Criteria

- [x] MenuBar renders horizontal menu items in caption bar
- [x] Clicking a menu item opens its dropdown
- [x] Dropdown displays menu items with labels
- [x] Keyboard shortcuts display right-aligned in dropdown
- [x] Separators render as horizontal lines (via height change)
- [ ] Hover highlights menu items (shader-based hover pending)
- [x] Clicking outside closes the dropdown
- [x] Pressing Escape closes the dropdown
- [ ] Submenus open on hover with arrow indicator (future)
- [x] Clicking a menu item emits an action
- [ ] Menu items can be disabled (grayed out - styling pending)
- [x] Keyboard navigation works (arrows, enter, escape)
- [x] Works correctly with custom titlebar

## Notes

- Consider using Makepad's `Popup` or overlay system for dropdowns
- May need to handle z-index/layering for dropdowns over content
- Submenus need collision detection to flip direction if needed
- Should integrate with global keyboard shortcuts system later
