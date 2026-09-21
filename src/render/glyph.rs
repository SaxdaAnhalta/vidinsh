//! Glyph-Matching: für jede Zelle das Zeichen suchen, dessen Form dem
//! Bildausschnitt am nächsten kommt.
//!
//! Die Masken werden aus der Schrift gerastert, die das Terminal tatsächlich
//! anzeigt -- unter Windows also `CascadiaMono.ttf`. Damit entspricht das, was
//! der Vergleich für richtig hält, dem, was am Ende auf dem Schirm steht.
//!
//! **Das Maß.** Naheliegend wäre, die Maske gegen den normalisierten
//! Helligkeitsausschnitt zu halten und die Fehlerquadrate zu summieren. Das
//! ist aber nicht die Frage, die hier zählt: Vorder- und Hintergrundfarbe
//! sind frei wählbar und werden ohnehin aus dem Ausschnitt gemittelt. Der
//! Fehler der fertigen Zelle ist deshalb genau die Streuung *innerhalb* der
//! beiden Gruppen, die die Maske aufteilt. Gesucht wird also die Maske mit
//! der kleinsten Summe der Gruppenvarianzen -- dieselbe Zielgröße wie bei
//! einer Zweiteilung nach k-Means, nur dass die erlaubten Aufteilungen durch
//! den Zeichensatz vorgegeben sind.

use super::{
    Cell, Charset, Grid, RenderOpts, Renderer, adjust_linear, color::Rgb, geometry::Layout,
    perceptual_luma, sub_bounds, to_rgb,
};
use crate::source::Frame;
use anyhow::{Context, Result, bail};
use rayon::prelude::*;
use std::path::{Path, PathBuf};

/// Rastergröße für die Maskenerzeugung. Feiner als nötig, damit das
/// anschließende Herunterrechnen sauber mittelt.
const RASTER_PX: f32 = 48.0;

/// Obergrenze für die Maskenauflösung. 16x16 wären 256 Felder -- darüber
/// bringt der Vergleich nichts mehr, kostet aber linear mehr.
const MAX_FELDER: usize = 256;

/// So viele Kandidaten werden mindestens geprüft, auch wenn das Dichtefenster
/// weniger hergibt.
const MIN_KANDIDATEN: usize = 8;

/// Halbe Breite des Dichtefensters. Enger wird schneller, aber beginnt
/// sichtbar danebenzugreifen.
const DICHTE_FENSTER: f32 = 0.22;

const ASCII_GLYPHEN: &str = concat!(
    " !\"#$%&'()*+,-./0123456789:;<=>?@",
    "ABCDEFGHIJKLMNOPQRSTUVWXYZ[\\]^_`",
    "abcdefghijklmnopqrstuvwxyz{|}~"
);

/// Zusätzliche Zeichen, die auf Unicode-fähigen Terminals Flächen besser
/// treffen als alles, was ASCII hergibt.
const EXTRA_GLYPHEN: &str = "░▒▓█▀▄▌▐";

struct Maske {
    ch: char,
    /// Anteil bedeckter Fläche, 0..1 -- Sortierschlüssel fürs Vorfiltern
    dichte: f32,
    /// Indizes der Felder, die als bedeckt gelten
    an: Vec<u16>,
}

pub struct GlyphRenderer {
    masken: Vec<Maske>,
    /// Maskenauflösung; bei geänderter Abtastung wird neu gerastert
    mw: u32,
    mh: u32,
    charset: Charset,
    font: Vec<u8>,
    pub quelle: String,
}

impl GlyphRenderer {
    pub fn new(explizit: Option<&Path>, charset: Charset) -> Result<Self> {
        let (font, quelle) = lade_schrift(explizit)?;
        Ok(GlyphRenderer {
            masken: Vec::new(),
            mw: 0,
            mh: 0,
            charset,
            font,
            quelle,
        })
    }

    fn sicherstellen(&mut self, mw: u32, mh: u32, charset: Charset) -> bool {
        if self.mw == mw && self.mh == mh && self.charset == charset && !self.masken.is_empty() {
            return true;
        }
        let zeichen = match charset {
            Charset::Ascii => ASCII_GLYPHEN.to_string(),
            _ => format!("{ASCII_GLYPHEN}{EXTRA_GLYPHEN}"),
        };
        match baue_masken(&self.font, &zeichen, mw, mh) {
            Ok(m) if !m.is_empty() => {
                self.masken = m;
                self.mw = mw;
                self.mh = mh;
                self.charset = charset;
                true
            }
            _ => false,
        }
    }
}

impl Renderer for GlyphRenderer {
    fn render(&mut self, frame: &Frame, layout: &Layout, opts: &RenderOpts, grid: &mut Grid) {
        if !frame.matches(layout) {
            return;
        }
        if grid.w != layout.grid_w || grid.h != layout.grid_h {
            grid.resize(layout.grid_w, layout.grid_h);
        }

        let mw = layout.ss_x.min(16);
        let mh = layout.ss_y.min(16);
        if !self.sicherstellen(mw, mh, opts.charset) {
            return;
        }

        let gw = layout.grid_w as usize;
        let (ssx, ssy) = (layout.ss_x, layout.ss_y);
        let felder = (mw * mh) as usize;
        let masken = &self.masken;

        grid.cells
            .par_chunks_mut(gw)
            .enumerate()
            .for_each(|(cy, row)| {
                let mut farbe = vec![(0.0f32, 0.0f32, 0.0f32); felder];
                let mut luma = vec![0.0f32; felder];

                for (cx, cell) in row.iter_mut().enumerate() {
                    super::subtile_means(frame, cx as u32, cy as u32, ssx, ssy, mw, mh, &mut farbe);
                    for (i, c) in farbe.iter_mut().enumerate() {
                        *c = adjust_linear(c.0, c.1, c.2, opts);
                        luma[i] = perceptual_luma(c.0, c.1, c.2);
                    }
                    *cell = beste_zelle(&luma, &farbe, masken);
                }
            });
    }
}

/// Sucht Zeichen, Vorder- und Hintergrundfarbe für eine Zelle.
fn beste_zelle(luma: &[f32], farbe: &[(f32, f32, f32)], masken: &[Maske]) -> Cell {
    let n = luma.len() as f32;
    let summe: f32 = luma.iter().sum();
    let quadrate: f32 = luma.iter().map(|l| l * l).sum();
    let streuung = quadrate - summe * summe / n;

    // Kaum Struktur im Ausschnitt: eine einfarbige Zelle ist die beste
    // Annäherung -- und im Writer die billigste.
    if streuung < 1e-4 {
        let c = mittel(farbe, 0..farbe.len());
        return Cell {
            ch: ' ',
            fg: c,
            bg: Some(c),
        };
    }

    let mittelwert = summe / n;
    let anteil = luma.iter().filter(|l| **l >= mittelwert).count() as f32 / n;
    let bereich = fenster(masken, anteil);

    let mut bester = &masken[bereich.start];
    let mut bester_fehler = f32::INFINITY;

    for m in &masken[bereich] {
        let k = m.an.len();
        if k == 0 || k == luma.len() {
            continue; // volle und leere Masken bilden keine Kante ab
        }
        let mut s1 = 0.0f32;
        let mut q1 = 0.0f32;
        for &i in &m.an {
            let l = luma[i as usize];
            s1 += l;
            q1 += l * l;
        }
        let (s0, q0) = (summe - s1, quadrate - q1);
        // Summe der Streuungen innerhalb beider Gruppen.
        let fehler = (q1 - s1 * s1 / k as f32) + (q0 - s0 * s0 / (luma.len() - k) as f32);
        if fehler < bester_fehler {
            bester_fehler = fehler;
            bester = m;
        }
    }

    let (mut ar, mut ag, mut ab) = (0.0, 0.0, 0.0);
    let mut an_maske = vec![false; luma.len()];
    for &i in &bester.an {
        an_maske[i as usize] = true;
        let c = farbe[i as usize];
        ar += c.0;
        ag += c.1;
        ab += c.2;
    }
    let k = bester.an.len() as f32;
    let (mut br, mut bg, mut bb) = (0.0, 0.0, 0.0);
    for (i, c) in farbe.iter().enumerate() {
        if !an_maske[i] {
            br += c.0;
            bg += c.1;
            bb += c.2;
        }
    }
    let j = (luma.len() - bester.an.len()) as f32;

    // `--invert` bleibt hier bewusst wirkungslos. Die Rampe braucht es, weil
    // sie nur die Vordergrundfarbe setzt und die Zeichendichte deshalb zum
    // Untergrund passen muss. Hier werden beide Farben aus dem Bild bestimmt
    // -- die Zelle sieht auf hellem wie auf dunklem Terminal gleich aus, und
    // ein Tausch ergäbe schlicht ein Negativ.
    Cell {
        ch: bester.ch,
        fg: to_rgb(ar / k, ag / k, ab / k),
        bg: Some(to_rgb(br / j, bg / j, bb / j)),
    }
}

/// Kandidatenfenster um eine Zieldichte. Die Masken sind nach Dichte
/// sortiert, deshalb genügt eine Bereichssuche.
fn fenster(masken: &[Maske], ziel: f32) -> std::ops::Range<usize> {
    let lo = masken.partition_point(|m| m.dichte < ziel - DICHTE_FENSTER);
    let hi = masken.partition_point(|m| m.dichte <= ziel + DICHTE_FENSTER);
    let (mut lo, mut hi) = (lo, hi.max(lo + 1).min(masken.len()));
    while hi - lo < MIN_KANDIDATEN.min(masken.len()) {
        lo = lo.saturating_sub(1);
        if hi < masken.len() {
            hi += 1;
        }
        if lo == 0 && hi == masken.len() {
            break;
        }
    }
    lo..hi
}

fn mittel(farbe: &[(f32, f32, f32)], r: std::ops::Range<usize>) -> Rgb {
    let n = r.len().max(1) as f32;
    let (mut a, mut b, mut c) = (0.0, 0.0, 0.0);
    for f in &farbe[r] {
        a += f.0;
        b += f.1;
        c += f.2;
    }
    to_rgb(a / n, b / n, c / n)
}

// ----------------------------------------------------------- Maskenerzeugung

fn schrift_kandidaten() -> Vec<PathBuf> {
    let mut v = Vec::new();
    #[cfg(windows)]
    {
        let dir =
            PathBuf::from(std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into()))
                .join("Fonts");
        for n in [
            "CascadiaMono.ttf",
            "CascadiaCode.ttf",
            "consola.ttf",
            "lucon.ttf",
        ] {
            v.push(dir.join(n));
        }
    }
    #[cfg(target_os = "macos")]
    {
        for n in [
            "/System/Library/Fonts/SFNSMono.ttf",
            "/System/Library/Fonts/Supplemental/Courier New.ttf",
        ] {
            v.push(PathBuf::from(n));
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        for n in [
            "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
            "/usr/share/fonts/truetype/liberation/LiberationMono-Regular.ttf",
            "/usr/share/fonts/TTF/DejaVuSansMono.ttf",
        ] {
            v.push(PathBuf::from(n));
        }
    }
    v
}

fn lade_schrift(explizit: Option<&Path>) -> Result<(Vec<u8>, String)> {
    if let Some(p) = explizit {
        let d = std::fs::read(p)
            .with_context(|| format!("Schrift {} lässt sich nicht lesen", p.display()))?;
        pruefe(&d).with_context(|| format!("{} ist keine brauchbare TTF", p.display()))?;
        return Ok((d, p.display().to_string()));
    }
    for p in schrift_kandidaten() {
        if let Ok(d) = std::fs::read(&p)
            && pruefe(&d).is_ok()
        {
            let name = p
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            return Ok((d, name));
        }
    }
    bail!(
        "Für den Modus 'glyph' wird eine TrueType-Schrift gebraucht, es wurde \
         aber keine gefunden. Mit --font <pfad.ttf> lässt sich eine angeben; \
         die übrigen Modi kommen ohne aus."
    )
}

fn pruefe(daten: &[u8]) -> Result<()> {
    fontdue::Font::from_bytes(daten, fontdue::FontSettings::default())
        .map(|_| ())
        .map_err(|e| anyhow::anyhow!("{e}"))
}

fn baue_masken(font_daten: &[u8], zeichen: &str, mw: u32, mh: u32) -> Result<Vec<Maske>> {
    if (mw * mh) as usize > MAX_FELDER {
        bail!("Maskenauflösung {mw}x{mh} ist zu fein");
    }
    let font = fontdue::Font::from_bytes(font_daten, fontdue::FontSettings::default())
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    let lm = font
        .horizontal_line_metrics(RASTER_PX)
        .context("Schrift hat keine waagerechten Zeilenmaße")?;
    let zell_h = (lm.ascent - lm.descent).ceil().max(1.0) as usize;
    let grundlinie = lm.ascent.round() as isize;
    // Monospace: die Vorschubbreite ist für alle Zeichen gleich.
    let zell_w = font.metrics('M', RASTER_PX).advance_width.ceil().max(1.0) as usize;

    let mut out = Vec::with_capacity(zeichen.chars().count());
    let mut deckung = vec![0f32; zell_w * zell_h];

    for ch in zeichen.chars() {
        deckung.fill(0.0);
        let (m, bitmap) = font.rasterize(ch, RASTER_PX);

        // Die Bitmap sitzt relativ zur Grundlinie; ymin ist ihr unterer Rand.
        let x0 = m.xmin as isize;
        let y0 = grundlinie - m.ymin as isize - m.height as isize;
        for gy in 0..m.height {
            for gx in 0..m.width {
                let (px, py) = (x0 + gx as isize, y0 + gy as isize);
                if px >= 0 && (px as usize) < zell_w && py >= 0 && (py as usize) < zell_h {
                    deckung[py as usize * zell_w + px as usize] =
                        bitmap[gy * m.width + gx] as f32 / 255.0;
                }
            }
        }

        // Auf die Maskenauflösung herunterrechnen.
        let mut an = Vec::new();
        let mut summe = 0.0f32;
        for my in 0..mh {
            let (y0, y1) = sub_bounds(zell_h as u32, mh, my);
            for mx in 0..mw {
                let (x0, x1) = sub_bounds(zell_w as u32, mw, mx);
                let mut acc = 0.0f32;
                for y in y0..y1 {
                    for x in x0..x1 {
                        acc += deckung[y as usize * zell_w + x as usize];
                    }
                }
                let wert = acc / ((y1 - y0) * (x1 - x0)) as f32;
                summe += wert;
                if wert > 0.5 {
                    an.push((my * mw + mx) as u16);
                }
            }
        }

        out.push(Maske {
            ch,
            dichte: summe / (mw * mh) as f32,
            an,
        });
    }

    out.sort_by(|a, b| {
        a.dichte
            .partial_cmp(&b.dichte)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::geometry::{self, Fit};

    fn layout(gw: u16, gh: u16) -> Layout {
        geometry::compute(gw, gh, 1.0, 2.0, Fit::Stretch, (4, 8), None)
    }

    fn renderer() -> Option<GlyphRenderer> {
        GlyphRenderer::new(None, Charset::Ascii).ok()
    }

    fn frame_von(l: &Layout, f: impl Fn(u32, u32) -> u8) -> Frame {
        let mut data = vec![0u8; (l.px_w * l.px_h * 3) as usize];
        for y in 0..l.px_h {
            for x in 0..l.px_w {
                let v = f(x, y);
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

    #[test]
    fn masken_sind_nach_dichte_sortiert_und_plausibel() {
        let Some(mut r) = renderer() else {
            eprintln!("keine Schrift gefunden -- Test übersprungen");
            return;
        };
        assert!(r.sicherstellen(4, 8, Charset::Ascii));
        assert!(r.masken.len() > 80, "zu wenige Kandidaten");
        assert!(
            r.masken.windows(2).all(|w| w[0].dichte <= w[1].dichte),
            "Dichtefenster setzt Sortierung voraus"
        );

        let d = |c: char| r.masken.iter().find(|m| m.ch == c).map(|m| m.dichte);
        assert_eq!(d(' '), Some(0.0), "Leerzeichen deckt nichts");
        assert!(d('@').unwrap() > d('.').unwrap(), "@ ist dichter als .");
        assert!(d('M').unwrap() > d('-').unwrap());
    }

    #[test]
    fn einfarbige_flaeche_ergibt_eine_einfarbige_zelle() {
        let Some(mut r) = renderer() else { return };
        let l = layout(4, 2);
        let mut g = Grid::new(l.grid_w, l.grid_h);
        r.render(
            &frame_von(&l, |_, _| 128),
            &l,
            &RenderOpts::default(),
            &mut g,
        );
        assert_eq!(g.cells[0].ch, ' ');
        assert_eq!(g.cells[0].bg, Some(g.cells[0].fg));
    }

    /// Zelle mit heller oberer und dunkler unterer Hälfte.
    fn geteilt(l: &Layout) -> Frame {
        frame_von(l, |_, y| if y % l.ss_y < l.ss_y / 2 { 250 } else { 10 })
    }

    #[test]
    fn beide_haelften_landen_in_den_beiden_farben() {
        let Some(mut r) = renderer() else { return };
        let l = layout(4, 2);
        let mut g = Grid::new(l.grid_w, l.grid_h);
        r.render(&geteilt(&l), &l, &RenderOpts::default(), &mut g);
        let c = g.cells[0];
        let bg = c.bg.expect("glyph setzt immer beide Farben");

        // Welche der beiden die Tinte trägt, ist gleichwertig: eine Maske und
        // ihr Komplement beschreiben dieselbe Zelle. Beide Farben müssen aber
        // die tatsächlichen Hälften treffen.
        let mut paar = [c.fg.0, bg.0];
        paar.sort_unstable();
        assert!(paar[0] < 40, "die dunkle Hälfte fehlt: {paar:?}");
        assert!(paar[1] > 200, "die helle Hälfte fehlt: {paar:?}");
        assert_ne!(c.ch, ' ', "eine Kante braucht ein Zeichen");
    }

    #[test]
    fn invert_erzeugt_kein_negativ() {
        // glyph setzt beide Farben aus dem Bild -- anders als die Rampe hängt
        // hier nichts am Untergrund des Terminals.
        let Some(mut r) = renderer() else { return };
        let l = layout(4, 2);
        let f = geteilt(&l);
        let lauf = |o: &RenderOpts| {
            let mut g = Grid::new(l.grid_w, l.grid_h);
            GlyphRenderer::new(None, Charset::Ascii)
                .unwrap()
                .render(&f, &l, o, &mut g);
            g.cells[0]
        };
        let _ = &mut r;
        assert_eq!(
            lauf(&RenderOpts::default()),
            lauf(&RenderOpts {
                invert: true,
                ..Default::default()
            })
        );
    }

    #[test]
    fn ascii_modus_bleibt_bei_ascii() {
        let Some(mut r) = renderer() else { return };
        let l = layout(8, 4);
        let f = frame_von(&l, |x, y| ((x * 7 + y * 13) % 256) as u8);
        let mut g = Grid::new(l.grid_w, l.grid_h);
        r.render(
            &f,
            &l,
            &RenderOpts {
                charset: Charset::Ascii,
                ..Default::default()
            },
            &mut g,
        );
        assert!(g.cells.iter().all(|c| c.ch.is_ascii()));
    }

    #[test]
    fn dichtefenster_liefert_immer_kandidaten() {
        let Some(mut r) = renderer() else { return };
        r.sicherstellen(4, 8, Charset::Ascii);
        for ziel in [0.0, 0.05, 0.3, 0.5, 0.9, 1.0] {
            let f = fenster(&r.masken, ziel);
            assert!(!f.is_empty(), "Zieldichte {ziel} ohne Kandidaten");
            assert!(f.end <= r.masken.len());
        }
    }

    #[test]
    fn fehlende_schrift_meldet_sich_verstaendlich() {
        let e = match GlyphRenderer::new(Some(Path::new("gibtsnicht.ttf")), Charset::Ascii) {
            Err(e) => format!("{e:#}"),
            Ok(_) => panic!("eine fehlende Schrift darf nicht durchgehen"),
        };
        assert!(e.contains("gibtsnicht.ttf"), "{e}");
    }

    #[test]
    fn veraltete_framegroesse_wird_verworfen() {
        let Some(mut r) = renderer() else { return };
        let l = layout(8, 4);
        let alt = layout(16, 8);
        let mut g = Grid::new(l.grid_w, l.grid_h);
        r.render(
            &frame_von(&alt, |_, _| 200),
            &l,
            &RenderOpts::default(),
            &mut g,
        );
        assert!(g.cells.iter().all(|c| c.ch == ' '));
    }
}
