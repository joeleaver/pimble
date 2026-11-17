//! A board as a standalone SVG document (Save as SVG...): the same fills and
//! strokes the raster renderer draws ([`crate::paint`]'s ops), each path
//! serialized from tiny-skia's own segments, in z-order, each element in a
//! group carrying its `world` transform. Text is its glyph outlines, so the
//! document holds no text nodes and no user text in any attribute; pictures
//! are embedded as base64 data URIs.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::io::Cursor;

use base64::Engine as _;
use tiny_skia::PathSegment;
use tiny_skia::{FillRule, Path, Transform as Affine};

use crate::model::*;
use crate::paint::{element_ops, GradientPaint, Op, Space, StrokeStyle};
use crate::render::included;
use crate::snapshot::{BoardSnapshot, Element, SHADOW_OFFSET};

/// A number for SVG: at most three decimals, no trailing zeros.
fn n(v: f32) -> String {
    if !v.is_finite() {
        return "0".into();
    }
    let s = format!("{:.3}", v);
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" || s.is_empty() {
        "0".into()
    } else {
        s.to_string()
    }
}

/// SVG path data from a tiny-skia path's segments.
fn path_data(path: &Path) -> String {
    let mut d = String::new();
    for seg in path.segments() {
        match seg {
            PathSegment::MoveTo(p) => {
                let _ = write!(d, "M{} {}", n(p.x), n(p.y));
            }
            PathSegment::LineTo(p) => {
                let _ = write!(d, "L{} {}", n(p.x), n(p.y));
            }
            PathSegment::QuadTo(a, p) => {
                let _ = write!(d, "Q{} {} {} {}", n(a.x), n(a.y), n(p.x), n(p.y));
            }
            PathSegment::CubicTo(a, b, p) => {
                let _ = write!(d, "C{} {} {} {} {} {}", n(a.x), n(a.y), n(b.x), n(b.y), n(p.x), n(p.y));
            }
            PathSegment::Close => d.push('Z'),
        }
    }
    d
}

/// `#rrggbb` and the alpha the color carries times `opacity`.
fn paint(color: Color, opacity: f32) -> (String, f32) {
    (format!("#{:02x}{:02x}{:02x}", color.r, color.g, color.b), color.a as f32 / 255.0 * opacity.clamp(0.0, 1.0))
}

fn matrix(m: &Affine) -> String {
    format!("matrix({} {} {} {} {} {})", n(m.sx), n(m.ky), n(m.kx), n(m.sy), n(m.tx), n(m.ty))
}

/// The MIME type of picture bytes, by their magic numbers.
fn sniff_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        Some("image/png")
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF8") {
        Some("image/gif")
    } else if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

/// The pixel size encoded in picture bytes (read from the header only).
fn picture_size(bytes: &[u8]) -> Option<(u32, u32)> {
    image::ImageReader::new(Cursor::new(bytes)).with_guessed_format().ok()?.into_dimensions().ok()
}

/// A gradient's definition (in `<defs>`, just before the path that uses
/// it), in the path's own user space; answers its `url(#...)`.
fn write_gradient(out: &mut String, g: &GradientPaint, opacity: f32, next_id: &mut usize) -> String {
    *next_id += 1;
    let id = format!("pimble-gradient-{next_id}");
    let (from, to, radius) = g.points();
    out.push_str("<defs>");
    match g.gradient.kind {
        GradientKind::Linear => {
            let _ = write!(
                out,
                "<linearGradient id=\"{id}\" gradientUnits=\"userSpaceOnUse\" x1=\"{}\" y1=\"{}\" x2=\"{}\" y2=\"{}\">",
                n(from.x),
                n(from.y),
                n(to.x),
                n(to.y)
            );
        }
        GradientKind::Radial => {
            let _ = write!(out, "<radialGradient id=\"{id}\" gradientUnits=\"userSpaceOnUse\" cx=\"{}\" cy=\"{}\" r=\"{}\">", n(from.x), n(from.y), n(radius));
        }
    }
    for stop in &g.gradient.stops {
        let (hex, alpha) = paint(stop.color, opacity);
        let _ = write!(out, "<stop offset=\"{}\" stop-color=\"{}\"", n(stop.offset.clamp(0.0, 1.0)), hex);
        if alpha < 1.0 {
            let _ = write!(out, " stop-opacity=\"{}\"", n(alpha));
        }
        out.push_str("/>");
    }
    out.push_str(match g.gradient.kind {
        GradientKind::Linear => "</linearGradient>",
        GradientKind::Radial => "</radialGradient>",
    });
    out.push_str("</defs>");
    format!("url(#{id})")
}

fn write_op(out: &mut String, op: &Op<'_>, next_id: &mut usize) {
    match op {
        Op::Fill { path, gradient: Some(g), opacity, rule, .. } => {
            let url = write_gradient(out, g, *opacity, next_id);
            let _ = write!(out, "<path d=\"{}\" fill=\"{}\"", path_data(path.get()), url);
            if *rule == FillRule::EvenOdd {
                out.push_str(" fill-rule=\"evenodd\"");
            }
            out.push_str("/>");
        }
        Op::Fill { path, color, opacity, rule, shadow, multiply, .. } => {
            let (hex, alpha) = paint(*color, *opacity);
            let _ = write!(out, "<path d=\"{}\" fill=\"{}\"", path_data(path.get()), hex);
            if alpha < 1.0 {
                let _ = write!(out, " fill-opacity=\"{}\"", n(alpha));
            }
            if *rule == FillRule::EvenOdd {
                out.push_str(" fill-rule=\"evenodd\"");
            }
            if *shadow {
                let _ = write!(out, " transform=\"translate({} {})\"", n(SHADOW_OFFSET), n(SHADOW_OFFSET));
            }
            if *multiply {
                out.push_str(" style=\"mix-blend-mode:multiply\"");
            }
            out.push_str("/>");
        }
        Op::Stroke { path, color, gradient, opacity, stroke, multiply, .. } => {
            let (mut hex, mut alpha) = paint(*color, *opacity);
            if let Some(g) = gradient {
                hex = write_gradient(out, g, *opacity, next_id);
                alpha = 1.0;
            }
            let width = stroke.width.max(0.01);
            let _ = write!(
                out,
                "<path d=\"{}\" fill=\"none\" stroke=\"{}\" stroke-width=\"{}\" stroke-linecap=\"{}\" stroke-linejoin=\"{}\"",
                path_data(path.get()),
                hex,
                n(width),
                stroke.cap().as_str(),
                stroke.join.as_str()
            );
            if stroke.join == Join::Miter && (stroke.miter - DEFAULT_MITER).abs() > 1e-4 {
                let _ = write!(out, " stroke-miterlimit=\"{}\"", n(stroke.miter));
            }
            if alpha < 1.0 {
                let _ = write!(out, " stroke-opacity=\"{}\"", n(alpha));
            }
            if let Some(v) = (StrokeStyle { width, ..stroke.clone() }).intervals() {
                let list: Vec<String> = v.iter().map(|x| n(*x)).collect();
                let _ = write!(out, " stroke-dasharray=\"{}\"", list.join(" "));
            }
            if *multiply {
                out.push_str(" style=\"mix-blend-mode:multiply\"");
            }
            out.push_str("/>");
        }
        Op::Picture(_) => {}
    }
}

fn write_picture(out: &mut String, e: &Element, p: &PictureGeom, picture_bytes: &dyn Fn(&str) -> Option<Vec<u8>>) {
    let r = p.rect.normalized();
    let bytes = picture_bytes(&p.src);
    let embedded = bytes.as_deref().and_then(|b| sniff_mime(b).map(|m| (m, b)));
    let Some((mime, bytes)) = embedded else {
        // Not available: the renderer's placeholder.
        let _ = write!(
            out,
            "<rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" fill=\"#e0e0e0\" stroke=\"#9e9e9e\" stroke-width=\"1\" vector-effect=\"non-scaling-stroke\"",
            n(r.x),
            n(r.y),
            n(r.w),
            n(r.h)
        );
        if e.opacity < 1.0 {
            let _ = write!(out, " opacity=\"{}\"", n(e.opacity));
        }
        out.push_str("/>");
        return;
    };
    let size = if p.px_w > 0 && p.px_h > 0 { Some((p.px_w, p.px_h)) } else { picture_size(bytes) };
    let data = base64::engine::general_purpose::STANDARD.encode(bytes);
    let opacity = if e.opacity < 1.0 { format!(" opacity=\"{}\"", n(e.opacity)) } else { String::new() };
    match size {
        Some((pw, ph)) => {
            let crop = p
                .crop
                .map(|c| c.normalized())
                .filter(|c| c.w > 0.0 && c.h > 0.0)
                .unwrap_or(Rect::new(0.0, 0.0, pw as f32, ph as f32));
            // A nested viewport: the crop in the picture's pixels, stretched
            // over the element's rectangle and clipped to it.
            let _ = write!(
                out,
                "<svg x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" viewBox=\"{} {} {} {}\" preserveAspectRatio=\"none\" overflow=\"hidden\"{}>\
                 <image x=\"0\" y=\"0\" width=\"{}\" height=\"{}\" preserveAspectRatio=\"none\" xlink:href=\"data:{};base64,{}\"/></svg>",
                n(r.x),
                n(r.y),
                n(r.w),
                n(r.h),
                n(crop.x),
                n(crop.y),
                n(crop.w),
                n(crop.h),
                opacity,
                pw,
                ph,
                mime,
                data
            );
        }
        None => {
            let _ = write!(
                out,
                "<image x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" preserveAspectRatio=\"none\"{} xlink:href=\"data:{};base64,{}\"/>",
                n(r.x),
                n(r.y),
                n(r.w),
                n(r.h),
                opacity,
                mime,
                data
            );
        }
    }
}

/// `rect` of the board as a standalone SVG document: `viewBox` is the
/// rectangle in board units, `width` and `height` its size (one pixel per
/// board unit). Every element the renderer would draw, in z-order, with the
/// same geometry; `only` keeps just those elements and their members, as
/// [`crate::render_area`] does. `picture_bytes` answers a picture's blob
/// bytes by its `src` (PNG, JPEG, GIF or WebP); a picture it has no bytes
/// for is the renderer's grey placeholder. A board with a page draws the
/// page's fill first (unless `only` is given).
///
/// An element's opacity applies to each of its parts, as the renderer's
/// does, so it is on each part, not on the element's group. A layer or
/// group the renderer gives a buffer of its own (an opacity below 1, a
/// blend that is not normal) is a `<g>` carrying `opacity` and
/// `mix-blend-mode` around its elements.
pub fn render_svg(
    snapshot: &BoardSnapshot,
    rect: Rect,
    only: Option<&HashSet<String>>,
    picture_bytes: &dyn Fn(&str) -> Option<Vec<u8>>,
) -> String {
    let rect = rect.normalized();
    let mut out = String::new();
    let _ = write!(
        out,
        "<svg xmlns=\"http://www.w3.org/2000/svg\" xmlns:xlink=\"http://www.w3.org/1999/xlink\" version=\"1.1\" \
         width=\"{}\" height=\"{}\" viewBox=\"{} {} {} {}\">",
        n(rect.w),
        n(rect.h),
        n(rect.x),
        n(rect.y),
        n(rect.w),
        n(rect.h)
    );
    if let (Some(page), None) = (&snapshot.page, only) {
        let p = page.rect.normalized();
        let (hex, alpha) = paint(page.fill.unwrap_or(Color::WHITE), 1.0);
        let _ = write!(out, "<rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" fill=\"{}\"", n(p.x), n(p.y), n(p.w), n(p.h), hex);
        if alpha < 1.0 {
            let _ = write!(out, " fill-opacity=\"{}\"", n(alpha));
        }
        out.push_str("/>");
    }
    let mask = only.map(|ids| included(snapshot, ids));
    let mut gradients = 0usize;
    let mut open_layers: Vec<usize> = Vec::new();
    const NONE: &[usize] = &[];
    let chain_of = |layer: usize| snapshot.chains.get(layer).map_or(NONE, |c| c.as_slice());
    // Paint layers go in at their place in the layer order, each as one
    // picture (none when only some elements are written).
    let paint: Vec<usize> = if only.is_none() { snapshot.paint_layers_in(&rect) } else { Vec::new() };
    let mut next_paint = 0;
    let decoded = |src: &str| picture_bytes(src).and_then(|b| crate::render::Picture::decode(&b).ok()).map(std::sync::Arc::new);
    for i in snapshot.query(&rect) {
        if mask.as_ref().is_some_and(|m| !m[i]) {
            continue;
        }
        let e = &snapshot.elements[i];
        if let Some(l) = e.layer_index {
            while next_paint < paint.len() && paint[next_paint] < l {
                let layer = paint[next_paint];
                next_paint += 1;
                sync_chain(&mut out, &mut open_layers, chain_of(layer), snapshot);
                write_paint_layer(&mut out, snapshot, layer, &rect, &decoded);
            }
        }
        let ops = element_ops(e, &rect, None);
        if ops.is_empty() {
            continue;
        }
        let chain = e.layer_index.map_or(NONE, chain_of);
        sync_chain(&mut out, &mut open_layers, chain, snapshot);
        // Consecutive ops in one space share a group.
        let mut open: Option<Space> = None;
        for op in &ops {
            let space = match op {
                Op::Fill { space, .. } | Op::Stroke { space, .. } => *space,
                Op::Picture(_) => Space::Element,
            };
            if open != Some(space) {
                if open.is_some() {
                    out.push_str("</g>");
                }
                if space == Space::Element && !e.world.is_identity() {
                    let _ = write!(out, "<g transform=\"{}\">", matrix(&e.world));
                } else {
                    out.push_str("<g>");
                }
                open = Some(space);
            }
            match op {
                Op::Picture(p) => write_picture(&mut out, e, p, picture_bytes),
                _ => write_op(&mut out, op, &mut gradients),
            }
        }
        out.push_str("</g>");
    }
    for &layer in &paint[next_paint..] {
        sync_chain(&mut out, &mut open_layers, chain_of(layer), snapshot);
        write_paint_layer(&mut out, snapshot, layer, &rect, &decoded);
    }
    for _ in open_layers {
        out.push_str("</g>");
    }
    out.push_str("</svg>");
    out
}

/// Close the layer groups `chain` leaves and open the ones it enters (a
/// layer or group with a buffer of its own is a `<g>` with its opacity and
/// blend).
fn sync_chain(out: &mut String, open_layers: &mut Vec<usize>, chain: &[usize], snapshot: &BoardSnapshot) {
    let common = open_layers.iter().zip(chain).take_while(|(a, b)| a == b).count();
    while open_layers.len() > common {
        open_layers.pop();
        out.push_str("</g>");
    }
    for layer in &chain[open_layers.len()..] {
        let l = &snapshot.layers[*layer];
        out.push_str("<g");
        if l.opacity < 1.0 {
            let _ = write!(out, " opacity=\"{}\"", n(l.opacity));
        }
        if let Some(mode) = l.blend.css() {
            let _ = write!(out, " style=\"mix-blend-mode:{mode}\"");
        }
        out.push('>');
        open_layers.push(*layer);
    }
}

/// A paint layer as one embedded PNG over what it paints within `rect`, at
/// its own density (two pixels per board unit).
fn write_paint_layer(out: &mut String, snapshot: &BoardSnapshot, layer: usize, rect: &Rect, pictures: &dyn crate::render::PictureSource) {
    let id = &snapshot.layers[layer].id;
    let Some(rendered) = crate::render::render_paint_layer(snapshot, id, pictures, crate::brush::PX_PER_UNIT, Some(*rect)) else { return };
    let Ok(png) = rendered.image.encode_png() else { return };
    let r = rendered.rect;
    let _ = write!(
        out,
        "<image x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" preserveAspectRatio=\"none\" xlink:href=\"data:image/png;base64,{}\"/>",
        n(r.x),
        n(r.y),
        n(r.w),
        n(r.h),
        base64::engine::general_purpose::STANDARD.encode(png)
    );
}
