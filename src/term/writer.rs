//! Die Ausgabe ins Terminal -- der eigentliche Engpass des Programms.
//!
//! Ein Raster von 200x60 in Truecolor sind naiv rund 240 KB pro Frame. Bei
//! 30 fps müssten 7 MB/s durch den Terminal-Parser. Drei Maßnahmen drücken das
//! typisch um eine Größenordnung:
//!
//! 1. **Diffing** -- nur geänderte Zellen ausgeben, der Rest wird per
//!    Cursorsprung übergangen.
//! 2. **SGR-Lauflängen** -- ein Farbcode nur, wenn er sich ändert. Ein
//!    Truecolor-Paar sind 39 Bytes, ein Zeichen eines. Das ist der größte
//!    Einzelhebel.
//! 3. **Ein `write_all` pro Frame**, geklammert in synchronisierte Ausgabe,
//!    damit das Terminal erst zeichnet, wenn alles da ist.
//!
//! `--bench-naive` schaltet 1 und 2 ab. Das ist keine Altlast, sondern der
//! Vergleichsmaßstab: nur so lässt sich belegen, dass die Optimierung wirkt.

use crate::render::color::{self, ColorMode, Rgb};
use crate::render::geometry::Layout;
use crate::render::{Cell, Grid};
use std::io::{self, Write};

/// Ab wie vielen zusammenhängend unveränderten Zellen sich ein Cursorsprung
/// lohnt. Ein Sprung kostet etwa 8 Bytes, eine übergangene Zelle je nach
/// Farbwechsel 1 bis 40. Unterhalb dieser Schwelle ist Mitschreiben billiger.
const SKIP_MIN: usize = 3;

const SYNC_ON: &[u8] = b"\x1b[?2026h";
const SYNC_OFF: &[u8] = b"\x1b[?2026l";

#[derive(Default, Clone, Copy)]
struct Sgr {
    fg: Option<Rgb>,
    /// `Some(None)` heißt: Standardhintergrund ist aktiv gesetzt.
    bg: Option<Option<Rgb>>,
}

#[derive(Default, Clone, Copy, Debug)]
pub struct Stats {
    pub bytes: usize,
    pub cells_written: usize,
    pub cells_total: usize,
}

pub struct Writer {
    sink: Box<dyn Write + Send>,
    buf: Vec<u8>,
    prev: Grid,
    sync: bool,
    naive: bool,
    force_full: bool,
    last_color: Option<ColorMode>,
    last_dither: bool,
    runs: Vec<(usize, usize)>,
    pub stats: Stats,
}

impl Writer {
    pub fn new(sink: Box<dyn Write + Send>, sync: bool, naive: bool) -> Self {
        Writer {
            sink,
            buf: Vec::with_capacity(1 << 20),
            prev: Grid::new(0, 0),
            sync,
            naive,
            force_full: true,
            last_color: None,
            last_dither: false,
            runs: Vec::new(),
            stats: Stats::default(),
        }
    }

    /// Nächster Frame wird vollständig neu gezeichnet. Nötig nach Resize,
    /// Moduswechsel und allem, was den Schirminhalt hinter unserem Rücken
    /// verändert haben könnte.
    pub fn force_redraw(&mut self) {
        self.force_full = true;
    }

    pub fn draw(
        &mut self,
        grid: &Grid,
        layout: &Layout,
        color: ColorMode,
        dither: bool,
        status: Option<&str>,
        term_h: u16,
    ) -> io::Result<()> {
        // Ein Farbwechsel ändert die Bytes jeder Zelle, auch wenn ihr Inhalt
        // gleich blieb -- das Diffing würde sonst alte Farben stehen lassen.
        if self.last_color != Some(color) || self.last_dither != dither {
            self.force_full = true;
            self.last_color = Some(color);
            self.last_dither = dither;
        }
        if self.prev.w != grid.w || self.prev.h != grid.h {
            self.prev.resize(grid.w, grid.h);
            self.force_full = true;
        }

        let full = self.force_full || self.naive;
        self.buf.clear();
        self.stats.cells_written = 0;
        self.stats.cells_total = grid.cells.len();

        if self.sync {
            self.buf.extend_from_slice(SYNC_ON);
        }
        if full {
            // Erst beim Vollbild löschen: sonst flackert jeder Frame.
            self.buf.extend_from_slice(b"\x1b[0m\x1b[2J");
        }

        let mut sgr = Sgr::default();
        let w = grid.w as usize;

        for y in 0..grid.h {
            let cur = grid.row(y);
            let prev = self.prev.row(y);
            collect_runs(cur, prev, full, &mut self.runs);

            for &(start, end) in &self.runs {
                push_move(&mut self.buf, layout.off_y + y, layout.off_x + start as u16);
                for (x, cell) in cur[start..end].iter().enumerate() {
                    let ort = dither.then_some((start + x, y as usize));
                    emit_cell(&mut self.buf, cell, color, ort, &mut sgr, self.naive);
                }
                self.stats.cells_written += end - start;
            }
            let _ = w;
        }

        if let Some(s) = status {
            self.buf.extend_from_slice(b"\x1b[0m");
            push_move(&mut self.buf, term_h.saturating_sub(1), 0);
            self.buf.extend_from_slice(b"\x1b[K");
            self.buf.extend_from_slice(s.as_bytes());
            sgr = Sgr::default();
        }
        let _ = sgr;

        self.buf.extend_from_slice(b"\x1b[0m");
        if self.sync {
            self.buf.extend_from_slice(SYNC_OFF);
        }

        self.stats.bytes = self.buf.len();
        self.sink.write_all(&self.buf)?;
        self.sink.flush()?;

        self.prev.cells.copy_from_slice(&grid.cells);
        self.force_full = false;
        Ok(())
    }
}

/// Zeilenweise die auszugebenden Abschnitte bestimmen. Lücken unter
/// `SKIP_MIN` werden mitgeschrieben, statt dafür den Cursor zu versetzen.
fn collect_runs(cur: &[Cell], prev: &[Cell], full: bool, out: &mut Vec<(usize, usize)>) {
    out.clear();
    if cur.is_empty() {
        return;
    }
    if full {
        out.push((0, cur.len()));
        return;
    }

    let n = cur.len();
    let mut i = 0;
    while i < n {
        if cur[i] == prev[i] {
            i += 1;
            continue;
        }
        let start = i;
        let mut end = i + 1;
        let mut j = i + 1;
        let mut gap = 0usize;
        while j < n {
            if cur[j] != prev[j] {
                end = j + 1;
                gap = 0;
            } else {
                gap += 1;
                if gap >= SKIP_MIN {
                    break;
                }
            }
            j += 1;
        }
        out.push((start, end));
        i = j.max(end);
    }
}

#[inline]
fn emit_cell(
    buf: &mut Vec<u8>,
    cell: &Cell,
    color: ColorMode,
    ort: Option<(usize, usize)>,
    sgr: &mut Sgr,
    naive: bool,
) {
    // Die Verschiebung hängt an der Zellposition, nicht am Inhalt -- deshalb
    // wird sie erst hier angewandt und nicht schon im Renderer. Eine
    // unveränderte Zelle ergibt so weiterhin dieselben Bytes.
    let (fg, cbg) = match ort {
        Some((x, y)) => (
            color::dither(cell.fg, color, x, y),
            cell.bg.map(|b| color::dither(b, color, x, y)),
        ),
        None => (cell.fg, cell.bg),
    };

    if naive {
        buf.extend_from_slice(b"\x1b[0m");
        color::push_fg(buf, fg, color);
        if let Some(bg) = cbg {
            color::push_bg(buf, bg, color);
        }
        push_char(buf, cell.ch);
        return;
    }

    if sgr.fg != Some(fg) {
        color::push_fg(buf, fg, color);
        sgr.fg = Some(fg);
    }
    match cbg {
        Some(bg) => {
            if sgr.bg != Some(Some(bg)) {
                color::push_bg(buf, bg, color);
                sgr.bg = Some(Some(bg));
            }
        }
        None => {
            if sgr.bg != Some(None) {
                if color != ColorMode::Mono {
                    buf.extend_from_slice(b"\x1b[49m");
                }
                sgr.bg = Some(None);
            }
        }
    }
    push_char(buf, cell.ch);
}

#[inline]
fn push_char(buf: &mut Vec<u8>, ch: char) {
    if ch.is_ascii() {
        buf.push(ch as u8);
    } else {
        let mut tmp = [0u8; 4];
        buf.extend_from_slice(ch.encode_utf8(&mut tmp).as_bytes());
    }
}

/// `ESC[{Zeile};{Spalte}H`, beide 1-basiert.
#[inline]
fn push_move(buf: &mut Vec<u8>, row0: u16, col0: u16) {
    buf.extend_from_slice(b"\x1b[");
    push_u16(buf, row0 + 1);
    buf.push(b';');
    push_u16(buf, col0 + 1);
    buf.push(b'H');
}

#[inline]
fn push_u16(buf: &mut Vec<u8>, n: u16) {
    if n >= 10000 {
        buf.push(b'0' + (n / 10000) as u8);
    }
    if n >= 1000 {
        buf.push(b'0' + ((n / 1000) % 10) as u8);
    }
    if n >= 100 {
        buf.push(b'0' + ((n / 100) % 10) as u8);
    }
    if n >= 10 {
        buf.push(b'0' + ((n / 10) % 10) as u8);
    }
    buf.push(b'0' + (n % 10) as u8);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::geometry::{self, Fit};

    fn layout(w: u16, h: u16) -> Layout {
        geometry::compute(w, h, 1.0, 2.0, Fit::Stretch, (4, 8), None)
    }

    /// Sammelt die Ausgabe, damit Tests ohne Terminal laufen.
    #[derive(Clone, Default)]
    struct Sammler(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl Write for Sammler {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn grid(w: u16, h: u16, ch: char) -> Grid {
        let mut g = Grid::new(w, h);
        for c in &mut g.cells {
            c.ch = ch;
            c.fg = Rgb(10, 20, 30);
        }
        g
    }

    #[test]
    fn zahlen_werden_korrekt_geschrieben() {
        for n in [0u16, 1, 9, 10, 99, 100, 999, 1000, 12345, 65535] {
            let mut b = Vec::new();
            push_u16(&mut b, n);
            assert_eq!(String::from_utf8(b).unwrap(), n.to_string());
        }
    }

    #[test]
    fn cursorsprung_ist_einsbasiert() {
        let mut b = Vec::new();
        push_move(&mut b, 0, 0);
        assert_eq!(b, b"\x1b[1;1H");
    }

    #[test]
    fn erster_frame_ist_immer_vollstaendig() {
        let s = Sammler::default();
        let mut w = Writer::new(Box::new(s.clone()), false, false);
        let g = grid(10, 3, 'x');
        w.draw(&g, &layout(10, 3), ColorMode::Truecolor, false, None, 4)
            .unwrap();
        assert_eq!(w.stats.cells_written, 30);
        assert!(
            s.0.lock().unwrap().windows(4).any(|x| x == b"\x1b[2J"),
            "Vollbild muss den Schirm löschen"
        );
    }

    #[test]
    fn unveraenderter_frame_schreibt_keine_zellen() {
        let mut w = Writer::new(Box::new(Sammler::default()), false, false);
        let g = grid(20, 5, 'x');
        let l = layout(20, 5);
        w.draw(&g, &l, ColorMode::Truecolor, false, None, 6)
            .unwrap();
        w.draw(&g, &l, ColorMode::Truecolor, false, None, 6)
            .unwrap();
        assert_eq!(
            w.stats.cells_written, 0,
            "nichts geändert -> nichts schreiben"
        );
    }

    #[test]
    fn nur_die_geaenderte_zelle_wird_neu_geschrieben() {
        let mut w = Writer::new(Box::new(Sammler::default()), false, false);
        let l = layout(30, 4);
        let g = grid(30, 4, 'x');
        w.draw(&g, &l, ColorMode::Truecolor, false, None, 5)
            .unwrap();

        let mut g2 = g.clone();
        g2.cells[15].ch = 'Y';
        w.draw(&g2, &l, ColorMode::Truecolor, false, None, 5)
            .unwrap();
        assert_eq!(w.stats.cells_written, 1);
    }

    #[test]
    fn farbwechsel_erzwingt_vollbild() {
        let mut w = Writer::new(Box::new(Sammler::default()), false, false);
        let l = layout(10, 2);
        let g = grid(10, 2, 'x');
        w.draw(&g, &l, ColorMode::Truecolor, false, None, 3)
            .unwrap();
        w.draw(&g, &l, ColorMode::Ansi256, false, None, 3).unwrap();
        assert_eq!(
            w.stats.cells_written, 20,
            "sonst bleiben alte Farbcodes auf dem Schirm stehen"
        );
    }

    #[test]
    fn sgr_lauflaenge_spart_gegenueber_naiv() {
        let l = layout(60, 20);
        let mut g = grid(60, 20, '#');
        // Realistischer Fall: waagerechte Flächen gleicher Farbe.
        for (i, c) in g.cells.iter_mut().enumerate() {
            c.fg = Rgb((i / 60) as u8 * 10, 40, 60);
        }

        let mut opt = Writer::new(Box::new(Sammler::default()), false, false);
        opt.draw(&g, &l, ColorMode::Truecolor, false, None, 21)
            .unwrap();

        let mut naiv = Writer::new(Box::new(Sammler::default()), false, true);
        naiv.draw(&g, &l, ColorMode::Truecolor, false, None, 21)
            .unwrap();

        assert!(
            opt.stats.bytes * 4 < naiv.stats.bytes,
            "Lauflängen müssen deutlich sparen: {} vs {}",
            opt.stats.bytes,
            naiv.stats.bytes
        );
    }

    #[test]
    fn kleine_luecken_werden_mitgeschrieben() {
        // Zwei Änderungen mit einer sauberen Zelle dazwischen: ein Lauf.
        let cur = vec![
            Cell {
                ch: 'a',
                ..Default::default()
            },
            Cell {
                ch: 'b',
                ..Default::default()
            },
            Cell {
                ch: 'c',
                ..Default::default()
            },
        ];
        let prev = vec![
            Cell {
                ch: 'x',
                ..Default::default()
            },
            Cell {
                ch: 'b',
                ..Default::default()
            },
            Cell {
                ch: 'z',
                ..Default::default()
            },
        ];
        let mut runs = Vec::new();
        collect_runs(&cur, &prev, false, &mut runs);
        assert_eq!(runs, vec![(0, 3)]);
    }

    #[test]
    fn grosse_luecken_werden_uebersprungen() {
        let mut cur = vec![Cell::default(); 20];
        let prev = cur.clone();
        cur[0].ch = 'a';
        cur[19].ch = 'b';
        let mut runs = Vec::new();
        collect_runs(&cur, &prev, false, &mut runs);
        assert_eq!(runs, vec![(0, 1), (19, 20)]);
    }

    #[test]
    fn unicode_zeichen_kommen_vollstaendig_an() {
        let s = Sammler::default();
        let mut w = Writer::new(Box::new(s.clone()), false, false);
        let mut g = Grid::new(1, 1);
        g.cells[0].ch = '▀';
        w.draw(&g, &layout(1, 1), ColorMode::Mono, false, None, 2)
            .unwrap();
        let out = s.0.lock().unwrap().clone();
        assert!(
            out.windows(3).any(|x| x == "▀".as_bytes()),
            "Blockzeichen muss als UTF-8 im Strom stehen"
        );
    }

    #[test]
    fn sync_klammert_den_frame() {
        let s = Sammler::default();
        let mut w = Writer::new(Box::new(s.clone()), true, false);
        w.draw(
            &grid(4, 2, 'x'),
            &layout(4, 2),
            ColorMode::Mono,
            false,
            None,
            3,
        )
        .unwrap();
        let out = s.0.lock().unwrap().clone();
        assert!(out.starts_with(SYNC_ON));
        assert!(out.ends_with(SYNC_OFF));
    }

    #[test]
    fn mono_schreibt_keine_farbcodes() {
        let s = Sammler::default();
        let mut w = Writer::new(Box::new(s.clone()), false, false);
        w.draw(
            &grid(8, 2, 'x'),
            &layout(8, 2),
            ColorMode::Mono,
            false,
            None,
            3,
        )
        .unwrap();
        let out = String::from_utf8_lossy(&s.0.lock().unwrap().clone()).to_string();
        assert!(!out.contains("38;2"), "mono darf keine RGB-Codes senden");
        assert!(
            !out.contains("[49m"),
            "mono braucht auch keinen Hintergrundreset"
        );
    }
}
