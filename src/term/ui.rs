//! Die Statuszeile am unteren Rand.

use crate::control::clock::format_hms;
use crate::render::{Charset, Mode, color::ColorMode};
use std::time::Duration;

#[derive(Clone, Copy, Debug, Default)]
pub struct Stats {
    pub fps: f64,
    pub dropped: u64,
    pub bytes: usize,
    pub cells_written: usize,
    pub cells_total: usize,
}

pub struct Status<'a> {
    pub quelle: &'a str,
    pub mode: Mode,
    pub color: ColorMode,
    pub charset: Charset,
    pub grid: (u16, u16),
    pub pos: Duration,
    pub dauer: Option<f64>,
    pub paused: bool,
    pub speed: f64,
    pub live: bool,
    /// Lautstärke, oder `None` wenn diese Quelle keinen Ton hat
    pub volume: Option<u32>,
    pub stats: Option<Stats>,
}

/// Fertige Zeile inklusive Invertierung, auf `width` gekürzt.
pub fn render(s: &Status, width: u16) -> String {
    let mut t = String::with_capacity(width as usize + 16);

    t.push_str(if s.paused { "|| " } else { "> " });
    t.push_str(s.quelle);

    if s.live {
        t.push_str("  live");
    } else {
        t.push_str("  ");
        t.push_str(&format_hms(s.pos));
        if let Some(d) = s.dauer {
            t.push('/');
            t.push_str(&format_hms(Duration::from_secs_f64(d.max(0.0))));
        }
    }

    if (s.speed - 1.0).abs() > 0.01 {
        t.push_str(&format!("  {:.2}x", s.speed));
    }

    // Ohne Anzeige ist nicht zu unterscheiden, ob der Ton stumm gedreht ist
    // oder die Quelle gar keinen hat.
    match s.volume {
        Some(0) => t.push_str("  Ton stumm"),
        Some(v) => t.push_str(&format!("  Ton {v}%")),
        None => t.push_str("  ohne Ton"),
    }

    t.push_str(&format!(
        "  {}/{}  {}  {}x{}",
        s.mode.label(),
        s.charset.label(),
        s.color.label(),
        s.grid.0,
        s.grid.1
    ));

    if let Some(st) = s.stats {
        t.push_str(&format!("  {:.1}fps", st.fps));
        if st.dropped > 0 {
            t.push_str(&format!("  {} weg", st.dropped));
        }
        t.push_str(&format!("  {}/Bild", human_bytes(st.bytes)));
        if let Some(anteil) = (st.cells_written * 100).checked_div(st.cells_total) {
            t.push_str(&format!("  {anteil}% Zellen"));
        }
    }

    let hinweis = "  [1-4 Modus  c Farbe  Leer Pause  q Ende]";
    if t.chars().count() + hinweis.chars().count() <= width as usize {
        t.push_str(hinweis);
    }

    format!("\x1b[7m{}\x1b[0m", pad_fit(&t, width as usize))
}

/// Auf genau `width` Zeichen bringen. Die Invertierung soll die ganze Zeile
/// füllen, sonst hängt ein abgerissener Balken in der Mitte.
fn pad_fit(s: &str, width: usize) -> String {
    let n = s.chars().count();
    if n > width {
        s.chars().take(width).collect()
    } else {
        let mut r = String::from(s);
        r.extend(std::iter::repeat_n(' ', width - n));
        r
    }
}

fn human_bytes(n: usize) -> String {
    if n >= 1 << 20 {
        format!("{:.1}MB", n as f64 / (1 << 20) as f64)
    } else if n >= 1024 {
        format!("{}KB", n / 1024)
    } else {
        format!("{n}B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status() -> Status<'static> {
        Status {
            quelle: "test.mp4",
            mode: Mode::Ramp,
            color: ColorMode::Truecolor,
            charset: Charset::Extended,
            grid: (200, 56),
            pos: Duration::from_secs(3),
            dauer: Some(10.0),
            paused: false,
            speed: 1.0,
            live: false,
            volume: Some(100),
            stats: None,
        }
    }

    /// Sichtbare Länge ohne die Escape-Sequenzen.
    fn sichtbar(s: &str) -> usize {
        s.replace("\x1b[7m", "")
            .replace("\x1b[0m", "")
            .chars()
            .count()
    }

    #[test]
    fn zeile_fuellt_die_breite_genau() {
        for w in [20u16, 40, 80, 200] {
            assert_eq!(sichtbar(&render(&status(), w)), w as usize, "Breite {w}");
        }
    }

    #[test]
    fn schmale_terminals_werden_gekuerzt_statt_umzubrechen() {
        let z = render(&status(), 12);
        assert_eq!(sichtbar(&z), 12);
        assert!(!z.contains('\n'));
    }

    #[test]
    fn position_und_dauer_stehen_drin() {
        let z = render(&status(), 120);
        assert!(z.contains("0:03/0:10"), "{z}");
    }

    #[test]
    fn live_quellen_zeigen_keine_dauer() {
        let mut s = status();
        s.live = true;
        let z = render(&s, 120);
        assert!(z.contains("live"));
        assert!(!z.contains("0:03/"));
    }

    #[test]
    fn pause_ist_erkennbar() {
        let mut s = status();
        s.paused = true;
        assert!(render(&s, 120).contains("||"));
    }

    #[test]
    fn ton_zustand_ist_immer_ablesbar() {
        let mut s = status();
        assert!(render(&s, 200).contains("Ton 100%"));
        s.volume = Some(0);
        assert!(render(&s, 200).contains("stumm"));
        s.volume = None;
        let z = render(&s, 200);
        assert!(z.contains("ohne Ton"), "{z}");
        assert!(
            !z.contains("Ton 0"),
            "eine tonlose Quelle ist nicht stummgedreht"
        );
    }

    #[test]
    fn tempo_erscheint_nur_wenn_es_abweicht() {
        assert!(!render(&status(), 120).contains("x  ramp"));
        let mut s = status();
        s.speed = 2.0;
        assert!(render(&s, 120).contains("2.00x"));
    }

    #[test]
    fn stats_zeigen_bandbreite_und_zellanteil() {
        let mut s = status();
        s.stats = Some(Stats {
            fps: 29.9,
            dropped: 3,
            bytes: 41 * 1024,
            cells_written: 5600,
            cells_total: 11200,
        });
        let z = render(&s, 200);
        assert!(z.contains("29.9fps"), "{z}");
        assert!(z.contains("3 weg"), "{z}");
        assert!(z.contains("41KB/Bild"), "{z}");
        assert!(z.contains("50% Zellen"), "{z}");
    }

    #[test]
    fn byteformat_skaliert() {
        assert_eq!(human_bytes(512), "512B");
        assert_eq!(human_bytes(2048), "2KB");
        assert_eq!(human_bytes(3 << 20), "3.0MB");
    }
}
