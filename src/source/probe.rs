//! ffprobe: Auflösung, Bildrate, Dauer, Audiospur.
//!
//! Ausgeben lassen wir uns `key=value`-Zeilen statt JSON -- das spart eine
//! Abhängigkeit und ist für ein halbes Dutzend Felder gut genug.

use anyhow::{Context, Result, bail};
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub struct MediaInfo {
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    /// `None` bei Live-Quellen
    pub duration: Option<f64>,
    pub has_audio: bool,
}

impl MediaInfo {
    pub fn aspect(&self) -> f64 {
        if self.height == 0 {
            16.0 / 9.0
        } else {
            self.width as f64 / self.height as f64
        }
    }

    /// Berücksichtigt eine getrennt danebenliegende Tonspur.
    ///
    /// `probe` befragt die *Bild*-Adresse. Liegt der Ton getrennt daneben --
    /// bei YouTube der Normalfall --, findet es dort naturgemäß keine
    /// Tonspur, und die Wiedergabe bliebe stumm. Eine zweite Adresse gibt es
    /// nur, weil der Formatselektor mit `ba` ausdrücklich eine Tonspur
    /// angefordert hat; sie ist also der verlässlichere Hinweis.
    pub fn mit_tonspur(mut self, getrennt: Option<&str>) -> Self {
        if getrennt.is_some() {
            self.has_audio = true;
        }
        self
    }

    /// Notnagel, wenn ffprobe nichts liefert -- etwa bei manchen Live-Quellen.
    pub fn fallback() -> Self {
        MediaInfo {
            width: 1280,
            height: 720,
            fps: 30.0,
            duration: None,
            has_audio: false,
        }
    }
}

/// Startet einen Prozess und bricht ihn ab, wenn er zu lange braucht.
/// Ohne das hängt das ganze Programm an einer nicht antwortenden Stream-URL.
pub fn run_limited(cmd: &mut Command, limit: Duration) -> Result<(bool, String)> {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("Prozess lässt sich nicht starten")?;

    let start = Instant::now();
    let status = loop {
        match child.try_wait()? {
            Some(s) => break Some(s),
            None if start.elapsed() >= limit => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    };

    // Nur lesen, wenn der Prozess von selbst zurückkam. Nach einem Abbruch
    // kann die Pipe noch von einem Enkelprozess offen gehalten werden -- dann
    // blockiert read_to_string unbegrenzt und das Zeitlimit wäre wertlos.
    let mut out = String::new();
    if status.is_some()
        && let Some(mut s) = child.stdout.take()
    {
        let _ = s.read_to_string(&mut out);
    }
    Ok((status.map(|s| s.success()).unwrap_or(false), out))
}

fn ffprobe_base(pre: &[String]) -> Command {
    let mut c = Command::new("ffprobe");
    c.args(["-v", "error", "-analyzeduration", "5M", "-probesize", "10M"]);
    c.args(pre);
    c
}

/// `pre` sind Argumente, die vor `-i` gehören (z. B. `-f dshow`).
pub fn probe(input: &str, pre: &[String], limit: Duration) -> Result<MediaInfo> {
    let mut c = ffprobe_base(pre);
    c.args([
        "-select_streams",
        "v:0",
        "-show_entries",
        "stream=width,height,r_frame_rate",
        "-show_entries",
        "format=duration",
        "-of",
        "default=noprint_wrappers=1",
        "-i",
        input,
    ]);
    let (ok, out) = run_limited(&mut c, limit)?;
    if !ok {
        bail!("ffprobe konnte die Quelle nicht lesen");
    }

    let mut info = MediaInfo::fallback();
    let mut sah_video = false;
    for line in out.lines() {
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let v = v.trim();
        match k.trim() {
            "width" => {
                if let Ok(n) = v.parse() {
                    info.width = n;
                    sah_video = true;
                }
            }
            "height" => {
                if let Ok(n) = v.parse() {
                    info.height = n;
                }
            }
            "r_frame_rate" => {
                if let Some(f) = parse_rate(v) {
                    info.fps = f;
                }
            }
            "duration" => info.duration = v.parse::<f64>().ok().filter(|d| *d > 0.0),
            _ => {}
        }
    }
    if !sah_video {
        bail!("Quelle enthält keine lesbare Videospur");
    }

    let mut c = ffprobe_base(pre);
    c.args([
        "-select_streams",
        "a:0",
        "-show_entries",
        "stream=codec_type",
        "-of",
        "default=noprint_wrappers=1:nokey=1",
        "-i",
        input,
    ]);
    if let Ok((true, a)) = run_limited(&mut c, limit) {
        info.has_audio = a.trim() == "audio";
    }

    Ok(info)
}

/// `r_frame_rate` kommt als Bruch, etwa `30/1` oder `30000/1001`.
fn parse_rate(s: &str) -> Option<f64> {
    let (n, d) = s.split_once('/')?;
    let n: f64 = n.trim().parse().ok()?;
    let d: f64 = d.trim().parse().ok()?;
    if d == 0.0 || n <= 0.0 {
        return None;
    }
    let f = n / d;
    if f.is_finite() && (0.1..=1000.0).contains(&f) {
        Some(f)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bildraten_werden_als_bruch_gelesen() {
        assert_eq!(parse_rate("30/1"), Some(30.0));
        assert!((parse_rate("30000/1001").unwrap() - 29.97).abs() < 0.01);
        assert_eq!(parse_rate("25/1"), Some(25.0));
    }

    #[test]
    fn unsinnige_bildraten_werden_abgelehnt() {
        assert_eq!(parse_rate("0/0"), None);
        assert_eq!(parse_rate("30"), None);
        assert_eq!(parse_rate("abc/1"), None);
        // ffprobe liefert 90000/1 für manche Container -- das ist keine Bildrate.
        assert_eq!(parse_rate("90000/1"), None);
    }

    #[test]
    fn getrennte_tonspur_setzt_has_audio() {
        // Der Fall YouTube: ffprobe sah auf der Bild-Adresse keinen Ton.
        let i = MediaInfo::fallback();
        assert!(!i.has_audio);
        assert!(
            i.mit_tonspur(Some("https://host/audio")).has_audio,
            "sonst bleibt die Wiedergabe stumm"
        );
    }

    #[test]
    fn ohne_getrennte_tonspur_bleibt_der_befund_stehen() {
        let stumm = MediaInfo::fallback();
        assert!(!stumm.clone().mit_tonspur(None).has_audio);

        let mut mit = MediaInfo::fallback();
        mit.has_audio = true;
        assert!(mit.mit_tonspur(None).has_audio, "darf nichts wegnehmen");
    }

    #[test]
    fn seitenverhaeltnis_ueberlebt_hoehe_null() {
        let mut i = MediaInfo::fallback();
        i.height = 0;
        assert!(i.aspect().is_finite() && i.aspect() > 0.0);
    }

    #[test]
    fn zeitlimit_beendet_haengende_prozesse() {
        // Ein Prozess, der nie von selbst zurückkommt.
        let mut c = if cfg!(windows) {
            let mut c = Command::new("cmd");
            c.args(["/c", "ping -n 30 127.0.0.1 >NUL"]);
            c
        } else {
            let mut c = Command::new("sleep");
            c.arg("30");
            c
        };
        let start = Instant::now();
        let (ok, _) = run_limited(&mut c, Duration::from_millis(300)).unwrap();
        assert!(!ok, "abgebrochener Prozess darf nicht als Erfolg gelten");
        assert!(start.elapsed() < Duration::from_secs(5));
    }
}
