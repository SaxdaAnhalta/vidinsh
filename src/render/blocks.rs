//! Blockzeichen: mehr Auflösung, indem eine Zelle zwei Farben trägt.
//!
//! Eine Terminalzelle kann Vorder- und Hintergrundfarbe getrennt halten. Ein
//! Zeichen wie `▀` teilt sie waagerecht -- damit verdoppelt sich die
//! senkrechte Auflösung, ohne dass das Terminal mehr Zellen bekommt.
//!
//! Bei Quadranten und Sextanten reichen zwei Farben nicht mehr für alle
//! Unterfelder. Dann wird nach Helligkeit in zwei Gruppen geteilt und für
//! jede der Mittelwert genommen -- das ist Block Truncation Coding, und für
//! Bewegtbild ist der Fehler kaum zu sehen.

use super::{
    Cell, Charset, Grid, RenderOpts, Renderer, adjust_linear, geometry::Layout, perceptual_luma,
    ramp_char, subtile_means, tile_mean_linear, to_rgb,
};
use crate::source::Frame;
use rayon::prelude::*;

/// Bitmaske -> Quadrantenzeichen. Bit 0 ist oben links, dann im Uhrzeigersinn
/// zeilenweise: oben rechts, unten links, unten rechts.
const QUAD: [char; 16] = [
    ' ', '▘', '▝', '▀', '▖', '▌', '▞', '▛', '▗', '▚', '▐', '▜', '▄', '▙', '▟', '█',
];

/// Höchste Zahl von Unterfeldern, die ein Zeichensatz hier braucht (Braille).
const MAX_SUB: usize = 8;

pub struct BlocksRenderer;

impl BlocksRenderer {
    pub fn new() -> Self {
        BlocksRenderer
    }
}

impl Default for BlocksRenderer {
    fn default() -> Self {
        Self::new()
    }
}

/// Unterteilung, die ein Zeichensatz braucht.
fn grid_of(cs: Charset) -> (u32, u32) {
    match cs {
        Charset::Half => (1, 2),
        Charset::Quad => (2, 2),
        Charset::Sextant => (2, 3),
        Charset::Braille => (2, 4),
        // Ohne Blockzeichen bleibt nur die Rampe; eine Unterteilung brächte
        // dort nichts, weil ASCII keine Teilflächen kennt.
        Charset::Ascii | Charset::Extended => (1, 1),
    }
}

impl Renderer for BlocksRenderer {
    fn render(&mut self, frame: &Frame, layout: &Layout, opts: &RenderOpts, grid: &mut Grid) {
        if !frame.matches(layout) {
            return;
        }
        if grid.w != layout.grid_w || grid.h != layout.grid_h {
            grid.resize(layout.grid_w, layout.grid_h);
        }

        let gw = layout.grid_w as usize;
        let (ssx, ssy) = (layout.ss_x, layout.ss_y);
        let (nx, ny) = grid_of(opts.charset);

        grid.cells
            .par_chunks_mut(gw)
            .enumerate()
            .for_each(|(cy, row)| {
                let mut sub = [(0.0f32, 0.0f32, 0.0f32); MAX_SUB];
                for (cx, cell) in row.iter_mut().enumerate() {
                    let n = (nx * ny) as usize;
                    subtile_means(frame, cx as u32, cy as u32, ssx, ssy, nx, ny, &mut sub[..n]);
                    for s in &mut sub[..n] {
                        *s = adjust_linear(s.0, s.1, s.2, opts);
                    }
                    *cell = match opts.charset {
                        Charset::Half => halb(&sub[..2]),
                        Charset::Quad => bitmuster(&sub[..4], |m| QUAD[m as usize]),
                        Charset::Sextant => bitmuster(&sub[..6], sextant_char),
                        Charset::Braille => braille(&sub[..8], opts),
                        Charset::Ascii | Charset::Extended => {
                            let (r, g, b) =
                                tile_mean_linear(frame, cx as u32 * ssx, cy as u32 * ssy, ssx, ssy);
                            let (r, g, b) = adjust_linear(r, g, b, opts);
                            Cell {
                                ch: ramp_char(perceptual_luma(r, g, b), opts),
                                fg: to_rgb(r, g, b),
                                bg: None,
                            }
                        }
                    };
                }
            });
    }
}

/// Halbblock: die beiden Hälften bekommen je eine eigene Farbe. Sind sie
/// gleich, genügt ein Leerzeichen mit Hintergrund -- das spart im Writer den
/// kompletten Vordergrund-Farbcode.
fn halb(s: &[(f32, f32, f32)]) -> Cell {
    let oben = to_rgb(s[0].0, s[0].1, s[0].2);
    let unten = to_rgb(s[1].0, s[1].1, s[1].2);
    if oben == unten {
        Cell {
            ch: ' ',
            fg: oben,
            bg: Some(unten),
        }
    } else {
        Cell {
            ch: '▀',
            fg: oben,
            bg: Some(unten),
        }
    }
}

/// Teilt die Unterfelder an ihrer mittleren Helligkeit in zwei Gruppen und
/// wählt das Zeichen, dessen Maske dazu passt.
fn bitmuster(s: &[(f32, f32, f32)], zeichen: impl Fn(u32) -> char) -> Cell {
    let lum: [f32; MAX_SUB] = std::array::from_fn(|i| {
        s.get(i)
            .map(|c| super::color::luma_linear(c.0, c.1, c.2))
            .unwrap_or(0.0)
    });
    let n = s.len();
    let mittel = lum[..n].iter().sum::<f32>() / n as f32;

    let mut maske = 0u32;
    let (mut ar, mut ag, mut ab, mut an) = (0.0, 0.0, 0.0, 0u32);
    let (mut br, mut bg, mut bb, mut bn) = (0.0, 0.0, 0.0, 0u32);
    for (i, c) in s.iter().enumerate() {
        if lum[i] >= mittel {
            maske |= 1 << i;
            ar += c.0;
            ag += c.1;
            ab += c.2;
            an += 1;
        } else {
            br += c.0;
            bg += c.1;
            bb += c.2;
            bn += 1;
        }
    }

    // Alle Felder in einer Gruppe: einfarbige Zelle, kein Zeichen nötig.
    if bn == 0 {
        let k = an as f32;
        let c = to_rgb(ar / k, ag / k, ab / k);
        return Cell {
            ch: ' ',
            fg: c,
            bg: Some(c),
        };
    }

    let (ak, bk) = (an as f32, bn as f32);
    Cell {
        ch: zeichen(maske),
        fg: to_rgb(ar / ak, ag / ak, ab / ak),
        bg: Some(to_rgb(br / bk, bg / bk, bb / bk)),
    }
}

/// Sextanten liegen ab U+1FB00, aber die vier Muster, für die es schon
/// Zeichen gibt (leer, linke Hälfte, rechte Hälfte, voll), sind ausgespart.
fn sextant_char(m: u32) -> char {
    match m {
        0 => ' ',
        0b010101 => '▌',
        0b101010 => '▐',
        0b111111 => '█',
        n => {
            let mut off = n - 1;
            if n > 0b010101 {
                off -= 1;
            }
            if n > 0b101010 {
                off -= 1;
            }
            char::from_u32(0x1FB00 + off).unwrap_or('?')
        }
    }
}

/// Braille trägt 2x4 Punkte, aber nur eine Farbe. Deshalb wird über der
/// mittleren Helligkeit geschwellt und die Farbe aus den gesetzten Punkten
/// genommen -- das ergibt feine Strukturen, aber flachere Farben.
fn braille(s: &[(f32, f32, f32)], opts: &RenderOpts) -> Cell {
    // Punktnummern 1..8 in der Reihenfolge, die Unicode verlangt.
    const BIT: [u32; 8] = [0, 1, 2, 6, 3, 4, 5, 7];

    let lum: [f32; 8] = std::array::from_fn(|i| super::color::luma_linear(s[i].0, s[i].1, s[i].2));
    let mittel = lum.iter().sum::<f32>() / 8.0;

    let mut muster = 0u32;
    let (mut r, mut g, mut b, mut n) = (0.0, 0.0, 0.0, 0u32);
    for i in 0..8 {
        // Unterfelder liegen zeilenweise (x schnell), die Bits spaltenweise.
        let (sx, sy) = (i % 2, i / 2);
        let hell = if opts.invert {
            lum[i] < mittel
        } else {
            lum[i] >= mittel
        };
        if hell {
            muster |= 1 << BIT[sy * 2 + sx];
            r += s[i].0;
            g += s[i].1;
            b += s[i].2;
            n += 1;
        }
    }

    let fg = if n > 0 {
        let k = n as f32;
        to_rgb(r / k, g / k, b / k)
    } else {
        let k = 8.0;
        let (r, g, b) = s
            .iter()
            .fold((0.0, 0.0, 0.0), |a, c| (a.0 + c.0, a.1 + c.1, a.2 + c.2));
        to_rgb(r / k, g / k, b / k)
    };

    Cell {
        ch: char::from_u32(0x2800 + muster).unwrap_or(' '),
        fg,
        bg: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::color::Rgb;
    use crate::render::geometry::{self, Fit};
    use crate::render::sub_bounds;

    fn layout(gw: u16, gh: u16) -> Layout {
        geometry::compute(gw, gh, 1.0, 2.0, Fit::Stretch, (4, 8), None)
    }

    /// Frame, dessen obere Hälfte weiß und untere Hälfte schwarz ist.
    fn oben_weiss(l: &Layout) -> Frame {
        let mut data = vec![0u8; (l.px_w * l.px_h * 3) as usize];
        for y in 0..l.px_h {
            // Obere Hälfte *jeder Zelle*, nicht des Bildes.
            let in_oberer = (y % l.ss_y) < l.ss_y / 2;
            let v = if in_oberer { 255 } else { 0 };
            for x in 0..l.px_w {
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

    fn einfarbig(l: &Layout, v: u8) -> Frame {
        Frame {
            w: l.px_w,
            h: l.px_h,
            data: vec![v; (l.px_w * l.px_h * 3) as usize],
            index: 0,
        }
    }

    fn opts(cs: Charset) -> RenderOpts {
        RenderOpts {
            charset: cs,
            ..Default::default()
        }
    }

    #[test]
    fn halbblock_traegt_zwei_farben() {
        let l = layout(4, 2);
        let mut g = Grid::new(l.grid_w, l.grid_h);
        BlocksRenderer::new().render(&oben_weiss(&l), &l, &opts(Charset::Half), &mut g);
        let c = g.cells[0];
        assert_eq!(c.ch, '▀');
        assert_eq!(
            c.fg,
            Rgb(255, 255, 255),
            "oben muss die Vordergrundfarbe sein"
        );
        assert_eq!(c.bg, Some(Rgb(0, 0, 0)));
    }

    #[test]
    fn einfarbige_zelle_spart_das_zeichen() {
        let l = layout(4, 2);
        let mut g = Grid::new(l.grid_w, l.grid_h);
        BlocksRenderer::new().render(&einfarbig(&l, 120), &l, &opts(Charset::Half), &mut g);
        let c = g.cells[0];
        assert_eq!(c.ch, ' ', "gleiche Hälften brauchen kein Blockzeichen");
        assert_eq!(c.bg, Some(c.fg));
    }

    #[test]
    fn quadranten_tabelle_ist_vollstaendig_und_eindeutig() {
        assert_eq!(QUAD[0b0000], ' ');
        assert_eq!(QUAD[0b1111], '█');
        assert_eq!(QUAD[0b0011], '▀', "oben links + oben rechts = obere Hälfte");
        assert_eq!(QUAD[0b1100], '▄');
        assert_eq!(QUAD[0b0101], '▌', "links oben + links unten = linke Hälfte");
        assert_eq!(QUAD[0b1010], '▐');
        let mut s: Vec<char> = QUAD.to_vec();
        s.sort_unstable();
        s.dedup();
        assert_eq!(s.len(), 16, "jede Maske braucht ein eigenes Zeichen");
    }

    #[test]
    fn quadrant_erkennt_die_obere_haelfte() {
        let l = layout(4, 2);
        let mut g = Grid::new(l.grid_w, l.grid_h);
        BlocksRenderer::new().render(&oben_weiss(&l), &l, &opts(Charset::Quad), &mut g);
        assert_eq!(g.cells[0].ch, '▀');
    }

    #[test]
    fn sextanten_ueberspringen_die_belegten_muster() {
        assert_eq!(sextant_char(0), ' ');
        assert_eq!(sextant_char(0b111111), '█');
        assert_eq!(sextant_char(0b010101), '▌');
        assert_eq!(sextant_char(0b101010), '▐');
        // Direkt vor und nach der ersten Aussparung.
        assert_eq!(sextant_char(1), '\u{1FB00}');
        assert_eq!(sextant_char(0b010100), '\u{1FB13}');
        assert_eq!(sextant_char(0b010110), '\u{1FB14}');
    }

    #[test]
    fn jedes_sextant_muster_ergibt_ein_eigenes_zeichen() {
        let mut alle: Vec<char> = (0..64).map(sextant_char).collect();
        alle.sort_unstable();
        alle.dedup();
        assert_eq!(alle.len(), 64);
        assert!(!alle.contains(&'?'), "kein Muster darf ins Leere zeigen");
    }

    #[test]
    fn braille_liegt_im_richtigen_block() {
        let l = layout(4, 2);
        let mut g = Grid::new(l.grid_w, l.grid_h);
        BlocksRenderer::new().render(&oben_weiss(&l), &l, &opts(Charset::Braille), &mut g);
        let c = g.cells[0].ch as u32;
        assert!((0x2800..=0x28FF).contains(&c), "U+{c:04X} ist kein Braille");
        assert_eq!(g.cells[0].bg, None, "Braille kennt nur eine Farbe");
    }

    #[test]
    fn ohne_unicode_faellt_blocks_auf_die_rampe_zurueck() {
        let l = layout(4, 2);
        let mut g = Grid::new(l.grid_w, l.grid_h);
        BlocksRenderer::new().render(&einfarbig(&l, 255), &l, &opts(Charset::Ascii), &mut g);
        assert!(g.cells[0].ch.is_ascii(), "auf ASCII-Terminals nur ASCII");
        assert_eq!(g.cells[0].ch, '@');
    }

    #[test]
    fn unterfelder_teilen_sich_auch_krumm_auf() {
        // 8 Pixel auf 3 Reihen: 3/3/2, lückenlos und ohne Überlappung.
        let g: Vec<(u32, u32)> = (0..3).map(|i| sub_bounds(8, 3, i)).collect();
        assert_eq!(g, vec![(0, 2), (2, 5), (5, 8)]);
        assert!(g.windows(2).all(|w| w[0].1 == w[1].0));
    }

    #[test]
    fn grobe_abtastung_laesst_kein_feld_leer() {
        // 1 Pixel auf 4 Felder: jedes Feld muss trotzdem einen Pixel sehen.
        for i in 0..4 {
            let (lo, hi) = sub_bounds(1, 4, i);
            assert!(hi > lo, "Feld {i} wäre leer");
        }
    }

    #[test]
    fn veraltete_framegroesse_wird_verworfen() {
        let l = layout(8, 4);
        let alt = layout(16, 8);
        let mut g = Grid::new(l.grid_w, l.grid_h);
        BlocksRenderer::new().render(&einfarbig(&alt, 200), &l, &opts(Charset::Half), &mut g);
        assert!(g.cells.iter().all(|c| c.ch == ' ' && c.bg.is_none()));
    }
}
