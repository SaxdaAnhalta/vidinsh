//! ffmpeg als Kindprozess: dekodiert, skaliert und schiebt rohe rgb24-Bilder
//! durch eine Pipe.
//!
//! Skaliert wird bewusst dort und nicht hier. swscale ist SIMD-optimiert und
//! weit schneller als alles, was sich mit vertretbarem Aufwand in Rust
//! nachbauen ließe. Angefordert wird das *Supersample*-Raster -- also
//! `Spalten*4 x Zeilen*8` statt `Spalten x Zeilen`. Jeder Renderer mittelt
//! sich daraus herunter, was er braucht, und ein Moduswechsel bleibt eine
//! reine Render-Operation ohne Neustart des Prozesses.

use super::Frame;
use super::input::Input;
use crate::render::geometry::{Fit, Layout};
use anyhow::{Context, Result, bail};
use clap::ValueEnum;
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy, PartialEq, Eq, Debug, ValueEnum)]
pub enum Scaler {
    /// Flächenmittel -- das Richtige beim starken Verkleinern
    Area,
    Bilinear,
    Bicubic,
    Lanczos,
}

impl Scaler {
    fn flag(self) -> &'static str {
        match self {
            Scaler::Area => "area",
            Scaler::Bilinear => "bilinear",
            Scaler::Bicubic => "bicubic",
            Scaler::Lanczos => "lanczos",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Config {
    pub input: Input,
    pub fps: f64,
    pub start: Option<f64>,
    pub end: Option<f64>,
    pub scaler: Scaler,
    pub fit: Fit,
    pub gamma_correct: bool,
    pub loop_forever: bool,
    /// Darf ffmpeg zum Spulen eine Range-Anfrage stellen?
    ///
    /// Manche Server -- googlevideo, also jede aufgeloeste YouTube-Adresse --
    /// beantworten die nicht, sondern lassen die Verbindung offen stehen.
    /// Mit `false` ueberspringt ffmpeg stattdessen sequenziell: langsamer,
    /// aber es kommt ueberhaupt ein Bild.
    pub seekable: bool,
}

/// Wie viele stderr-Zeilen wir vorhalten, um im Fehlerfall etwas Brauchbares
/// zeigen zu können.
const STDERR_TAIL: usize = 12;

/// Wie ein ffmpeg-Lauf geendet hat.
pub struct Abschluss {
    pub erfolg: bool,
    pub meldungen: String,
    /// wie viele Bilder insgesamt durch die Pipe kamen
    pub bilder: u64,
}

pub struct FfmpegSource {
    child: Child,
    out: BufReader<ChildStdout>,
    stderr: Arc<Mutex<VecDeque<String>>>,
    frame_bytes: usize,
    w: u32,
    h: u32,
    index: u64,
    pub command_line: String,
}

pub fn build_args(cfg: &Config, layout: &Layout) -> Vec<String> {
    let mut a: Vec<String> = vec![
        "-hide_banner".into(),
        "-loglevel".into(),
        "error".into(),
        "-nostdin".into(),
    ];

    if cfg.loop_forever && !cfg.input.is_live {
        a.push("-stream_loop".into());
        a.push("-1".into());
    }
    a.extend(cfg.input.pre_args.iter().cloned());

    if !cfg.seekable {
        a.push("-seekable".into());
        a.push("0".into());
    }

    // -ss vor -i: ffmpeg springt dann im Container statt alles zu dekodieren.
    if let Some(s) = cfg.start.filter(|s| *s > 0.0) {
        a.push("-ss".into());
        a.push(format!("{s:.3}"));
    }
    a.push("-i".into());
    a.push(cfg.input.ffmpeg_input.clone());

    if let Some(e) = cfg.end {
        let dauer = e - cfg.start.unwrap_or(0.0);
        if dauer > 0.0 {
            a.push("-t".into());
            a.push(format!("{dauer:.3}"));
        }
    }

    // Ton, Untertitel und Datenspuren interessieren hier nicht -- der Ton
    // läuft über einen eigenen Prozess.
    a.extend(["-an", "-sn", "-dn"].map(String::from));

    a.push("-vf".into());
    a.push(filter_chain(cfg, layout));

    a.extend(["-f", "rawvideo", "-pix_fmt", "rgb24"].map(String::from));

    // Konstante Bildrate erzwingen. Dadurch ist der Zeitstempel eines Bildes
    // schlicht seine laufende Nummer -- kein PTS aus der Pipe zu fischen.
    if cfg.fps > 0.0 {
        a.push("-r".into());
        a.push(format!("{:.6}", cfg.fps));
    }
    a.push("pipe:1".into());
    a
}

fn filter_chain(cfg: &Config, l: &Layout) -> String {
    let (w, h) = (l.px_w, l.px_h);
    let f = cfg.scaler.flag();

    // Bei Contain ist das Raster bereits im Seitenverhältnis der Quelle
    // berechnet, deshalb genügt hier ein glattes scale.
    let scale = match cfg.fit {
        Fit::Cover => {
            format!("scale={w}:{h}:force_original_aspect_ratio=increase:flags={f},crop={w}:{h}")
        }
        Fit::Contain | Fit::Stretch => format!("scale={w}:{h}:flags={f}"),
    };

    if cfg.gamma_correct {
        // In linearem Licht verkleinern. Korrekter, kostet aber CPU und
        // braucht libzimg -- fehlt das, meldet ffmpeg es deutlich.
        format!("zscale=t=linear:npl=100,{scale},zscale=t=bt709,format=rgb24")
    } else {
        format!("{scale},format=rgb24")
    }
}

impl FfmpegSource {
    pub fn spawn(cfg: &Config, layout: &Layout) -> Result<Self> {
        let args = build_args(cfg, layout);
        let command_line = format!("ffmpeg {}", args.join(" "));

        let mut child = Command::new("ffmpeg")
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context(
                "ffmpeg lässt sich nicht starten. Liegt es im PATH? \
                 Prüfen mit: ffmpeg -version",
            )?;

        let out = BufReader::with_capacity(
            1 << 20,
            child
                .stdout
                .take()
                .expect("stdout wurde als Pipe angefordert"),
        );

        let stderr = Arc::new(Mutex::new(VecDeque::with_capacity(STDERR_TAIL)));
        if let Some(e) = child.stderr.take() {
            let sink = Arc::clone(&stderr);
            std::thread::spawn(move || {
                for line in BufReader::new(e).lines().map_while(Result::ok) {
                    let mut q = sink.lock().unwrap();
                    if q.len() == STDERR_TAIL {
                        q.pop_front();
                    }
                    q.push_back(line);
                }
            });
        }

        Ok(FfmpegSource {
            child,
            out,
            stderr,
            frame_bytes: (layout.px_w * layout.px_h * 3) as usize,
            w: layout.px_w,
            h: layout.px_h,
            index: 0,
            command_line,
        })
    }

    /// `Ok(None)` heißt sauberes Ende des Stroms.
    pub fn next_frame(&mut self) -> Result<Option<Frame>> {
        let mut data = vec![0u8; self.frame_bytes];
        let mut gelesen = 0usize;

        while gelesen < self.frame_bytes {
            match self.out.read(&mut data[gelesen..]) {
                Ok(0) => break,
                Ok(n) => gelesen += n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e).context("Lesen aus der ffmpeg-Pipe fehlgeschlagen"),
            }
        }

        if gelesen == 0 {
            return Ok(None); // Strom zu Ende
        }
        if gelesen < self.frame_bytes {
            // Angebrochenes Bild: ffmpeg ist mittendrin weggebrochen.
            bail!("ffmpeg hat abgebrochen.\n{}", self.stderr_tail());
        }

        let f = Frame {
            w: self.w,
            h: self.h,
            data,
            index: self.index,
        };
        self.index += 1;
        Ok(Some(f))
    }

    pub fn stderr_tail(&self) -> String {
        let q = self.stderr.lock().unwrap();
        if q.is_empty() {
            "(ffmpeg hat nichts gemeldet)".into()
        } else {
            q.iter().cloned().collect::<Vec<_>>().join("\n")
        }
    }

    /// Wartet auf das Prozessende und liefert die gesammelten Meldungen.
    ///
    /// Gewartet wird bewusst: nach dem Dateiende ist der Prozess oft noch
    /// nicht abgeräumt, und ein `try_wait` an dieser Stelle meldete dann
    /// fälschlich "alles in Ordnung".
    pub fn finish(&mut self) -> Abschluss {
        let erfolg = self.child.wait().map(|s| s.success()).unwrap_or(false);
        // Dem stderr-Thread einen Moment lassen, die letzten Zeilen zu holen.
        std::thread::sleep(std::time::Duration::from_millis(50));
        Abschluss {
            erfolg,
            meldungen: self.stderr_tail(),
            bilder: self.index,
        }
    }
}

impl Drop for FfmpegSource {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::geometry;
    use crate::source::input;

    fn cfg(datei: &str) -> Config {
        Config {
            input: input::classify(datei),
            fps: 30.0,
            start: None,
            end: None,
            scaler: Scaler::Area,
            fit: Fit::Contain,
            gamma_correct: false,
            loop_forever: false,
            seekable: true,
        }
    }

    fn layout() -> Layout {
        geometry::compute(100, 50, 16.0 / 9.0, 2.0, Fit::Contain, (4, 8), None)
    }

    fn args_von(c: &Config) -> Vec<String> {
        build_args(c, &layout())
    }

    fn enthaelt_paar(a: &[String], k: &str, v: &str) -> bool {
        a.windows(2).any(|w| w[0] == k && w[1] == v)
    }

    #[test]
    fn grundgeruest_stimmt() {
        let a = args_von(&cfg("x.mp4"));
        assert!(enthaelt_paar(&a, "-f", "rawvideo"));
        assert!(enthaelt_paar(&a, "-pix_fmt", "rgb24"));
        assert!(enthaelt_paar(&a, "-i", "x.mp4"));
        assert_eq!(a.last().unwrap(), "pipe:1");
        assert!(
            a.contains(&"-an".to_string()),
            "Ton gehört nicht in diese Pipe"
        );
    }

    #[test]
    fn skaliert_auf_das_supersample_raster() {
        let l = layout();
        let a = args_von(&cfg("x.mp4"));
        let vf = a[a.iter().position(|s| s == "-vf").unwrap() + 1].clone();
        assert!(
            vf.contains(&format!("scale={}:{}", l.px_w, l.px_h)),
            "erwartet wird das Supersample-Raster, nicht das Zellenraster: {vf}"
        );
        assert!(
            l.px_w > l.grid_w as u32,
            "Supersample muss feiner sein als das Raster"
        );
    }

    #[test]
    fn konstante_bildrate_wird_erzwungen() {
        let a = args_von(&cfg("x.mp4"));
        let i = a
            .iter()
            .position(|s| s == "-r")
            .expect("-r muss gesetzt sein");
        assert!(a[i + 1].starts_with("30"));
    }

    #[test]
    fn startzeit_steht_vor_dem_eingang() {
        let mut c = cfg("x.mp4");
        c.start = Some(12.5);
        let a = args_von(&c);
        let ss = a.iter().position(|s| s == "-ss").unwrap();
        let i = a.iter().position(|s| s == "-i").unwrap();
        assert!(ss < i, "-ss nach -i wäre langsames Suchen");
        assert_eq!(a[ss + 1], "12.500");
    }

    #[test]
    fn to_wird_zur_dauer_ab_startzeit() {
        let mut c = cfg("x.mp4");
        c.start = Some(10.0);
        c.end = Some(25.0);
        let a = args_von(&c);
        assert!(enthaelt_paar(&a, "-t", "15.000"));
    }

    #[test]
    fn schleife_nur_bei_nicht_live_quellen() {
        let mut c = cfg("x.mp4");
        c.loop_forever = true;
        assert!(enthaelt_paar(&args_von(&c), "-stream_loop", "-1"));

        let mut c = cfg("rtsp://host/live");
        c.loop_forever = true;
        assert!(
            !args_von(&c).contains(&"-stream_loop".to_string()),
            "eine Live-Quelle lässt sich nicht wiederholen"
        );
    }

    #[test]
    fn seekable_null_landet_vor_dem_eingang() {
        let mut c = cfg("https://host/videoplayback");
        c.seekable = false;
        c.start = Some(30.0);
        let a = args_von(&c);
        let sk = a
            .iter()
            .position(|s| s == "-seekable")
            .expect("-seekable fehlt");
        assert_eq!(a[sk + 1], "0");
        assert!(sk < a.iter().position(|s| s == "-i").unwrap());
    }

    #[test]
    fn seekable_ist_normalerweise_nicht_gesetzt() {
        assert!(!args_von(&cfg("x.mp4")).contains(&"-seekable".to_string()));
    }

    #[test]
    fn cover_schneidet_zu_statt_zu_verzerren() {
        let mut c = cfg("x.mp4");
        c.fit = Fit::Cover;
        let a = args_von(&c);
        let vf = a[a.iter().position(|s| s == "-vf").unwrap() + 1].clone();
        assert!(vf.contains("force_original_aspect_ratio=increase"));
        assert!(vf.contains("crop="));
    }

    #[test]
    fn gammakorrektur_klammert_die_skalierung() {
        let mut c = cfg("x.mp4");
        c.gamma_correct = true;
        let a = args_von(&c);
        let vf = a[a.iter().position(|s| s == "-vf").unwrap() + 1].clone();
        assert!(vf.starts_with("zscale=t=linear"));
        assert!(vf.contains("zscale=t=bt709"));
    }

    #[test]
    fn netzargumente_der_quelle_landen_vor_dem_eingang() {
        let c = cfg("https://example.com/live.m3u8");
        let a = args_von(&c);
        let rc = a.iter().position(|s| s == "-reconnect").unwrap();
        let i = a.iter().position(|s| s == "-i").unwrap();
        assert!(rc < i);
    }
}
