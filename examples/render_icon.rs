//! Renders `packaging/macos/icon.svg` into a macOS `.iconset` directory,
//! which `iconutil` turns into `AppIcon.icns`. Run by
//! `packaging/macos/icon.sh`.
//!
//! ```sh
//! cargo run --example render_icon -- packaging/macos/icon.svg target/AppIcon.iconset
//! ```

use std::{fs, path::PathBuf};

use resvg::{tiny_skia, usvg};

fn main() {
    let mut args = std::env::args().skip(1);
    let (Some(svg), Some(out)) = (args.next(), args.next()) else {
        eprintln!("usage: render_icon <icon.svg> <out.iconset>");
        std::process::exit(2);
    };
    let data = fs::read(&svg).expect("read svg");
    let tree = usvg::Tree::from_data(&data, &usvg::Options::default()).expect("parse svg");
    let out = PathBuf::from(out);
    fs::create_dir_all(&out).expect("create iconset");
    for (points, scale) in [16, 32, 128, 256, 512]
        .into_iter()
        .flat_map(|points| [(points, 1), (points, 2)])
    {
        let px = points * scale;
        let suffix = if scale == 2 { "@2x" } else { "" };
        let path = out.join(format!("icon_{points}x{points}{suffix}.png"));
        render(&tree, px).save(&path).expect("write png");
    }
    // For running unbundled, where the app sets its dock icon itself: the
    // tile alone, edge to edge, since the toolkit adds Apple's margin.
    render_region(&tree, 512, TILE)
        .save(out.with_file_name("icon-512.png"))
        .expect("write png");
}

/// Where the tile sits in the 1024-unit artwork: Apple's 824 grid.
const TILE: (f32, f32) = (100.0, 824.0);

fn render(tree: &usvg::Tree, px: u32) -> image::RgbaImage {
    render_region(tree, px, (0.0, tree.size().width()))
}

/// Renders the square of the artwork starting at `origin` and `side` units
/// across into `px` pixels.
fn render_region(tree: &usvg::Tree, px: u32, (origin, side): (f32, f32)) -> image::RgbaImage {
    let mut pixmap = tiny_skia::Pixmap::new(px, px).expect("pixmap");
    let scale = px as f32 / side;
    resvg::render(
        tree,
        tiny_skia::Transform::from_scale(scale, scale)
            .post_translate(-origin * scale, -origin * scale),
        &mut pixmap.as_mut(),
    );
    // tiny-skia keeps premultiplied alpha; PNG wants it straight.
    let mut data = pixmap.take();
    for p in data.chunks_exact_mut(4) {
        let a = u32::from(p[3]);
        if a > 0 && a < 255 {
            for c in &mut p[..3] {
                *c = ((u32::from(*c) * 255 + a / 2) / a).min(255) as u8;
            }
        }
    }
    image::RgbaImage::from_raw(px, px, data).expect("pixels")
}
