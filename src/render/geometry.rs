//! Rasterberechnung: aus Terminalgröße und Videoformat das Zellenraster.
//!
//! Eine Terminalzelle ist höher als breit -- typisch etwa 1:2. Ohne diese
//! Korrektur wird jedes Bild doppelt so hoch dargestellt wie es sein sollte.

use clap::ValueEnum;

#[derive(Clone, Copy, PartialEq, Eq, Debug, ValueEnum)]
pub enum Fit {
    /// vollständig sichtbar, Rest bleibt leer (Letterbox)
    Contain,
    /// Terminal füllen, Überstand wird beschnitten
    Cover,
    /// Terminal füllen, Seitenverhältnis verzerren
    Stretch,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    /// Raster in Zellen
    pub grid_w: u16,
    pub grid_h: u16,
    /// Versatz für die Zentrierung im Terminal
    pub off_x: u16,
    pub off_y: u16,
    /// Pixelraster, das ffmpeg liefern soll: grid * supersample
    pub px_w: u32,
    pub px_h: u32,
    /// Abtastung pro Zelle
    pub ss_x: u32,
    pub ss_y: u32,
}

impl Layout {
    pub fn cells(&self) -> usize {
        self.grid_w as usize * self.grid_h as usize
    }
}

/// `video_aspect` ist Breite/Höhe der Quelle, `cell_aspect` Höhe/Breite einer
/// Terminalzelle (typisch 2.0).
pub fn compute(
    term_w: u16,
    term_h: u16,
    video_aspect: f64,
    cell_aspect: f64,
    fit: Fit,
    ss: (u32, u32),
    forced: Option<(u16, u16)>,
) -> Layout {
    let term_w = term_w.max(1);
    let term_h = term_h.max(1);

    let (gw, gh) = match forced {
        Some((w, h)) => (w.max(1), h.max(1)),
        None => match fit {
            Fit::Cover | Fit::Stretch => (term_w, term_h),
            Fit::Contain => {
                // Physisches Seitenverhältnis des Rasters:
                //   grid_w / (grid_h * cell_aspect)  ==  video_aspect
                let va = if video_aspect.is_finite() && video_aspect > 0.0 {
                    video_aspect
                } else {
                    16.0 / 9.0
                };
                let ca = if cell_aspect.is_finite() && cell_aspect > 0.0 {
                    cell_aspect
                } else {
                    2.0
                };

                let h_for_full_width = (term_w as f64 / (va * ca)).round();
                if h_for_full_width >= 1.0 && h_for_full_width <= term_h as f64 {
                    (term_w, h_for_full_width as u16)
                } else {
                    let w = (term_h as f64 * va * ca).round().clamp(1.0, term_w as f64);
                    (w as u16, term_h)
                }
            }
        },
    };

    let gw = gw.min(term_w).max(1);
    let gh = gh.min(term_h).max(1);

    Layout {
        grid_w: gw,
        grid_h: gh,
        off_x: (term_w - gw) / 2,
        off_y: (term_h - gh) / 2,
        px_w: gw as u32 * ss.0.max(1),
        px_h: gh as u32 * ss.1.max(1),
        ss_x: ss.0.max(1),
        ss_y: ss.1.max(1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SS: (u32, u32) = (4, 8);

    #[test]
    fn contain_haelt_das_seitenverhaeltnis() {
        // 16:9 in einem 200x60-Terminal bei Zellverhältnis 2.0:
        // 200 / (1.778 * 2.0) = 56.25 -> 56 Zeilen, passt in 60.
        let l = compute(200, 60, 16.0 / 9.0, 2.0, Fit::Contain, SS, None);
        assert_eq!((l.grid_w, l.grid_h), (200, 56));
        assert_eq!(l.off_y, 2); // (60-56)/2
        assert_eq!(l.off_x, 0);
    }

    #[test]
    fn contain_weicht_auf_die_hoehe_aus() {
        // Hochformat 9:16 in 200x60: volle Breite ergäbe 200/(0.5625*2)=178
        // Zeilen -- zu hoch, also an der Höhe ausrichten.
        let l = compute(200, 60, 9.0 / 16.0, 2.0, Fit::Contain, SS, None);
        assert_eq!(l.grid_h, 60);
        assert_eq!(l.grid_w, 68); // 60 * 0.5625 * 2 = 67.5 -> 68
        assert!(l.off_x > 0);
    }

    #[test]
    fn stretch_und_cover_fuellen_das_terminal() {
        for fit in [Fit::Stretch, Fit::Cover] {
            let l = compute(120, 40, 16.0 / 9.0, 2.0, fit, SS, None);
            assert_eq!((l.grid_w, l.grid_h), (120, 40));
            assert_eq!((l.off_x, l.off_y), (0, 0));
        }
    }

    #[test]
    fn pixelraster_ist_das_vielfache_der_abtastung() {
        let l = compute(100, 50, 1.0, 2.0, Fit::Stretch, (4, 8), None);
        assert_eq!(l.px_w, 400);
        assert_eq!(l.px_h, 400);
    }

    #[test]
    fn erzwungene_groesse_wird_am_terminal_gekappt() {
        let l = compute(80, 24, 1.0, 2.0, Fit::Contain, SS, Some((999, 999)));
        assert_eq!((l.grid_w, l.grid_h), (80, 24));
    }

    #[test]
    fn winzige_terminals_ergeben_kein_nullraster() {
        let l = compute(1, 1, 16.0 / 9.0, 2.0, Fit::Contain, SS, None);
        assert!(l.grid_w >= 1 && l.grid_h >= 1);
        assert!(l.px_w >= 1 && l.px_h >= 1);
    }

    #[test]
    fn unsinnige_seitenverhaeltnisse_stuerzen_nicht_ab() {
        for va in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            let l = compute(100, 40, va, 2.0, Fit::Contain, SS, None);
            assert!(l.grid_w >= 1 && l.grid_h >= 1);
        }
    }
}
