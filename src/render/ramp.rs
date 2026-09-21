//! Der klassische Modus: Helligkeit pro Zelle -> Zeichen aus einer Rampe.

use super::{
    Grid, RenderOpts, Renderer, adjust_linear, geometry::Layout, perceptual_luma, ramp_char,
    tile_mean_linear, to_rgb,
};
use crate::source::Frame;
use rayon::prelude::*;

/// Anteil der Zellen, der bei `--auto-contrast` an jedem Ende abgeschnitten
/// wird. Ohne diesen Puffer reicht ein einzelnes Glanzlicht, um die
/// Normalisierung für das ganze Bild unbrauchbar zu machen.
const CLIP: f32 = 0.01;

/// Wie stark der Hintergrund gegenüber dem Zeichen abgedunkelt wird, wenn
/// `--bg` aktiv ist. Ohne Abstand verschwindet die Zeichenform im Untergrund.
const BG_FACTOR: f32 = 0.35;

pub struct RampRenderer {
    /// Helligkeit pro Zelle, zwischen den beiden Durchgängen
    luma: Vec<f32>,
}

impl RampRenderer {
    pub fn new() -> Self {
        RampRenderer { luma: Vec::new() }
    }
}

impl Default for RampRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl Renderer for RampRenderer {
    fn render(&mut self, frame: &Frame, layout: &Layout, opts: &RenderOpts, grid: &mut Grid) {
        if !frame.matches(layout) {
            return;
        }
        if grid.w != layout.grid_w || grid.h != layout.grid_h {
            grid.resize(layout.grid_w, layout.grid_h);
        }

        let gw = layout.grid_w as usize;
        let (ssx, ssy) = (layout.ss_x, layout.ss_y);
        self.luma.clear();
        self.luma.resize(layout.cells(), 0.0);

        // Erster Durchgang: Farbe und Helligkeit je Zelle.
        grid.cells
            .par_chunks_mut(gw)
            .zip(self.luma.par_chunks_mut(gw))
            .enumerate()
            .for_each(|(cy, (crow, lrow))| {
                let y0 = cy as u32 * ssy;
                for cx in 0..gw {
                    let (r, g, b) = tile_mean_linear(frame, cx as u32 * ssx, y0, ssx, ssy);
                    let (r, g, b) = adjust_linear(r, g, b, opts);
                    crow[cx].fg = to_rgb(r, g, b);
                    crow[cx].bg = if opts.bg {
                        Some(to_rgb(r * BG_FACTOR, g * BG_FACTOR, b * BG_FACTOR))
                    } else {
                        None
                    };
                    lrow[cx] = perceptual_luma(r, g, b);
                }
            });

        let (lo, span) = if opts.auto_contrast {
            percentile_span(&self.luma)
        } else {
            (0.0, 1.0)
        };

        // Zweiter Durchgang: Helligkeit -> Zeichen. Getrennt, weil
        // `--auto-contrast` erst die Verteilung des ganzen Bildes braucht.
        grid.cells
            .par_chunks_mut(gw)
            .zip(self.luma.par_chunks(gw))
            .for_each(|(crow, lrow)| {
                for (cell, &l) in crow.iter_mut().zip(lrow) {
                    cell.ch = ramp_char((l - lo) / span, opts);
                }
            });
    }
}

/// Untergrenze und Spannweite zwischen dem 1.- und 99.-Perzentil.
/// Histogramm statt Sortieren: 256 Eimer reichen für eine Zeichenrampe
/// von etwa zehn Stufen bei weitem.
fn percentile_span(luma: &[f32]) -> (f32, f32) {
    if luma.is_empty() {
        return (0.0, 1.0);
    }
    let mut hist = [0u32; 256];
    for &l in luma {
        hist[(l.clamp(0.0, 1.0) * 255.0) as usize] += 1;
    }
    let cut = (luma.len() as f32 * CLIP) as u32;

    let mut acc = 0u32;
    let mut lo = 0usize;
    for (i, &n) in hist.iter().enumerate() {
        acc += n;
        if acc > cut {
            lo = i;
            break;
        }
    }
    let mut acc = 0u32;
    let mut hi = 255usize;
    for (i, &n) in hist.iter().enumerate().rev() {
        acc += n;
        if acc > cut {
            hi = i;
            break;
        }
    }

    if hi <= lo {
        return (0.0, 1.0);
    }
    let lo = lo as f32 / 255.0;
    let hi = hi as f32 / 255.0;
    // Kleine Spannweiten nicht ins Extreme ziehen -- sonst rauscht ein
    // fast einfarbiges Bild wild auf.
    (lo, (hi - lo).max(0.05))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::geometry::{self, Fit};

    fn layout(gw: u16, gh: u16, ss: (u32, u32)) -> Layout {
        geometry::compute(gw, gh, 1.0, 2.0, Fit::Stretch, ss, None)
    }

    /// Frame mit einem waagerechten Helligkeitsverlauf von links nach rechts.
    fn verlauf(l: &Layout) -> Frame {
        let mut data = vec![0u8; (l.px_w * l.px_h * 3) as usize];
        for y in 0..l.px_h {
            for x in 0..l.px_w {
                let v = (x * 255 / (l.px_w - 1).max(1)) as u8;
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

    fn einfarbig(l: &Layout, c: (u8, u8, u8)) -> Frame {
        let mut data = vec![0u8; (l.px_w * l.px_h * 3) as usize];
        for px in data.chunks_exact_mut(3) {
            px[0] = c.0;
            px[1] = c.1;
            px[2] = c.2;
        }
        Frame {
            w: l.px_w,
            h: l.px_h,
            data,
            index: 0,
        }
    }

    #[test]
    fn verlauf_ergibt_aufsteigende_zeichendichte() {
        let l = layout(10, 2, (4, 8));
        let mut g = Grid::new(l.grid_w, l.grid_h);
        RampRenderer::new().render(&verlauf(&l), &l, &RenderOpts::default(), &mut g);

        let ramp = RenderOpts::default().ramp;
        let idx: Vec<usize> = g
            .row(0)
            .iter()
            .map(|c| ramp.iter().position(|&r| r == c.ch).unwrap())
            .collect();
        assert_eq!(idx[0], 0, "links muss das leichteste Zeichen stehen");
        assert_eq!(idx[9], ramp.len() - 1, "rechts das dichteste");
        assert!(
            idx.windows(2).all(|w| w[0] <= w[1]),
            "Dichte muss monoton steigen: {idx:?}"
        );
    }

    #[test]
    fn farbe_wird_aus_dem_tile_gemittelt() {
        let l = layout(4, 2, (4, 8));
        let mut g = Grid::new(l.grid_w, l.grid_h);
        RampRenderer::new().render(
            &einfarbig(&l, (200, 100, 50)),
            &l,
            &RenderOpts::default(),
            &mut g,
        );
        for c in &g.cells {
            assert_eq!(c.fg, super::super::color::Rgb(200, 100, 50));
            assert_eq!(c.bg, None, "ohne --bg darf kein Hintergrund gesetzt sein");
        }
    }

    #[test]
    fn bg_ist_dunkler_als_der_vordergrund() {
        let l = layout(4, 2, (4, 8));
        let mut g = Grid::new(l.grid_w, l.grid_h);
        let o = RenderOpts {
            bg: true,
            ..Default::default()
        };
        RampRenderer::new().render(&einfarbig(&l, (200, 100, 50)), &l, &o, &mut g);
        let c = g.cells[0];
        let bg = c.bg.expect("--bg muss einen Hintergrund setzen");
        assert!(bg.0 < c.fg.0 && bg.1 < c.fg.1 && bg.2 < c.fg.2);
    }

    #[test]
    fn veraltete_framegroesse_wird_verworfen_statt_zu_paniken() {
        let l = layout(8, 4, (4, 8));
        let mut g = Grid::new(l.grid_w, l.grid_h);
        let alt = layout(16, 8, (4, 8));
        // Frame der alten Größe darf das Raster nicht anfassen.
        RampRenderer::new().render(&verlauf(&alt), &l, &RenderOpts::default(), &mut g);
        assert!(g.cells.iter().all(|c| c.ch == ' '));
    }

    #[test]
    fn auto_contrast_spreizt_einen_flauen_verlauf() {
        let l = layout(10, 2, (4, 8));
        // Verlauf nur im unteren Viertel des Wertebereichs.
        let mut f = verlauf(&l);
        for b in f.data.iter_mut() {
            *b /= 4;
        }

        let dichte = |o: &RenderOpts| {
            let mut g = Grid::new(l.grid_w, l.grid_h);
            RampRenderer::new().render(&f, &l, o, &mut g);
            let ramp = &o.ramp;
            ramp.iter().position(|&r| r == g.row(0)[9].ch).unwrap()
        };

        let ohne = dichte(&RenderOpts::default());
        let o = RenderOpts {
            auto_contrast: true,
            ..Default::default()
        };
        let mit = dichte(&o);
        assert!(
            mit > ohne,
            "auto-contrast muss das hellste Zeichen anheben ({ohne} -> {mit})"
        );
    }

    #[test]
    fn einfarbiges_bild_ueberdreht_auto_contrast_nicht() {
        let l = layout(6, 3, (4, 8));
        let o = RenderOpts {
            auto_contrast: true,
            ..Default::default()
        };
        let mut g = Grid::new(l.grid_w, l.grid_h);
        RampRenderer::new().render(&einfarbig(&l, (90, 90, 90)), &l, &o, &mut g);
        // Kein Panik-, kein NaN-Fall; alle Zellen gleich.
        let erste = g.cells[0].ch;
        assert!(g.cells.iter().all(|c| c.ch == erste));
    }
}
