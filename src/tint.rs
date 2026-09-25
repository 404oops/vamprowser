//! The window takes its hue from the page in the active tab: the page's
//! `theme-color`, else the colour along the top of the page, else the page
//! background, else the site's icon. A site with no colour anywhere leaves
//! the window neutral rather than inventing one.

use vampir::color::srgb_to_linear;

/// Returns the candidate colours, most specific first, as a JSON array of
/// `[r, g, b, a]` bytes. Painting through a canvas normalises every CSS
/// colour syntax (named, hex, `oklch()`, `color(display-p3 …)`) to sRGB.
pub const SAMPLE_SCRIPT: &str = r#"(() => {
  const canvas = document.createElement('canvas');
  canvas.width = canvas.height = 1;
  const g = canvas.getContext('2d', { willReadFrequently: true });
  const bytes = (color) => {
    if (!color || !CSS.supports('color', color)) return null;
    g.clearRect(0, 0, 1, 1);
    g.fillStyle = color;
    g.fillRect(0, 0, 1, 1);
    return Array.from(g.getImageData(0, 0, 1, 1).data);
  };
  const background = (el) => {
    for (; el && el.nodeType === 1; el = el.parentElement) {
      const color = bytes(getComputedStyle(el).backgroundColor);
      if (color && color[3] > 200) return color;
    }
    return null;
  };
  const meta = [...document.querySelectorAll('meta[name="theme-color"]')]
    .find((m) => !m.media || matchMedia(m.media).matches);
  const top = document.elementFromPoint(innerWidth / 2, 8);
  return JSON.stringify([
    bytes(meta && meta.content),
    background(top),
    document.body && background(document.body),
    background(document.documentElement),
  ].filter(Boolean));
})()"#;

/// Below this OKLCH chroma a colour reads as grey and says nothing about hue.
const MIN_CHROMA: f64 = 0.035;
/// The chroma that maps to the toolkit's default saturation. Vivid colours
/// run above it, muted ones below.
const DEFAULT_CHROMA: f64 = 0.12;

/// The theme colour a page or icon asks for: which hue, and how vividly.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tint {
    pub hue: f64,
    /// For [`vampir::Theme::saturation`]: 1 is the toolkit's default.
    pub saturation: f64,
}

impl Tint {
    fn new(chroma: f64, hue: f64) -> Self {
        Self {
            hue,
            // Never quite as loud as the page itself, never so faint that
            // the hue stops reading.
            saturation: (chroma / DEFAULT_CHROMA).clamp(0.5, 1.8),
        }
    }
}

/// The tint the script's result asks for, if any candidate is colourful.
pub fn tint_from_sample(result: &str) -> Option<Tint> {
    // WebKit hands back the script's return value JSON-encoded, so the
    // string the script built arrives quoted once more.
    let inner = serde_json::from_str::<String>(result).unwrap_or_else(|_| result.to_owned());
    let colors: Vec<[u8; 4]> = serde_json::from_str(&inner).ok()?;
    colors
        .into_iter()
        .filter(|[_, _, _, a]| *a > 200)
        .find_map(|[r, g, b, _]| {
            let (chroma, hue) = oklch(r, g, b);
            (chroma >= MIN_CHROMA).then(|| Tint::new(chroma, hue))
        })
}

/// The tint most of an image's colourful pixels share, if enough of it is
/// colourful: an icon that is mostly black, white or grey has none.
pub fn dominant_tint(pixels: impl Iterator<Item = [u8; 4]>) -> Option<Tint> {
    // Chroma-weighted votes into 10° bins, then the circular mean of the
    // winning bin and its neighbours so a hue on a bin edge isn't split.
    // The fourth field counts pixels, for the bin's mean chroma.
    let mut bins = [(0.0f64, 0.0f64, 0.0f64, 0u32); 36];
    let (mut opaque, mut colourful) = (0u32, 0u32);
    for [r, g, b, a] in pixels {
        if a < 128 {
            continue;
        }
        opaque += 1;
        let (chroma, hue) = oklch(r, g, b);
        if chroma < 0.06 {
            continue;
        }
        colourful += 1;
        let bin = &mut bins[(hue / 10.0) as usize % 36];
        let radians = hue.to_radians();
        bin.0 += chroma;
        bin.1 += chroma * radians.cos();
        bin.2 += chroma * radians.sin();
        bin.3 += 1;
    }
    if opaque == 0 || f64::from(colourful) < f64::from(opaque) * 0.1 {
        return None;
    }
    let best = (0..36).max_by(|&a, &b| bins[a].0.total_cmp(&bins[b].0))?;
    let (x, y, total, count) =
        [35, 0, 1]
            .iter()
            .fold((0.0, 0.0, 0.0, 0), |(x, y, total, count), step| {
                let bin = bins[(best + step) % 36];
                (x + bin.1, y + bin.2, total + bin.0, count + bin.3)
            });
    let hue = y.atan2(x).to_degrees().rem_euclid(360.0);
    Some(Tint::new(total / f64::from(count.max(1)), hue))
}

/// OKLCH chroma and hue in degrees of an sRGB colour.
fn oklch(r: u8, g: u8, b: u8) -> (f64, f64) {
    let [r, g, b] = [r, g, b].map(|v| srgb_to_linear(f64::from(v) / 255.0));
    let l = (0.412_221_470_8 * r + 0.536_332_536_3 * g + 0.051_445_992_9 * b).cbrt();
    let m = (0.211_903_498_2 * r + 0.680_699_545_1 * g + 0.107_396_956_6 * b).cbrt();
    let s = (0.088_302_461_9 * r + 0.281_718_837_6 * g + 0.629_978_700_5 * b).cbrt();
    let a = 1.977_998_495_1 * l - 2.428_592_205 * m + 0.450_593_709_9 * s;
    let b = 0.025_904_037_1 * l + 0.782_771_766_2 * m - 0.808_675_766 * s;
    (a.hypot(b), b.atan2(a).to_degrees().rem_euclid(360.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_the_first_colourful_candidate() {
        // White and a translucent red are skipped; the olive wins.
        let json = r#""[[255,255,255,255],[255,0,0,40],[164,176,36,255]]""#;
        let tint = tint_from_sample(json).unwrap();
        assert!((100.0..130.0).contains(&tint.hue), "{tint:?}");
    }

    #[test]
    fn grey_pages_have_no_hue() {
        assert_eq!(tint_from_sample("[[250,250,250,255],[20,20,20,255]]"), None);
        assert_eq!(tint_from_sample("not json"), None);
    }

    #[test]
    fn icons_have_a_hue_only_if_colourful() {
        let orange = [240, 100, 20, 255];
        let black = [0, 0, 0, 255];
        let clear = [0, 0, 0, 0];
        let tint = dominant_tint([orange, orange, black, clear].into_iter()).unwrap();
        assert!((40.0..70.0).contains(&tint.hue), "{tint:?}");
        // A vivid orange asks for more than the default intensity, a
        // muted one for less.
        assert!(tint.saturation > 1.0, "{tint:?}");
        let muted = dominant_tint([[150, 120, 100, 255]; 4].into_iter());
        assert!(muted.is_none_or(|t| t.saturation < 1.0), "{muted:?}");
        // Black with a speck of colour, and fully transparent, have none.
        let mostly_black = std::iter::repeat_n(black, 40).chain([orange]);
        assert_eq!(dominant_tint(mostly_black), None);
        assert_eq!(dominant_tint([clear; 4].into_iter()), None);
    }

    #[test]
    fn primaries_land_on_their_hues() {
        assert!((oklch(255, 0, 0).1 - 29.2).abs() < 1.0);
        assert!((oklch(0, 0, 255).1 - 264.1).abs() < 1.0);
    }
}
