//! The header of every page (ADR-0180): the MonitorFlow logo drawn as
//! vectors, the product name, and the report's title and period on the
//! right, over a thin rule. No image is embedded, so the file stays small.

use pdf_writer::types::{LineCapStyle, LineJoinStyle};

use super::{width, Doc, Rgb, HEIGHT, MARGIN, MUTED, RULE, WIDTH};

/// The logo's purple, the middle of its gradient.
const PURPLE: Rgb = (0.56, 0.33, 0.92);
/// The logo's side, in points.
const SIZE: f32 = 18.0;
/// Where the header's text sits.
const BASELINE: f32 = HEIGHT - 32.0;
/// Bézier handles of a quarter circle of radius 1.
const K: f32 = 0.552_284_8;

/// A quarter circle from the current point, around the corner
/// `(cx, cy)` of a rounded square, in SVG units.
fn quarter(doc: &mut Doc, from: (f32, f32), to: (f32, f32), corner: (f32, f32)) {
    let handle = |a: f32, c: f32| a + (c - a) * K;
    doc.page.cubic_to(
        handle(from.0, corner.0),
        handle(from.1, corner.1),
        handle(to.0, corner.0),
        handle(to.1, corner.1),
        to.0,
        to.1,
    );
}

/// The logo, its lower-left corner at `(x, y)`: a purple rounded square
/// with the white slime of `web/public/brand/slime-violette.svg`.
pub(super) fn logo(doc: &mut Doc, x: f32, y: f32) {
    let s = SIZE / 240.0;
    doc.page.save_state();
    // SVG units, y down, from the logo's top-left corner.
    doc.page.transform([s, 0.0, 0.0, -s, x, y + SIZE]);
    doc.page.set_fill_rgb(PURPLE.0, PURPLE.1, PURPLE.2);
    let r = 56.0;
    doc.page.move_to(r, 0.0);
    doc.page.line_to(240.0 - r, 0.0);
    quarter(doc, (240.0 - r, 0.0), (240.0, r), (240.0, 0.0));
    doc.page.line_to(240.0, 240.0 - r);
    quarter(doc, (240.0, 240.0 - r), (240.0 - r, 240.0), (240.0, 240.0));
    doc.page.line_to(r, 240.0);
    quarter(doc, (r, 240.0), (0.0, 240.0 - r), (0.0, 240.0));
    doc.page.line_to(0.0, r);
    quarter(doc, (0.0, r), (r, 0.0), (0.0, 0.0));
    doc.page.close_path();
    doc.page.fill_nonzero();

    // translate(120 124) scale(0.9) translate(-120 -122)
    doc.page.transform([0.9, 0.0, 0.0, 0.9, 12.0, 14.2]);
    doc.page.set_stroke_rgb(1.0, 1.0, 1.0);
    doc.page.set_fill_rgb(1.0, 1.0, 1.0);
    doc.page.set_line_cap(LineCapStyle::RoundCap);
    doc.page.set_line_join(LineJoinStyle::RoundJoin);
    doc.page.set_line_width(21.0);
    let body: [[f32; 6]; 9] = [
        [186.0, 72.0, 202.0, 98.0, 202.0, 126.0],
        [202.0, 140.0, 200.0, 154.0, 196.0, 164.0],
        [206.0, 167.0, 216.0, 171.0, 216.0, 179.0],
        [216.0, 188.0, 206.0, 192.0, 194.0, 192.0],
        [28.0, 192.0, 20.0, 184.0, 24.0, 176.0],
        [27.0, 170.0, 36.0, 167.0, 44.0, 164.0],
        [40.0, 154.0, 38.0, 140.0, 38.0, 126.0],
        [38.0, 80.0, 74.0, 46.0, 120.0, 46.0],
        [128.0, 46.0, 135.0, 47.0, 142.0, 49.0],
    ];
    doc.page.move_to(160.0, 58.0);
    for (i, [x1, y1, x2, y2, x3, y3]) in body.into_iter().enumerate() {
        if i == 4 {
            // H46: the flat bottom before the left side.
            doc.page.line_to(46.0, 192.0);
        }
        doc.page.cubic_to(x1, y1, x2, y2, x3, y3);
    }
    doc.page.stroke();
    // The eyes.
    doc.page.set_line_width(22.0);
    for eye in [95.0, 145.0] {
        doc.page.move_to(eye, 112.0);
        doc.page.line_to(eye, 126.0);
    }
    doc.page.stroke();
    // The drop above its head.
    let (cx, cy, r) = (168.0, 36.0, 14.0);
    doc.page.move_to(cx + r, cy);
    quarter(doc, (cx + r, cy), (cx, cy + r), (cx + r, cy + r));
    quarter(doc, (cx, cy + r), (cx - r, cy), (cx - r, cy + r));
    quarter(doc, (cx - r, cy), (cx, cy - r), (cx - r, cy - r));
    quarter(doc, (cx, cy - r), (cx + r, cy), (cx + r, cy - r));
    doc.page.close_path();
    doc.page.fill_nonzero();
    doc.page.restore_state();
}

/// The header of the current page: the logo and the name on the left,
/// `what` (the title and period) on the right.
pub(super) fn header(doc: &mut Doc, what: &str) {
    logo(doc, MARGIN, BASELINE - 5.0);
    let ink = super::INK;
    doc.text(
        MARGIN + SIZE + 7.0,
        BASELINE,
        12.0,
        true,
        ink,
        "MonitorFlow",
    );
    let room = WIDTH - 2.0 * MARGIN - SIZE - 120.0;
    let what = super::fit(what, 8.5, false, room);
    let at = WIDTH - MARGIN - width(&what, 8.5, false);
    doc.text(at, BASELINE + 1.0, 8.5, false, MUTED, &what);
    doc.page.set_stroke_rgb(RULE.0, RULE.1, RULE.2);
    doc.page.set_line_width(0.5);
    doc.page.move_to(MARGIN, BASELINE - 12.0);
    doc.page.line_to(WIDTH - MARGIN, BASELINE - 12.0);
    doc.page.stroke();
}
