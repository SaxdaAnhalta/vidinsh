//! Zeichenraster und die Renderer, die ein Frame darin ablegen.

pub mod blocks;
pub mod color;
pub mod edge;
pub mod geometry;
pub mod glyph;
pub mod ramp;

use crate::source::Frame;
use clap::ValueEnum;
use color::Rgb;
use geometry::Layout;

#[derive(Clone, Copy, PartialEq, Eq, Debug, ValueEnum)]
pub enum Mode {
    /// Helligkeit -> Zeichen aus einer Rampe
    Ramp,
    /// bestes Zeichen per Formvergleich
    Glyph,
    /// Halbblöcke und Quadranten
    Blocks,
    /// Kanten als | / - \
    Edge,
}

impl Mode {
    pub fn label(self) -> &'static str {
        match self {
            Mode::Ramp => "ramp",
            Mode::Glyph => "glyph",
            Mode::Blocks => "blocks",
            Mode::Edge => "edge",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, ValueEnum)]
pub enum Charset {
    /// nur 0x20..0x7E, läuft überall
    Ascii,
    /// ASCII plus gängige Unicode-Schattierungen
    Extended,
    /// Halbblöcke, 1x2 pro Zelle
    Half,
    /// Quadranten, 2x2 pro Zelle
    Quad,
    /// Sextanten, 2x3 pro Zelle -- Schriftabdeckung ist unsicher
    Sextant,
    /// Braille, 2x4 Punkte, aber nur eine Farbe pro Zelle
    Braille,
}

impl Charset {
    /// Braucht der Zeichensatz mehr als reines ASCII?
    pub fn needs_unicode(self) -> bool {
        !matches!(self, Charset::Ascii)
    }

    pub fn label(self) -> &'static str {
        match self {
            Charset::Ascii => "ascii",
            Charset::Extended => "extended",
            Charset::Half => "half",
            Charset::Quad => "quad",
            Charset::Sextant => "sextant",
            Charset::Braille => "braille",
        }
    }
}

pub const DEFAULT_RAMP: &str = " .:-=+*#%@";

/// Was die Renderer über die gewünschte Darstellung wissen müssen.
#[derive(Clone, Debug)]
pub struct RenderOpts {
    pub ramp: Vec<char>,
    pub charset: Charset,
    pub invert: bool,
    /// Hintergrundfarbe mitfärben (in `blocks` immer an)
    pub bg: bool,
    pub brightness: f32,
    pub contrast: f32,
    pub saturation: f32,
    pub auto_contrast: bool,
    /// ab welcher Kantenstärke der edge-Modus ein Richtungszeichen setzt
    pub edge_threshold: f32,
    /// Radius des kleineren der beiden Weichzeichner
    pub edge_sigma: f32,
}

impl Default for RenderOpts {
    fn default() -> Self {
        RenderOpts {
            ramp: DEFAULT_RAMP.chars().collect(),
            charset: Charset::Extended,
            invert: false,
            bg: false,
            brightness: 1.0,
            contrast: 1.0,
            saturation: 1.0,
            auto_contrast: false,
            edge_threshold: 0.10,
            edge_sigma: 1.0,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Cell {
    pub ch: char,
    pub fg: Rgb,
    pub bg: Option<Rgb>,
}

impl Default for Cell {
    fn default() -> Self {
        Cell {
            ch: ' ',
            fg: Rgb(0, 0, 0),
            bg: None,
        }
    }
}

/// Das Zeichenraster eines Frames. Der Writer vergleicht zwei davon,
/// um nur geänderte Zellen auszugeben.
#[derive(Clone, Debug)]
pub struct Grid {
    pub w: u16,
    pub h: u16,
    pub cells: Vec<Cell>,
}

impl Grid {
    pub fn new(w: u16, h: u16) -> Self {
        Grid {
            w,
            h,
            cells: vec![Cell::default(); w as usize * h as usize],
        }
    }

    /// Passt die Größe an und setzt alles zurück. Bewusst kein Erhalten des
    /// alten Inhalts: nach einem Resize ist der ohnehin wertlos.
    pub fn resize(&mut self, w: u16, h: u16) {
        self.w = w;
        self.h = h;
        self.cells.clear();
        self.cells.resize(w as usize * h as usize, Cell::default());
    }

    pub fn row(&self, y: u16) -> &[Cell] {
        let s = y as usize * self.w as usize;
        &self.cells[s..s + self.w as usize]
    }
}

pub trait Renderer: Send {
    fn render(&mut self, frame: &Frame, layout: &Layout, opts: &RenderOpts, grid: &mut Grid);
}

// ------------------------------------------------------- gemeinsame Bausteine

/// Mittelt ein Teilrechteck in linearem Licht und gibt lineares RGB zurück.
///
/// Der Aufrufer garantiert, dass das Rechteck im Frame liegt -- das Raster ist
/// per Konstruktion ein exaktes Vielfaches der Abtastung.
#[inline]
pub fn tile_mean_linear(frame: &Frame, x0: u32, y0: u32, w: u32, h: u32) -> (f32, f32, f32) {
    let (mut rs, mut gs, mut bs) = (0.0f32, 0.0f32, 0.0f32);
    let stride = frame.w as usize * 3;
    for y in y0..y0 + h {
        let base = y as usize * stride + x0 as usize * 3;
        for px in frame.data[base..base + w as usize * 3].chunks_exact(3) {
            rs += color::to_linear(px[0]);
            gs += color::to_linear(px[1]);
            bs += color::to_linear(px[2]);
        }
    }
    let n = (w * h) as f32;
    (rs / n, gs / n, bs / n)
}

/// Helligkeit, Kontrast und Sättigung -- alles in linearem Licht, damit die
/// Reihenfolge der Operationen keine Rolle spielt. 0.18 ist Mittelgrau.
#[inline]
pub fn adjust_linear(mut r: f32, mut g: f32, mut b: f32, o: &RenderOpts) -> (f32, f32, f32) {
    if o.brightness != 1.0 {
        r *= o.brightness;
        g *= o.brightness;
        b *= o.brightness;
    }
    if o.contrast != 1.0 {
        r = (r - 0.18) * o.contrast + 0.18;
        g = (g - 0.18) * o.contrast + 0.18;
        b = (b - 0.18) * o.contrast + 0.18;
    }
    if o.saturation != 1.0 {
        let l = color::luma_linear(r, g, b);
        r = l + (r - l) * o.saturation;
        g = l + (g - l) * o.saturation;
        b = l + (b - l) * o.saturation;
    }
    (r.max(0.0), g.max(0.0), b.max(0.0))
}

/// Lineares RGB -> darstellbare Farbe.
#[inline]
pub fn to_rgb(r: f32, g: f32, b: f32) -> Rgb {
    Rgb(color::to_srgb(r), color::to_srgb(g), color::to_srgb(b))
}

/// Wahrnehmungsnahe Helligkeit 0..1 aus linearem RGB. Die Rampe ist nach
/// optischer Dichte sortiert, nicht nach Lichtmenge -- deshalb gamma-kodiert.
#[inline]
pub fn perceptual_luma(r: f32, g: f32, b: f32) -> f32 {
    color::to_srgb(color::luma_linear(r, g, b)) as f32 / 255.0
}

/// Grenzen des `i`-ten von `n` Unterfeldern über `ss` Pixel.
///
/// Ganzzahlig geteilt, damit auch krumme Verhältnisse aufgehen -- 8 Pixel auf
/// 3 Sextanten-Reihen ergibt 3/3/2 statt eines Rundungsfehlers. Ist die
/// Abtastung gröber als die Unterteilung, teilen sich Felder Pixel, statt
/// leer zu bleiben.
#[inline]
pub fn sub_bounds(ss: u32, n: u32, i: u32) -> (u32, u32) {
    let ss = ss.max(1);
    let lo = (i * ss / n).min(ss - 1);
    let hi = ((i + 1) * ss / n).max(lo + 1).min(ss);
    (lo, hi)
}

/// Lineare Mittelwerte der `nx * ny` Unterfelder einer Zelle, zeilenweise.
// Viele Argumente, aber bewusst: das hier läuft je Zelle und Bild. Ein
// Parameterobjekt zu bauen hieße, es in der heißen Schleife zu füllen.
#[allow(clippy::too_many_arguments)]
#[inline]
pub fn subtile_means(
    frame: &Frame,
    cx: u32,
    cy: u32,
    ssx: u32,
    ssy: u32,
    nx: u32,
    ny: u32,
    out: &mut [(f32, f32, f32)],
) {
    for sy in 0..ny {
        let (y0, y1) = sub_bounds(ssy, ny, sy);
        for sx in 0..nx {
            let (x0, x1) = sub_bounds(ssx, nx, sx);
            out[(sy * nx + sx) as usize] =
                tile_mean_linear(frame, cx * ssx + x0, cy * ssy + y0, x1 - x0, y1 - y0);
        }
    }
}

/// Helligkeit 0..1 -> Zeichen aus der Rampe.
#[inline]
pub fn ramp_char(l: f32, opts: &RenderOpts) -> char {
    let n = opts.ramp.len();
    if n == 0 {
        return ' ';
    }
    let l = if opts.invert { 1.0 - l } else { l };
    let i = (l.clamp(0.0, 1.0) * (n - 1) as f32 + 0.5) as usize;
    opts.ramp[i.min(n - 1)]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts() -> RenderOpts {
        RenderOpts::default()
    }

    #[test]
    fn rampe_trifft_beide_enden() {
        let o = opts();
        assert_eq!(ramp_char(0.0, &o), ' ');
        assert_eq!(ramp_char(1.0, &o), '@');
    }

    #[test]
    fn invert_dreht_die_rampe_um() {
        let mut o = opts();
        o.invert = true;
        assert_eq!(ramp_char(0.0, &o), '@');
        assert_eq!(ramp_char(1.0, &o), ' ');
    }

    #[test]
    fn leere_rampe_liefert_leerzeichen_statt_panik() {
        let mut o = opts();
        o.ramp.clear();
        assert_eq!(ramp_char(0.5, &o), ' ');
    }

    #[test]
    fn neutrale_anpassung_laesst_farben_unveraendert() {
        let o = opts();
        let (r, g, b) = adjust_linear(0.3, 0.4, 0.5, &o);
        assert!((r - 0.3).abs() < 1e-6 && (g - 0.4).abs() < 1e-6 && (b - 0.5).abs() < 1e-6);
    }

    #[test]
    fn saettigung_null_ergibt_grau() {
        let mut o = opts();
        o.saturation = 0.0;
        let (r, g, b) = adjust_linear(0.8, 0.2, 0.1, &o);
        assert!((r - g).abs() < 1e-6 && (g - b).abs() < 1e-6);
    }

    #[test]
    fn anpassung_bleibt_nicht_negativ() {
        let mut o = opts();
        o.brightness = 0.0;
        o.contrast = 4.0;
        let (r, g, b) = adjust_linear(0.5, 0.5, 0.5, &o);
        assert!(r >= 0.0 && g >= 0.0 && b >= 0.0);
    }

    #[test]
    fn grid_resize_setzt_alles_zurueck() {
        let mut g = Grid::new(4, 2);
        g.cells[0].ch = 'X';
        g.resize(3, 3);
        assert_eq!(g.cells.len(), 9);
        assert!(g.cells.iter().all(|c| c.ch == ' '));
    }
}
