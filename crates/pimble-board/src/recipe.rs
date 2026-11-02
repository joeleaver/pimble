//! How a new canvas starts (docs/CANVAS_CONTRACT.md), and the sizes new
//! elements take when only a point is given: one definition for the app and
//! the MCP, so a canvas an agent makes is the canvas a person would have made.

use crate::doc::{BoardTxn, Result};
use crate::model::{Geom, Kind, NewElement, PictureGeom, Placement, Rect};

/// The icon a new canvas node starts with (the node's ordinary custom icon,
/// so the person can change it): an empty canvas.
pub const CANVAS_ICON: &str = "chalkboard";
/// The icon of a canvas node made from a picture.
pub const PICTURE_CANVAS_ICON: &str = "photo";
/// The name of the locked layer a canvas made from a picture holds it in.
pub const PICTURE_LAYER_NAME: &str = "Picture";

/// A sticky note's side when it is placed with a click.
pub const STICKY_SIZE: f32 = 200.0;
/// A node card's size when it is placed with a click.
pub const CARD_SIZE: (f32, f32) = (240.0, 56.0);

/// The icon a new canvas node takes.
pub fn canvas_icon(from_picture: bool) -> &'static str {
    if from_picture {
        PICTURE_CANVAS_ICON
    } else {
        CANVAS_ICON
    }
}

/// The picture a canvas starts from: its `pimble-blob:` URL and pixel size.
#[derive(Debug, Clone, Copy)]
pub struct StartingPicture<'a> {
    pub src: &'a str,
    pub px_w: u32,
    pub px_h: u32,
}

/// What [`BoardTxn::start_canvas`] made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewCanvas {
    /// "Layer 1", where marks go.
    pub draw_layer: String,
    /// The picture's locked layer and the picture's element, when it started
    /// from one.
    pub picture: Option<(String, String)>,
}

impl BoardTxn<'_, '_> {
    /// Make a new canvas, empty or starting from a picture: the picture at
    /// one unit per pixel in a locked layer "Picture" of its own and "Layer 1"
    /// above it to draw in. Every canvas starts this way, as a node or in
    /// text. The picture's layer is locked last, once the picture is in it.
    pub fn start_canvas(&mut self, picture: Option<StartingPicture<'_>>) -> Result<NewCanvas> {
        let draw_layer = self.create()?;
        let picture = match picture {
            Some(p) => {
                let layer = self.add_layer(PICTURE_LAYER_NAME, Placement::Bottom)?;
                let geom = PictureGeom {
                    rect: Rect::new(0.0, 0.0, p.px_w as f32, p.px_h as f32),
                    src: p.src.to_string(),
                    px_w: p.px_w,
                    px_h: p.px_h,
                    crop: None,
                };
                let element = self.add_element(NewElement::new(Kind::Picture, Geom::Picture(geom)).in_layer(layer.clone()))?;
                self.set_layer_locked(&layer, true)?;
                Some((layer, element))
            }
            None => None,
        };
        Ok(NewCanvas { draw_layer, picture })
    }
}
