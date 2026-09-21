//! Die Befehlszeile. Flags sind die Hauptsteuerung -- die Tasten während der
//! Wiedergabe verändern nur, was sich sinnvoll live umschalten lässt.

use crate::render::geometry::Fit;
use crate::render::{Charset, Mode, color::ColorMode};
use crate::source::ffmpeg::Scaler;
use anyhow::{Result, bail};
use clap::Parser;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "vidinsh",
    version,
    about = "Video als farbiges ASCII im Terminal",
    long_about = "Spielt Videos, Streams und Kamerabilder als farbige Zeichengrafik \
                  im Terminal ab.\n\n\
                  Die Shell spielt keine Rolle -- vidinsh schreibt ANSI-Bytes nach \
                  stdout. Was zählt, ist der Terminal-Emulator; mit --probe siehst \
                  du, was er kann."
)]
pub struct Args {
    /// Datei, URL (HLS/DASH/RTSP/RTMP/SRT), YouTube-Link, cam:0 oder - für stdin
    #[arg(value_name = "EINGABE", required_unless_present_any = ["probe", "list_devices"])]
    pub input: Option<String>,

    // ------------------------------------------------------------------ Optik
    /// Renderverfahren
    #[arg(
        short = 'm',
        long,
        value_enum,
        default_value = "ramp",
        help_heading = "Optik"
    )]
    pub mode: Mode,

    /// Zeichenrampe von hell nach dunkel
    #[arg(long, default_value = crate::render::DEFAULT_RAMP, help_heading = "Optik")]
    pub ramp: String,

    /// Zeichensatz; ohne Angabe passend zum Modus und zum Terminal
    #[arg(long, value_enum, help_heading = "Optik")]
    pub charset: Option<Charset>,

    /// TTF für die Glyph-Masken; ohne Angabe die Schrift des Terminals
    #[arg(long, value_name = "PFAD", help_heading = "Optik")]
    pub font: Option<PathBuf>,

    /// Farbtiefe; ohne Angabe wird sie erkannt
    #[arg(short = 'c', long, value_enum, help_heading = "Optik")]
    pub color: Option<ColorMode>,

    /// Hintergrundfarbe mitfärben
    #[arg(long, help_heading = "Optik")]
    pub bg: bool,

    /// Bayer-Dithering; hebt 256 und 16 Farben deutlich
    #[arg(long, help_heading = "Optik")]
    pub dither: bool,

    /// Rampe umkehren, für helle Terminals
    #[arg(long, help_heading = "Optik")]
    pub invert: bool,

    /// Kurzform für --charset ascii
    #[arg(long, help_heading = "Optik")]
    pub ascii: bool,

    /// edge: ab welcher Kantenstärke ein Richtungszeichen gesetzt wird
    #[arg(long, default_value = "0.10", value_name = "F", help_heading = "Optik")]
    pub edge_threshold: f32,

    /// edge: Radius des engeren Weichzeichners
    #[arg(long, default_value = "1.0", value_name = "F", help_heading = "Optik")]
    pub edge_sigma: f32,

    // ------------------------------------------------------------- Geometrie
    /// Raster erzwingen, etwa 120x40; ohne Angabe die Terminalgröße
    #[arg(short = 's', long, value_name = "BxH", value_parser = parse_size, help_heading = "Geometrie")]
    pub size: Option<(u16, u16)>,

    /// Verhältnis Höhe zu Breite einer Terminalzelle
    #[arg(
        long,
        default_value = "2.0",
        value_name = "F",
        help_heading = "Geometrie"
    )]
    pub cell_aspect: f64,

    /// Wie das Bild ins Terminal gelegt wird
    #[arg(
        long,
        value_enum,
        default_value = "contain",
        help_heading = "Geometrie"
    )]
    pub fit: Fit,

    /// Abtastung pro Zelle; feiner kostet CPU, gröber kostet Detail
    #[arg(long, default_value = "4x8", value_name = "BxH", value_parser = parse_size32, help_heading = "Geometrie")]
    pub supersample: (u32, u32),

    // ------------------------------------------------------------------- Zeit
    /// Ziel-Framerate; ohne Angabe die der Quelle
    #[arg(short = 'f', long, value_name = "N", help_heading = "Zeit")]
    pub fps: Option<f64>,

    /// Wiedergabegeschwindigkeit
    #[arg(long, default_value = "1.0", value_name = "F", help_heading = "Zeit")]
    pub speed: f64,

    /// Startposition, etwa 90 oder 1:30
    #[arg(long, value_name = "ZEIT", value_parser = parse_time, help_heading = "Zeit")]
    pub ss: Option<f64>,

    /// Endposition
    #[arg(long, value_name = "ZEIT", value_parser = parse_time, help_heading = "Zeit")]
    pub to: Option<f64>,

    /// Endlos wiederholen
    #[arg(long = "loop", help_heading = "Zeit")]
    pub loop_forever: bool,

    /// Ein Einzelbild rendern und beenden
    #[arg(long, help_heading = "Zeit")]
    pub once: bool,

    // ------------------------------------------------------------------- Bild
    #[arg(long, default_value = "1.0", value_name = "F", help_heading = "Bild")]
    pub brightness: f32,

    #[arg(long, default_value = "1.0", value_name = "F", help_heading = "Bild")]
    pub contrast: f32,

    #[arg(long, default_value = "1.0", value_name = "F", help_heading = "Bild")]
    pub saturation: f32,

    /// Helligkeit je Bild normalisieren; hilft bei dunklem Material
    #[arg(long, help_heading = "Bild")]
    pub auto_contrast: bool,

    /// In linearem Licht skalieren; korrekter, braucht libzimg
    #[arg(long, help_heading = "Bild")]
    pub gamma_correct: bool,

    #[arg(long, value_enum, default_value = "area", help_heading = "Bild")]
    pub scaler: Scaler,

    // -------------------------------------------------------------------- Ton
    /// Ton abspielen (Vorgabe bei Dateien und Streams mit Audiospur)
    #[arg(long, overrides_with = "no_audio", help_heading = "Ton")]
    pub audio: bool,

    /// Ohne Ton
    #[arg(long, help_heading = "Ton")]
    pub no_audio: bool,

    #[arg(
        long,
        default_value = "100",
        value_name = "0-100",
        help_heading = "Ton"
    )]
    pub volume: u32,

    // -------------------------------------------------------------- Sonstiges
    /// Erkannte Terminal-Fähigkeiten und ein Testbild zeigen, dann beenden
    #[arg(long, help_heading = "Sonstiges")]
    pub probe: bool,

    /// Statuszeile ausblenden
    #[arg(long, help_heading = "Sonstiges")]
    pub no_ui: bool,

    /// Bildrate, verworfene Bilder und Bandbreite anzeigen
    #[arg(long, help_heading = "Sonstiges")]
    pub stats: bool,

    /// ANSI-Ausgabe in eine Datei schreiben statt ins Terminal
    #[arg(long, value_name = "DATEI", help_heading = "Sonstiges")]
    pub write: Option<PathBuf>,

    /// Verfügbare Kameras auflisten
    #[arg(long, help_heading = "Sonstiges")]
    pub list_devices: bool,

    /// Eigene ffmpeg-Programmdatei benutzen
    #[arg(long, value_name = "PFAD", help_heading = "Sonstiges")]
    pub ffmpeg: Option<PathBuf>,

    /// Maximale Höhe, die yt-dlp holen soll
    #[arg(
        long,
        default_value = "720",
        value_name = "PIXEL",
        help_heading = "Sonstiges"
    )]
    pub max_height: u32,

    /// ffmpeg-Kommando und erkannte Fähigkeiten ausgeben
    #[arg(short = 'v', long, help_heading = "Sonstiges")]
    pub verbose: bool,

    /// Vergleichsmaßstab: ohne Diffing und ohne SGR-Lauflängen ausgeben
    #[arg(long, hide = true)]
    pub bench_naive: bool,
}

impl Args {
    /// Soll Ton laufen? `--no-audio` schlägt `--audio`.
    pub fn want_audio(&self) -> bool {
        !self.no_audio && !self.once && self.write.is_none()
    }

    pub fn effective_charset(&self) -> Option<Charset> {
        if self.ascii {
            Some(Charset::Ascii)
        } else {
            self.charset
        }
    }

    pub fn validate(&self) -> Result<()> {
        if self.cell_aspect <= 0.0 || !self.cell_aspect.is_finite() {
            bail!("--cell-aspect muss größer als 0 sein");
        }
        if self.volume > 100 {
            bail!("--volume liegt zwischen 0 und 100");
        }
        if let (Some(a), Some(b)) = (self.ss, self.to)
            && b <= a
        {
            bail!("--to ({b}) muss nach --ss ({a}) liegen");
        }
        if let Some(f) = self.fps
            && !(0.1..=240.0).contains(&f)
        {
            bail!("--fps liegt zwischen 0.1 und 240");
        }
        if self.ramp.is_empty() {
            bail!("--ramp darf nicht leer sein");
        }
        if !(0.05..=8.0).contains(&self.edge_sigma) {
            bail!("--edge-sigma liegt zwischen 0.05 und 8");
        }
        if self.edge_threshold < 0.0 {
            bail!("--edge-threshold ist nicht negativ");
        }
        Ok(())
    }
}

// ------------------------------------------------------------------- Parser

fn split_x(s: &str) -> Option<(&str, &str)> {
    s.split_once(['x', 'X', '*'])
}

fn parse_size(s: &str) -> Result<(u16, u16), String> {
    let (w, h) = split_x(s).ok_or_else(|| format!("'{s}' ist kein BxH, etwa 120x40"))?;
    let w: u16 = w
        .trim()
        .parse()
        .map_err(|_| format!("'{w}' ist keine Breite"))?;
    let h: u16 = h
        .trim()
        .parse()
        .map_err(|_| format!("'{h}' ist keine Höhe"))?;
    if w == 0 || h == 0 {
        return Err("Breite und Höhe müssen größer als 0 sein".into());
    }
    Ok((w, h))
}

fn parse_size32(s: &str) -> Result<(u32, u32), String> {
    let (w, h) = split_x(s).ok_or_else(|| format!("'{s}' ist kein BxH, etwa 4x8"))?;
    let w: u32 = w
        .trim()
        .parse()
        .map_err(|_| format!("'{w}' ist keine Zahl"))?;
    let h: u32 = h
        .trim()
        .parse()
        .map_err(|_| format!("'{h}' ist keine Zahl"))?;
    if w == 0 || h == 0 || w > 16 || h > 16 {
        return Err("Abtastung liegt zwischen 1x1 und 16x16".into());
    }
    Ok((w, h))
}

/// Akzeptiert `90`, `12.5`, `1:30` und `1:02:03`.
fn parse_time(s: &str) -> Result<f64, String> {
    let teile: Vec<&str> = s.split(':').collect();
    if teile.len() > 3 {
        return Err(format!("'{s}' ist keine Zeitangabe"));
    }
    let mut sek = 0.0f64;
    for t in &teile {
        let v: f64 = t
            .trim()
            .parse()
            .map_err(|_| format!("'{t}' in '{s}' ist keine Zahl"))?;
        if v < 0.0 {
            return Err("Zeitangaben sind nicht negativ".into());
        }
        sek = sek * 60.0 + v;
    }
    Ok(sek)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn clap_definition_ist_widerspruchsfrei() {
        Args::command().debug_assert();
    }

    #[test]
    fn groessen_werden_gelesen() {
        assert_eq!(parse_size("120x40").unwrap(), (120, 40));
        assert_eq!(parse_size("120X40").unwrap(), (120, 40));
        assert!(parse_size("120").is_err());
        assert!(parse_size("0x40").is_err());
        assert!(parse_size("axb").is_err());
    }

    #[test]
    fn abtastung_wird_begrenzt() {
        assert_eq!(parse_size32("4x8").unwrap(), (4, 8));
        assert!(
            parse_size32("32x8").is_err(),
            "zu fein wäre Speicherverschwendung"
        );
        assert!(parse_size32("0x8").is_err());
    }

    #[test]
    fn zeitangaben_in_allen_schreibweisen() {
        assert_eq!(parse_time("90").unwrap(), 90.0);
        assert_eq!(parse_time("12.5").unwrap(), 12.5);
        assert_eq!(parse_time("1:30").unwrap(), 90.0);
        assert_eq!(parse_time("1:02:03").unwrap(), 3723.0);
        assert!(parse_time("1:2:3:4").is_err());
        assert!(parse_time("-5").is_err());
    }

    fn parse(args: &[&str]) -> Args {
        Args::try_parse_from(std::iter::once("vidinsh").chain(args.iter().copied())).unwrap()
    }

    #[test]
    fn eingabe_ist_pflicht_ausser_bei_probe_und_geraeteliste() {
        assert!(Args::try_parse_from(["vidinsh"]).is_err());
        assert!(Args::try_parse_from(["vidinsh", "--probe"]).is_ok());
        assert!(Args::try_parse_from(["vidinsh", "--list-devices"]).is_ok());
    }

    #[test]
    fn no_audio_schlaegt_audio() {
        assert!(parse(&["x.mp4"]).want_audio());
        assert!(!parse(&["x.mp4", "--no-audio"]).want_audio());
        assert!(!parse(&["x.mp4", "--audio", "--no-audio"]).want_audio());
    }

    #[test]
    fn einzelbild_und_dateiausgabe_brauchen_keinen_ton() {
        assert!(!parse(&["x.jpg", "--once"]).want_audio());
        assert!(!parse(&["x.mp4", "--write", "out.ans"]).want_audio());
    }

    #[test]
    fn ascii_kurzform_uebersteuert_den_zeichensatz() {
        assert_eq!(
            parse(&["x.mp4", "--ascii"]).effective_charset(),
            Some(Charset::Ascii)
        );
        assert_eq!(
            parse(&["x.mp4", "--charset", "quad"]).effective_charset(),
            Some(Charset::Quad)
        );
        assert_eq!(parse(&["x.mp4"]).effective_charset(), None);
    }

    #[test]
    fn farbmodi_heissen_wie_dokumentiert() {
        assert_eq!(
            parse(&["x.mp4", "-c", "256"]).color,
            Some(ColorMode::Ansi256)
        );
        assert_eq!(parse(&["x.mp4", "-c", "16"]).color, Some(ColorMode::Ansi16));
        assert_eq!(
            parse(&["x.mp4", "-c", "truecolor"]).color,
            Some(ColorMode::Truecolor)
        );
    }

    #[test]
    fn widerspruechliche_angaben_werden_abgelehnt() {
        assert!(
            parse(&["x.mp4", "--ss", "30", "--to", "10"])
                .validate()
                .is_err()
        );
        assert!(parse(&["x.mp4", "--volume", "101"]).validate().is_err());
        assert!(parse(&["x.mp4", "--cell-aspect", "0"]).validate().is_err());
        assert!(parse(&["x.mp4", "--fps", "500"]).validate().is_err());
        assert!(
            parse(&["x.mp4", "--ss", "10", "--to", "30"])
                .validate()
                .is_ok()
        );
    }

    #[test]
    fn loop_heisst_auf_der_kommandozeile_loop() {
        assert!(parse(&["x.mp4", "--loop"]).loop_forever);
    }
}
