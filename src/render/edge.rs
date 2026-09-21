//! Kantenmodus: Umrisse als `| / - \`, Flächen als Zeichenrampe.
//!
//! Zwei getrennte Fragen, zwei getrennte Werkzeuge:
//!
//! * **Wo ist eine Kante?** Difference of Gaussians -- zweimal weichzeichnen,
//!   einmal enger, einmal weiter, und die Differenz nehmen. Das reagiert auf
//!   echte Konturen und lässt sanfte Verläufe in Ruhe, anders als ein roher
//!   Sobel-Betrag, der auch in jedem Rauschen anschlägt.
//! * **In welche Richtung läuft sie?** Sobel liefert den Helligkeitsgradienten;
//!   die Kante steht senkrecht darauf.

use super::{
    Cell, Grid, RenderOpts, Renderer, adjust_linear, color::Rgb, geometry::Layout, perceptual_luma,
    ramp_char, tile_mean_linear, to_rgb,
};
use crate::source::Frame;
use rayon::prelude::*;

/// Verhältnis der beiden Weichzeichner. 1.6 ist der übliche Wert, bei dem die
/// Differenz einem Laplacian-of-Gaussian nahekommt.
const SIGMA_RATIO: f32 = 1.6;

pub struct EdgeRenderer {
    luma: Vec<f32>,
    a: Vec<f32>,
    b: Vec<f32>,
    tmp: Vec<f32>,
    farbe: Vec<Rgb>,
}

impl EdgeRenderer {
    pub fn new() -> Self {
        EdgeRenderer {
            luma: Vec::new(),
            a: Vec::new(),
            b: Vec::new(),
            tmp: Vec::new(),
            farbe: Vec::new(),
        }
    }
}

impl Default for EdgeRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl Renderer for EdgeRenderer {
    fn render(&mut self, frame: &Frame, layout: &Layout, opts: &RenderOpts, grid: &mut Grid) {
        if !frame.matches(layout) {
            return;
        }
        if grid.w != layout.grid_w || grid.h != layout.grid_h {
            grid.resize(layout.grid_w, layout.grid_h);
        }

        let (gw, gh) = (layout.grid_w as usize, layout.grid_h as usize);
        let n = gw * gh;
        let (ssx, ssy) = (layout.ss_x, layout.ss_y);

        self.luma.clear();
        self.luma.resize(n, 0.0);
        self.farbe.clear();
        self.farbe.resize(n, Rgb(0, 0, 0));

        // Helligkeit und Farbe je Zelle.
        self.luma
            .par_chunks_mut(gw)
            .zip(self.farbe.par_chunks_mut(gw))
            .enumerate()
            .for_each(|(cy, (lrow, frow))| {
                for cx in 0..gw {
                    let (r, g, b) =
                        tile_mean_linear(frame, cx as u32 * ssx, cy as u32 * ssy, ssx, ssy);
                    let (r, g, b) = adjust_linear(r, g, b, opts);
                    lrow[cx] = perceptual_luma(r, g, b);
                    frow[cx] = to_rgb(r, g, b);
                }
            });

        let sigma = opts.edge_sigma.clamp(0.3, 8.0);
        self.a.clear();
        self.a.extend_from_slice(&self.luma);
        self.b.clear();
        self.b.extend_from_slice(&self.luma);
        self.tmp.clear();
        self.tmp.resize(n, 0.0);

        blur(&mut self.a, &mut self.tmp, gw, gh, sigma);
        blur(&mut self.b, &mut self.tmp, gw, gh, sigma * SIGMA_RATIO);

        let schwelle = opts.edge_threshold.max(0.0);

        grid.cells
            .par_chunks_mut(gw)
            .enumerate()
            .for_each(|(cy, row)| {
                for (cx, cell) in row.iter_mut().enumerate() {
                    let i = cy * gw + cx;
                    // Differenz der beiden Weichzeichner: hoher Betrag = Kontur.
                    let dog = (self.a[i] - self.b[i]).abs();

                    let ch = if dog > schwelle {
                        let (gx, gy) = sobel(&self.a, gw, gh, cx, cy);
                        richtung(gx, gy)
                    } else {
                        ramp_char(self.luma[i], opts)
                    };

                    *cell = Cell {
                        ch,
                        fg: self.farbe[i],
                        bg: None,
                    };
                }
            });
    }
}

/// Separabler Gauß. Zwei Durchgänge über je `2r+1` Stützstellen; bei den
/// Größenordnungen hier (ein paar tausend Zellen) ist das nicht messbar.
fn blur(buf: &mut [f32], tmp: &mut [f32], w: usize, h: usize, sigma: f32) {
    let r = (sigma * 3.0).ceil().max(1.0) as isize;
    let kern: Vec<f32> = (-r..=r)
        .map(|i| (-(i * i) as f32 / (2.0 * sigma * sigma)).exp())
        .collect();
    let summe: f32 = kern.iter().sum();
    let kern: Vec<f32> = kern.iter().map(|k| k / summe).collect();

    // waagerecht
    for y in 0..h {
        for x in 0..w {
            let mut acc = 0.0;
            for (k, &kv) in kern.iter().enumerate() {
                let sx = (x as isize + k as isize - r).clamp(0, w as isize - 1) as usize;
                acc += buf[y * w + sx] * kv;
            }
            tmp[y * w + x] = acc;
        }
    }
    // senkrecht
    for y in 0..h {
        for x in 0..w {
            let mut acc = 0.0;
            for (k, &kv) in kern.iter().enumerate() {
                let sy = (y as isize + k as isize - r).clamp(0, h as isize - 1) as usize;
                acc += tmp[sy * w + x] * kv;
            }
            buf[y * w + x] = acc;
        }
    }
}

#[inline]
fn at(v: &[f32], w: usize, h: usize, x: isize, y: isize) -> f32 {
    let x = x.clamp(0, w as isize - 1) as usize;
    let y = y.clamp(0, h as isize - 1) as usize;
    v[y * w + x]
}

#[inline]
fn sobel(v: &[f32], w: usize, h: usize, cx: usize, cy: usize) -> (f32, f32) {
    let (x, y) = (cx as isize, cy as isize);
    let p = |dx: isize, dy: isize| at(v, w, h, x + dx, y + dy);
    let gx = (p(1, -1) + 2.0 * p(1, 0) + p(1, 1)) - (p(-1, -1) + 2.0 * p(-1, 0) + p(-1, 1));
    let gy = (p(-1, 1) + 2.0 * p(0, 1) + p(1, 1)) - (p(-1, -1) + 2.0 * p(0, -1) + p(1, -1));
    (gx, gy)
}

/// Gradient -> Zeichen. Die Kante steht senkrecht auf dem Gradienten, also
/// ist ihre Richtung `(-gy, gx)`. Gerechnet wird in Bildschirmkoordinaten,
/// in denen y nach unten wächst -- deshalb ist rechts-und-runter ein `\`.
fn richtung(gx: f32, gy: f32) -> char {
    if gx == 0.0 && gy == 0.0 {
        return '-';
    }
    let mut phi = gx.atan2(-gy);
    let pi = std::f32::consts::PI;
    while phi < 0.0 {
        phi += pi;
    }
    while phi >= pi {
        phi -= pi;
    }
    let sektor = (phi / (pi / 4.0) + 0.5) as u32 % 4;
    match sektor {
        0 => '-',
        1 => '\\',
        2 => '|',
        _ => '/',
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::geometry::{self, Fit};

    fn layout(gw: u16, gh: u16) -> Layout {
        geometry::compute(gw, gh, 1.0, 2.0, Fit::Stretch, (4, 8), None)
    }

    /// Frame aus einer Funktion (x, y) -> Helligkeit, in Zellkoordinaten.
    fn frame_von(l: &Layout, f: impl Fn(u32, u32) -> u8) -> Frame {
        let mut data = vec![0u8; (l.px_w * l.px_h * 3) as usize];
        for y in 0..l.px_h {
            for x in 0..l.px_w {
                let v = f(x / l.ss_x, y / l.ss_y);
                let i = ((y * l.px_w + x) * 3) as usize;
                data[i] = v;
                data[i + 1] = v;
                data[i + 2] = v;
            }
        }
        Frame {
            w: l.px_w,
            h: l.px_h,
            data,
            index: 0,
        }
    }

    fn render(l: &Layout, f: &Frame, schwelle: f32) -> Grid {
        let mut g = Grid::new(l.grid_w, l.grid_h);
        let o = RenderOpts {
            edge_threshold: schwelle,
            ..Default::default()
        };
        EdgeRenderer::new().render(f, l, &o, &mut g);
        g
    }

    #[test]
    fn senkrechte_kante_ergibt_senkrechte_striche() {
        let l = layout(20, 12);
        // Linke Hälfte schwarz, rechte weiß -> die Naht läuft senkrecht.
        let g = render(
            &l,
            &frame_von(&l, |x, _| if x < 10 { 0 } else { 255 }),
            0.02,
        );
        let an_der_naht: Vec<char> = (2..10).map(|y| g.row(y)[10].ch).collect();
        assert!(
            an_der_naht.iter().filter(|c| **c == '|').count() >= 6,
            "erwartet senkrechte Striche, war {an_der_naht:?}"
        );
    }

    #[test]
    fn waagerechte_kante_ergibt_waagerechte_striche() {
        let l = layout(20, 12);
        let g = render(&l, &frame_von(&l, |_, y| if y < 6 { 0 } else { 255 }), 0.02);
        let an_der_naht: Vec<char> = (4..16).map(|x| g.row(6)[x].ch).collect();
        assert!(
            an_der_naht.iter().filter(|c| **c == '-').count() >= 8,
            "erwartet waagerechte Striche, war {an_der_naht:?}"
        );
    }

    #[test]
    fn richtungen_decken_alle_vier_faelle_ab() {
        // Gradient zeigt nach rechts -> Kante ist senkrecht.
        assert_eq!(richtung(1.0, 0.0), '|');
        // Gradient nach unten -> Kante waagerecht.
        assert_eq!(richtung(0.0, 1.0), '-');
        // Diagonalen.
        assert_eq!(richtung(1.0, 1.0), '/');
        assert_eq!(richtung(1.0, -1.0), '\\');
        // Entgegengesetzte Gradienten meinen dieselbe Kante.
        assert_eq!(richtung(-1.0, 0.0), richtung(1.0, 0.0));
        assert_eq!(richtung(-1.0, -1.0), richtung(1.0, 1.0));
    }

    #[test]
    fn flaechen_ohne_kante_nutzen_die_rampe() {
        let l = layout(12, 6);
        let g = render(&l, &frame_von(&l, |_, _| 128), 0.02);
        let kanten = ['|', '-', '/', '\\'];
        assert!(
            !g.cells.iter().any(|c| kanten.contains(&c.ch)),
            "eine einfarbige Fläche hat keine Kanten"
        );
    }

    #[test]
    fn hohe_schwelle_unterdrueckt_kanten() {
        let l = layout(20, 12);
        let f = frame_von(&l, |x, _| if x < 10 { 0 } else { 255 });
        let viele = render(&l, &f, 0.01);
        let keine = render(&l, &f, 5.0);
        let zaehl = |g: &Grid| g.cells.iter().filter(|c| c.ch == '|').count();
        assert!(zaehl(&viele) > 0);
        assert_eq!(zaehl(&keine), 0);
    }

    #[test]
    fn weichzeichner_erhaelt_den_mittelwert() {
        let mut v = vec![0.0f32; 64];
        v[27] = 1.0;
        let vorher: f32 = v.iter().sum();
        let mut tmp = vec![0.0; 64];
        blur(&mut v, &mut tmp, 8, 8, 1.0);
        let nachher: f32 = v.iter().sum();
        assert!(
            (vorher - nachher).abs() < 0.05,
            "Energie darf nicht verloren gehen: {vorher} -> {nachher}"
        );
    }

    #[test]
    fn veraltete_framegroesse_wird_verworfen() {
        let l = layout(8, 4);
        let alt = layout(16, 8);
        let mut g = Grid::new(l.grid_w, l.grid_h);
        EdgeRenderer::new().render(
            &frame_von(&alt, |_, _| 200),
            &l,
            &RenderOpts::default(),
            &mut g,
        );
        assert!(g.cells.iter().all(|c| c.ch == ' '));
    }
}
