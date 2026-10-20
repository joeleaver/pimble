//! Pimble's mark and wordmark, drawn into the title bar.
//!
//! The pebble and the lowercase wordmark (Manrope Medium, outlined, so no font
//! file ships for it) are the same shapes as `assets/brand/` (Joe, 2026-10-20).
//! They are two inline SVGs in their own coordinates rather than one with
//! transforms. On the dark theme the pebble is the brand slate lifted for a dark
//! ground; on the light theme the lockup is the title bar's text color.

use rinch::prelude::*;

use crate::state::AppStore;

/// The pebble on its 64-unit grid; the hole winds the other way, so it stays
/// open under either fill rule.
const PEBBLE_D: &str = "M33 15.5C44.5 15.5 50.5 23 50.5 32.5C50.5 42.5 42.5 49.5 31 49.5C20.5 49.5 13.5 43 13.5 33C13.5 22.5 21.5 15.5 33 15.5ZM36.5 23.5a6 6 0 1 0 0.01 0Z";
const PEBBLE_VIEW_BOX: &str = "13 15 38 35";

/// "pimble" in Manrope at weight 500, font units (2000 per em), y down, tracked -0.02 em.
const WORD_D: &str = "M636 30Q484 30 380.0 -45.5Q276 -121 222.5 -250.25Q169.0 -379.5 169 -541Q169 -704 222.5 -832.75Q276.0 -961.5 380.25 -1035.75Q484.5 -1110.0 637 -1110Q787 -1110 895.25 -1035.25Q1003.5 -960.5 1061.75 -831.75Q1120 -703 1120 -541Q1120 -379 1061.75 -249.75Q1003.5 -120.5 895.25 -45.25Q787 30 636 30ZM139 480V-1080H298V-285H318V480ZM614 -133Q720.5 -133.0 790.75 -187.0Q861 -241 896.0 -333.5Q931 -426 931 -541Q931 -655 896.25 -747.0Q861.5 -839.0 790.25 -893.0Q719 -947 609 -947Q503.5 -947.0 434.5 -895.5Q365.5 -844.0 331.75 -752.25Q298.0 -660.5 298 -541Q298.0 -422.5 331.5 -330.25Q365 -238 435.0 -185.5Q505 -133 614 -133Z M1320.0 -1271V-1455H1498.0V-1271ZM1320.0 0V-1080H1498.0V0Z M3049.0 0 3050.0 -691Q3050.0 -812 2985.25 -880.5Q2920.5 -949.0 2818.0 -949Q2757.5 -949.0 2703.75 -921.25Q2650.0 -893.5 2616.0 -835.25Q2582.0 -777 2582.0 -686L2488.0 -724Q2485.5 -839.0 2535.0 -925.5Q2584.5 -1012.0 2670.75 -1060.0Q2757.0 -1108 2865.0 -1108Q3034.0 -1108 3131.0 -1005.25Q3228.0 -902.5 3228.0 -730L3227.0 0ZM1757.0 0V-1080H1916.0V-791H1936.0V0ZM2404.0 0 2405.0 -684Q2405.0 -807.5 2341.0 -878.25Q2277.0 -949 2171.0 -949Q2065.5 -949.0 2000.75 -876.75Q1936.0 -804.5 1936.0 -686L1842.0 -742Q1842.0 -847 1892.0 -929.75Q1942.0 -1012.5 2028.0 -1060.25Q2114.0 -1108 2223.0 -1108Q2332.0 -1108 2412.75 -1061.75Q2493.5 -1015.5 2537.75 -930.0Q2582.0 -844.5 2582.0 -727L2581.0 0Z M3944.0 30Q3792.0 30 3688.0 -45.5Q3584.0 -121 3530.5 -250.25Q3477.0 -379.5 3477.0 -541Q3477.0 -704 3530.5 -832.75Q3584.0 -961.5 3688.25 -1035.75Q3792.5 -1110.0 3945.0 -1110Q4095.0 -1110 4203.25 -1035.25Q4311.5 -960.5 4369.75 -831.75Q4428.0 -703 4428.0 -541Q4428.0 -379 4369.75 -249.75Q4311.5 -120.5 4203.25 -45.25Q4095.0 30 3944.0 30ZM3447.0 0V-1440H3626.0V-795H3606.0V0ZM3922.0 -133Q4028.5 -133.0 4098.75 -187.0Q4169.0 -241 4204.0 -333.5Q4239.0 -426 4239.0 -541Q4239.0 -655 4204.25 -747.0Q4169.5 -839.0 4098.25 -893.0Q4027.0 -947 3917.0 -947Q3811.5 -947.0 3742.5 -895.5Q3673.5 -844.0 3639.75 -752.25Q3606.0 -660.5 3606.0 -541Q3606.0 -422.5 3639.5 -330.25Q3673.0 -238 3743.0 -185.5Q3813.0 -133 3922.0 -133Z M4628.0 0V-1470H4806.0V0Z M5534.0 30Q5374.5 30.0 5256.0 -40.25Q5137.5 -110.5 5071.75 -237.25Q5006.0 -364 5006.0 -533Q5006.0 -710 5070.75 -839.5Q5135.5 -969.0 5252.25 -1039.5Q5369.0 -1110 5526.0 -1110Q5688.5 -1110.0 5803.0 -1035.0Q5917.5 -960.0 5975.25 -821.0Q6033.0 -682 6024.5 -490.0H5845.0V-554Q5842.0 -753 5764.0 -851.5Q5686.0 -950 5532.0 -950Q5366.5 -950.0 5280.75 -844.0Q5195.0 -738 5195.0 -540Q5195.0 -349 5280.75 -244.0Q5366.5 -139.0 5526.0 -139Q5633.0 -139 5712.0 -188.0Q5791.0 -237 5836.0 -329L6007.0 -270Q5943.5 -127.0 5817.25 -48.5Q5691.0 30 5534.0 30ZM5135.0 -490V-633H5933.0V-490Z";
const WORD_VIEW_BOX: &str = "139 -1470 5887 1950";

/// The brand slate, and the lighter one the site uses on dark grounds.
pub const SLATE: &str = "#3f6491";
pub const SLATE_ON_DARK: &str = "#7ba1d6";

fn svg_path(__scope: &mut RenderScope, view_box: &str, d: &str, height: f32, width: f32) -> NodeHandle {
    let svg = rsx! { svg { xmlns: "http://www.w3.org/2000/svg", fill: "currentColor" } };
    svg.set_attribute("viewBox", view_box);
    svg.set_attribute("width", &format!("{width:.1}"));
    svg.set_attribute("height", &format!("{height:.1}"));
    svg.set_attribute("style", &format!("width: {width:.1}px; height: {height:.1}px; flex-shrink: 0;"));
    let path = rsx! { path {} };
    path.set_attribute("d", d);
    path.set_attribute("fill-rule", "evenodd");
    svg.append_child(&path);
    svg
}

/// The lockup for the title bar's left section: the pebble, then the wordmark,
/// with the word 14 px tall.
pub fn titlebar_lockup(__scope: &mut RenderScope, store: AppStore) -> NodeHandle {
    // The word's box is the ink of "pimble": ascender to descender, 1950 units.
    let word_h = 14.0_f32;
    let em = word_h * 2000.0 / 1950.0;
    let word_w = word_h * 5887.0 / 1950.0;
    // The pebble is 0.875 em tall (34 of its view box's 35 units), 0.28 em before the word.
    let mark_h = em * 0.875 * 35.0 / 34.0;
    let mark_w = mark_h * 38.0 / 35.0;
    let mark = svg_path(__scope, PEBBLE_VIEW_BOX, PEBBLE_D, mark_h, mark_w);
    let word = svg_path(__scope, WORD_VIEW_BOX, WORD_D, word_h, word_w);
    // The gap is 0.28 em of the word, 4 px at this size (`.pimble-titlebar-brand`).
    debug_assert!((em * 0.28 - 4.0).abs() < 0.1);
    // The dark title bar is near-black: the pebble in the light slate, the word
    // in the body text color, apart from the menus' blue. The light one is
    // rinch's blue with white menus, where slate sinks: the whole lockup is
    // the title bar's text color there.
    rsx! {
        div {
            class: "pimble-titlebar-brand",
            style: {move || if store.dark_mode.get() { "color: var(--rinch-color-text);" } else { "color: var(--rinch-titlebar-text);" }},
            span {
                class: "pimble-titlebar-brand__mark",
                style: {move || if store.dark_mode.get() { format!("color: {SLATE_ON_DARK};") } else { String::new() }},
                {mark}
            }
            {word}
        }
    }
}
