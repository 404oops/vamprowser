//! Toolbar and tab icons drawn as paths on a 16-unit grid. Font glyphs sit
//! on their font's baseline and land a pixel or two off centre in a square
//! button; paths are centred exactly.

use gpui::{
    Bounds, IntoElement, PathBuilder, PathStyle, Pixels, Point, Rgba, StrokeOptions, Styled,
    Window, canvas, point, px,
};
use lyon_tessellation::{LineCap, LineJoin};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    Back,
    Forward,
    Reload,
    Bookmark,
    Sidebar,
    Star,
    StarFilled,
    Plus,
    Close,
    ChevronLeft,
    ChevronRight,
    Home,
    Private,
    Search,
    Link,
    Download,
    Media,
    Gear,
    Puzzle,
    File,
    Folder,
    Sound,
    SoundMuted,
}

/// `icon` drawn `size` points square in `color`.
pub fn icon(icon: Icon, size: f32, color: Rgba) -> impl IntoElement {
    canvas(
        |_, _, _| {},
        move |bounds, _, window, _| paint(icon, bounds, color, window),
    )
    .size(px(size))
    .flex_none()
}

fn paint(icon: Icon, bounds: Bounds<Pixels>, color: Rgba, window: &mut Window) {
    let scale = f32::from(bounds.size.width) / 16.0;
    let origin = bounds.origin;
    let at = |x: f32, y: f32| -> Point<Pixels> {
        point(origin.x + px(x * scale), origin.y + px(y * scale))
    };
    let stroke = || {
        PathBuilder::stroke(px(1.5 * scale)).with_style(PathStyle::Stroke(
            StrokeOptions::default()
                .with_line_width(1.5 * scale)
                .with_line_cap(LineCap::Round)
                .with_line_join(LineJoin::Round),
        ))
    };
    let polyline = |path: &mut PathBuilder, points: &[(f32, f32)]| {
        for (i, &(x, y)) in points.iter().enumerate() {
            if i == 0 {
                path.move_to(at(x, y));
            } else {
                path.line_to(at(x, y));
            }
        }
    };
    let mut paths = Vec::new();
    match icon {
        Icon::Back | Icon::Forward => {
            let flip = |x: f32| if icon == Icon::Back { x } else { 16.0 - x };
            let mut path = stroke();
            polyline(&mut path, &[(flip(12.5), 8.0), (flip(3.5), 8.0)]);
            polyline(
                &mut path,
                &[(flip(7.5), 4.0), (flip(3.5), 8.0), (flip(7.5), 12.0)],
            );
            paths.push(path);
        }
        Icon::Reload => {
            // A clockwise arc open at the upper right, with its head there.
            let (cx, cy, r) = (8.0, 8.0, 5.0);
            let arc: Vec<(f32, f32)> = (0..=24)
                .map(|step| {
                    let angle = (20.0 + step as f32 * 265.0 / 24.0).to_radians();
                    (cx + r * angle.cos(), cy + r * angle.sin())
                })
                .collect();
            let mut path = stroke();
            polyline(&mut path, &arc);
            paths.push(path);
            let end = (285.0f32).to_radians();
            let (ex, ey) = (cx + r * end.cos(), cy + r * end.sin());
            // Tangent of a clockwise (screen) sweep, and its normal.
            let (dx, dy) = (-end.sin(), end.cos());
            let (nx, ny) = (-dy, dx);
            let mut head = PathBuilder::fill();
            polyline(
                &mut head,
                &[
                    (ex + dx * 2.6, ey + dy * 2.6),
                    (ex + nx * 2.4 - dx * 0.6, ey + ny * 2.4 - dy * 0.6),
                    (ex - nx * 2.4 - dx * 0.6, ey - ny * 2.4 - dy * 0.6),
                ],
            );
            head.close();
            paths.push(head);
        }
        Icon::Bookmark => {
            let mut path = stroke();
            polyline(
                &mut path,
                &[
                    (4.5, 2.5),
                    (11.5, 2.5),
                    (11.5, 13.5),
                    (8.0, 10.5),
                    (4.5, 13.5),
                ],
            );
            path.close();
            paths.push(path);
        }
        Icon::Sidebar => {
            let mut path = stroke();
            rounded_rect(&mut path, &at, (2.25, 3.25), (13.75, 12.75), 2.0);
            polyline(&mut path, &[(6.5, 3.25), (6.5, 12.75)]);
            paths.push(path);
        }
        Icon::Star | Icon::StarFilled => {
            let points: Vec<(f32, f32)> = (0..10)
                .map(|i| {
                    let r = if i % 2 == 0 { 6.3 } else { 2.7 };
                    let angle = (-90.0 + i as f32 * 36.0).to_radians();
                    (8.0 + r * angle.cos(), 8.6 + r * angle.sin())
                })
                .collect();
            let mut path = if icon == Icon::StarFilled {
                PathBuilder::fill()
            } else {
                stroke()
            };
            polyline(&mut path, &points);
            path.close();
            paths.push(path);
        }
        Icon::Plus => {
            let mut path = stroke();
            polyline(&mut path, &[(8.0, 3.0), (8.0, 13.0)]);
            polyline(&mut path, &[(3.0, 8.0), (13.0, 8.0)]);
            paths.push(path);
        }
        Icon::Close => {
            let mut path = stroke();
            polyline(&mut path, &[(4.0, 4.0), (12.0, 12.0)]);
            polyline(&mut path, &[(12.0, 4.0), (4.0, 12.0)]);
            paths.push(path);
        }
        Icon::Sound | Icon::SoundMuted => {
            let mut speaker = stroke();
            polyline(&mut speaker, &[(2.0, 6.0), (4.5, 6.0), (8.0, 3.0), (8.0, 13.0), (4.5, 10.0), (2.0, 10.0), (2.0, 6.0)]);
            paths.push(speaker);
            if icon == Icon::SoundMuted {
                let mut slash = stroke();
                polyline(&mut slash, &[(10.0, 6.0), (14.0, 10.0)]);
                polyline(&mut slash, &[(14.0, 6.0), (10.0, 10.0)]);
                paths.push(slash);
            } else {
                let mut wave = stroke();
                polyline(&mut wave, &[(10.0, 5.5), (11.5, 8.0), (10.0, 10.5)]);
                polyline(&mut wave, &[(12.0, 3.5), (14.0, 8.0), (12.0, 12.5)]);
                paths.push(wave);
            }
        }
        Icon::ChevronLeft | Icon::ChevronRight => {
            let flip = |x: f32| {
                if icon == Icon::ChevronLeft {
                    x
                } else {
                    16.0 - x
                }
            };
            let mut path = stroke();
            polyline(
                &mut path,
                &[(flip(10.0), 3.5), (flip(5.5), 8.0), (flip(10.0), 12.5)],
            );
            paths.push(path);
        }
        Icon::Home => {
            let mut path = stroke();
            polyline(&mut path, &[(2.5, 7.5), (8.0, 2.75), (13.5, 7.5)]);
            polyline(
                &mut path,
                &[(4.25, 6.25), (4.25, 13.25), (11.75, 13.25), (11.75, 6.25)],
            );
            polyline(
                &mut path,
                &[(6.75, 13.25), (6.75, 9.5), (9.25, 9.5), (9.25, 13.25)],
            );
            paths.push(path);
        }
        Icon::Private => {
            // A masquerade mask: two eye holes under a brim.
            let mut path = stroke();
            polyline(&mut path, &[(2.0, 6.5), (14.0, 6.5)]);
            polyline(
                &mut path,
                &[(4.0, 6.5), (5.25, 3.5), (10.75, 3.5), (12.0, 6.5)],
            );
            paths.push(path);
            for cx in [5.25, 10.75] {
                let mut eye = stroke();
                let ring: Vec<(f32, f32)> = (0..=16)
                    .map(|i| {
                        let a = (i as f32 * 22.5).to_radians();
                        (cx + 2.1 * a.cos(), 10.5 + 1.9 * a.sin())
                    })
                    .collect();
                polyline(&mut eye, &ring);
                paths.push(eye);
            }
            let mut bridge = stroke();
            polyline(&mut bridge, &[(7.35, 10.0), (8.65, 10.0)]);
            paths.push(bridge);
        }
        Icon::Search => {
            let mut path = stroke();
            let ring: Vec<(f32, f32)> = (0..=24)
                .map(|i| {
                    let a = (i as f32 * 15.0).to_radians();
                    (7.0 + 4.25 * a.cos(), 7.0 + 4.25 * a.sin())
                })
                .collect();
            polyline(&mut path, &ring);
            polyline(&mut path, &[(10.1, 10.1), (13.5, 13.5)]);
            paths.push(path);
        }
        Icon::Link => {
            let mut path = stroke();
            rounded_rect(&mut path, &at, (2.0, 6.0), (9.0, 10.0), 2.0);
            paths.push(path);
            let mut other = stroke();
            rounded_rect(&mut other, &at, (7.0, 6.0), (14.0, 10.0), 2.0);
            paths.push(other);
        }
        Icon::Folder => {
            // A folder with its tab up at the left.
            let mut path = stroke();
            polyline(
                &mut path,
                &[
                    (2.5, 12.5),
                    (2.5, 4.0),
                    (6.25, 4.0),
                    (7.75, 5.5),
                    (13.5, 5.5),
                    (13.5, 12.5),
                    (2.5, 12.5),
                ],
            );
            polyline(&mut path, &[(2.5, 7.25), (13.5, 7.25)]);
            paths.push(path);
        }
        Icon::Download => {
            let mut path = stroke();
            polyline(&mut path, &[(8.0, 2.5), (8.0, 10.0)]);
            polyline(&mut path, &[(4.75, 7.0), (8.0, 10.25), (11.25, 7.0)]);
            polyline(
                &mut path,
                &[(3.0, 11.0), (3.0, 13.5), (13.0, 13.5), (13.0, 11.0)],
            );
            paths.push(path);
        }
        Icon::Media => {
            let mut play = stroke();
            polyline(
                &mut play,
                &[(2.5, 3.0), (2.5, 11.5), (9.0, 7.25), (2.5, 3.0)],
            );
            paths.push(play);
            let mut arrow = stroke();
            polyline(&mut arrow, &[(12.0, 5.0), (12.0, 12.5)]);
            polyline(&mut arrow, &[(9.5, 10.0), (12.0, 12.5), (14.5, 10.0)]);
            paths.push(arrow);
        }
        Icon::Gear => {
            // Eight teeth around a ring.
            let mut path = stroke();
            let rim: Vec<(f32, f32)> = (0..=48)
                .map(|i| {
                    let a = (i as f32 * 7.5).to_radians();
                    let tooth = (i / 3) % 2 == 0;
                    let r = if tooth { 6.0 } else { 4.6 };
                    (8.0 + r * a.cos(), 8.0 + r * a.sin())
                })
                .collect();
            polyline(&mut path, &rim);
            paths.push(path);
            let mut hub = stroke();
            let ring: Vec<(f32, f32)> = (0..=16)
                .map(|i| {
                    let a = (i as f32 * 22.5).to_radians();
                    (8.0 + 1.9 * a.cos(), 8.0 + 1.9 * a.sin())
                })
                .collect();
            polyline(&mut hub, &ring);
            paths.push(hub);
        }
        Icon::Puzzle => {
            let mut path = stroke();
            polyline(
                &mut path,
                &[
                    (3.0, 5.0),
                    (6.25, 5.0),
                    (6.25, 3.75),
                    (7.25, 2.75),
                    (8.25, 3.75),
                    (8.25, 5.0),
                    (11.5, 5.0),
                    (11.5, 8.0),
                    (12.75, 8.0),
                    (13.75, 9.0),
                    (12.75, 10.0),
                    (11.5, 10.0),
                    (11.5, 13.25),
                    (3.0, 13.25),
                ],
            );
            path.close();
            paths.push(path);
        }
        Icon::File => {
            let mut path = stroke();
            polyline(
                &mut path,
                &[
                    (4.0, 2.5),
                    (9.5, 2.5),
                    (12.5, 5.5),
                    (12.5, 13.5),
                    (4.0, 13.5),
                ],
            );
            path.close();
            polyline(&mut path, &[(9.5, 2.5), (9.5, 5.5), (12.5, 5.5)]);
            paths.push(path);
        }
    }
    for path in paths {
        if let Ok(path) = path.build() {
            window.paint_path(path, color);
        }
    }
}

fn rounded_rect(
    path: &mut PathBuilder,
    at: &impl Fn(f32, f32) -> Point<Pixels>,
    (x0, y0): (f32, f32),
    (x1, y1): (f32, f32),
    r: f32,
) {
    path.move_to(at(x0 + r, y0));
    path.line_to(at(x1 - r, y0));
    path.curve_to(at(x1, y0 + r), at(x1, y0));
    path.line_to(at(x1, y1 - r));
    path.curve_to(at(x1 - r, y1), at(x1, y1));
    path.line_to(at(x0 + r, y1));
    path.curve_to(at(x0, y1 - r), at(x0, y1));
    path.line_to(at(x0, y0 + r));
    path.curve_to(at(x0 + r, y0), at(x0, y0));
    path.close();
}
