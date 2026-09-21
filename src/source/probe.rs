//! Auflösung, Bildrate, Dauer und Tonspur -- aus ffmpegs eigener Ausgabe.
//!
//! Früher stand hier ffprobe. Das ist bequemer zu lesen, kostet aber eine
//! zweite Programmdatei von gut 230 MB -- und die soll mitgeliefert werden
//! können. `ffmpeg -i <quelle>` schreibt dieselben Angaben auf stderr und
//! bricht danach ab, weil keine Ausgabedatei genannt ist. Genau das nutzen
//! wir: der Fehlercode ist erwartet, entscheidend ist der Text.

use super::tools;
use anyhow::{Context, Result};
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq)]
pub struct MediaInfo {
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    /// `None` bei Live-Quellen und Einzelbildern
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

    /// Notnagel, wenn ffmpeg nichts Verwertbares liefert -- etwa bei manchen
    /// Live-Quellen, die erst beim Abspielen preisgeben, was sie sind.
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

#[derive(Debug, Default)]
pub struct Lauf {
    pub erfolg: bool,
    pub stdout: String,
    pub stderr: String,
}

/// Startet einen Prozess und bricht ihn ab, wenn er zu lange braucht.
/// Ohne das hängt das ganze Programm an einer nicht antwortenden Stream-URL.
///
/// Beide Ausgabeströme werden in eigenen Threads gelesen. Nur einen zu lesen
/// wäre ein Verklemmungsrisiko: läuft der andere Puffer voll, blockiert der
/// Kindprozess beim Schreiben und kommt nie zum Ende.
pub fn run_limited(cmd: &mut Command, limit: Duration) -> Result<Lauf> {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("Prozess lässt sich nicht starten")?;

    fn lesen(quelle: Option<impl Read + Send + 'static>) -> mpsc::Receiver<String> {
        let (tx, rx) = mpsc::channel();
        if let Some(mut s) = quelle {
            std::thread::spawn(move || {
                let mut b = Vec::new();
                let _ = s.read_to_end(&mut b);
                let _ = tx.send(String::from_utf8_lossy(&b).into_owned());
            });
        }
        rx
    }
    let rx_out = lesen(child.stdout.take());
    let rx_err = lesen(child.stderr.take());

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

    // Kurz auf die Lese-Threads warten, aber nicht auf sie bauen: hält ein
    // Enkelprozess die Pipe offen, kommen sie nie zurück, und das Zeitlimit
    // wäre wertlos.
    let frist = Duration::from_millis(500);
    Ok(Lauf {
        erfolg: status.map(|s| s.success()).unwrap_or(false),
        stdout: rx_out.recv_timeout(frist).unwrap_or_default(),
        stderr: rx_err.recv_timeout(frist).unwrap_or_default(),
    })
}

/// `pre` sind Argumente, die vor `-i` gehören (z. B. `-f dshow`).
pub fn probe(input: &str, pre: &[String], limit: Duration) -> Result<MediaInfo> {
    let mut c = Command::new(tools::ffmpeg());
    c.args(["-hide_banner", "-nostdin"]);
    c.args(pre);
    c.args(["-i", input]);

    // Der Fehlercode ist hier erwartet ("At least one output file must be
    // specified") -- geprüft wird deshalb der Text, nicht der Rückgabewert.
    let lauf = run_limited(&mut c, limit)?;
    parse_info(&lauf.stderr).ok_or_else(|| {
        let letzte = lauf
            .stderr
            .lines()
            .rfind(|l| !l.trim().is_empty())
            .unwrap_or("(ffmpeg hat nichts gemeldet)");
        anyhow::anyhow!("Quelle enthält keine lesbare Videospur.\n{letzte}")
    })
}

// ------------------------------------------------------------------- Parser

/// Liest die Angaben aus dem, was `ffmpeg -i` auf stderr schreibt.
pub fn parse_info(stderr: &str) -> Option<MediaInfo> {
    let mut info = MediaInfo {
        width: 0,
        height: 0,
        fps: 0.0,
        duration: None,
        has_audio: false,
    };
    let mut sah_video = false;

    for zeile in stderr.lines() {
        let z = zeile.trim();

        if let Some(rest) = z.strip_prefix("Duration:") {
            let wert = rest.split(',').next().unwrap_or("").trim();
            info.duration = parse_hms(wert).filter(|d| *d > 0.0);
            continue;
        }

        // "Stream #0:0[0x1](und): Video: h264 (High) ..., 1280x720 [SAR ...]"
        if !z.starts_with("Stream #") {
            continue;
        }
        if z.contains(": Audio:") {
            info.has_audio = true;
        } else if z.contains(": Video:") && !sah_video {
            // Nur die erste Videospur -- weitere sind Vorschaubilder o. ä.
            if let Some((w, h)) = find_resolution(z) {
                info.width = w;
                info.height = h;
                info.fps = find_fps(z).unwrap_or(25.0);
                sah_video = true;
            }
        }
    }

    sah_video.then_some(info)
}

/// Sucht `BREITExHOEHE`. Zahlen wie `0x31637661` (FourCC) und `yuv420p`
/// dürfen dabei nicht durchrutschen, deshalb die Plausibilitätsgrenzen.
fn find_resolution(s: &str) -> Option<(u32, u32)> {
    for teil in s.split([',', ' ', '(', ')', '[', ']']) {
        let Some((w, h)) = teil.split_once('x') else {
            continue;
        };
        let (Ok(w), Ok(h)) = (w.parse::<u32>(), h.parse::<u32>()) else {
            continue;
        };
        if (16..=16384).contains(&w) && (16..=16384).contains(&h) {
            return Some((w, h));
        }
    }
    None
}

/// `fps` ist die gemeldete Bildrate, `tbr` die aus dem Container geschätzte.
/// Erstere ist verlässlicher, letztere der Ersatz.
fn find_fps(s: &str) -> Option<f64> {
    let mut fps = None;
    let mut tbr = None;
    for teil in s.split(',') {
        let t = teil.trim();
        if let Some(v) = t.strip_suffix(" fps") {
            fps = v.trim().parse::<f64>().ok();
        } else if let Some(v) = t.strip_suffix(" tbr") {
            tbr = v.trim().parse::<f64>().ok();
        }
    }
    fps.or(tbr).filter(|f| (0.1..=1000.0).contains(f))
}

/// `00:01:02.50` -> Sekunden. `N/A` und alles Unverständliche ergibt `None`.
fn parse_hms(s: &str) -> Option<f64> {
    let t: Vec<&str> = s.split(':').collect();
    if t.len() != 3 {
        return None;
    }
    let h: f64 = t[0].trim().parse().ok()?;
    let m: f64 = t[1].trim().parse().ok()?;
    let sek: f64 = t[2].trim().parse().ok()?;
    let ganz = h * 3600.0 + m * 60.0 + sek;
    ganz.is_finite().then_some(ganz)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Echte Ausgabe von ffmpeg 8.1.2, gekürzt auf die tragenden Zeilen.
    const MIT_TON: &str = r#"
Input #0, mov,mp4,m4a,3gp,3g2,mj2, from 'testdata/av.mp4':
  Metadata:
    encoder         : Lavf62.6.100
  Duration: 00:00:10.00, start: 0.000000, bitrate: 3196 kb/s
  Stream #0:0[0x1](und): Video: h264 (High) (avc1 / 0x31637661), yuv420p(progressive), 1280x720 [SAR 1:1 DAR 16:9], 3118 kb/s, 30 fps, 30 tbr, 15360 tbn (default)
  Stream #0:1[0x2](und): Audio: aac (LC) (mp4a / 0x6134706D), 44100 Hz, mono, fltp, 69 kb/s (default)
At least one output file must be specified
"#;

    const OHNE_TON: &str = r#"
  Duration: 00:00:10.00, start: 0.000000, bitrate: 3121 kb/s
  Stream #0:0[0x1](und): Video: h264 (High) (avc1 / 0x31637661), yuv420p(progressive), 1280x720 [SAR 1:1 DAR 16:9], 3118 kb/s, 30 fps, 30 tbr, 15360 tbn (default)
"#;

    const EINZELBILD: &str = r#"
  Duration: N/A, bitrate: N/A
  Stream #0:0: Video: png, rgb24(pc, gbr/unknown/unknown), 640x480 [SAR 1:1 DAR 4:3], 25 fps, 25 tbr, 25 tbn
"#;

    #[test]
    fn liest_video_mit_ton() {
        let i = parse_info(MIT_TON).expect("muss erkannt werden");
        assert_eq!((i.width, i.height), (1280, 720));
        assert_eq!(i.fps, 30.0);
        assert_eq!(i.duration, Some(10.0));
        assert!(i.has_audio);
    }

    #[test]
    fn erkennt_fehlende_tonspur() {
        assert!(!parse_info(OHNE_TON).unwrap().has_audio);
    }

    #[test]
    fn einzelbild_hat_keine_dauer() {
        let i = parse_info(EINZELBILD).expect("auch ein Bild ist eine Videospur");
        assert_eq!((i.width, i.height), (640, 480));
        assert_eq!(i.duration, None, "'N/A' ist keine Dauer");
        assert_eq!(i.fps, 25.0);
    }

    #[test]
    fn fourcc_und_pixelformat_werden_nicht_fuer_die_aufloesung_gehalten() {
        let i = parse_info(MIT_TON).unwrap();
        assert_eq!((i.width, i.height), (1280, 720));
        assert_eq!(find_resolution("(avc1 / 0x31637661), yuv420p"), None);
    }

    #[test]
    fn tbr_springt_ein_wenn_fps_fehlt() {
        let s = "  Stream #0:0: Video: h264, yuv420p, 640x480, 24 tbr, 12288 tbn";
        assert_eq!(parse_info(s).unwrap().fps, 24.0);
    }

    #[test]
    fn fps_schlaegt_tbr() {
        let s = "  Stream #0:0: Video: h264, 640x480, 30 fps, 60 tbr";
        assert_eq!(parse_info(s).unwrap().fps, 30.0);
    }

    #[test]
    fn unbrauchbare_bildrate_faellt_auf_den_standard() {
        // tbn ist die Zeitbasis, keine Bildrate -- die lesen wir gar nicht erst.
        let s = "  Stream #0:0: Video: h264, 640x480, 90000 tbn";
        assert_eq!(parse_info(s).unwrap().fps, 25.0);
    }

    #[test]
    fn ohne_videospur_kein_ergebnis() {
        let nur_ton = "  Stream #0:0: Audio: aac, 44100 Hz, stereo";
        assert!(parse_info(nur_ton).is_none());
        assert!(parse_info("").is_none());
        assert!(parse_info("gibtsnicht.mp4: No such file or directory").is_none());
    }

    #[test]
    fn nur_die_erste_videospur_zaehlt() {
        let zwei = "  Stream #0:0: Video: h264, 1920x1080, 30 fps\n  \
                    Stream #0:1: Video: mjpeg, 320x240, 15 fps";
        let i = parse_info(zwei).unwrap();
        assert_eq!((i.width, i.height), (1920, 1080), "Vorschaubild ignorieren");
    }

    #[test]
    fn zeitangaben() {
        assert_eq!(parse_hms("00:00:10.00"), Some(10.0));
        assert_eq!(parse_hms("01:02:03.50"), Some(3723.5));
        assert_eq!(parse_hms("N/A"), None);
        assert_eq!(parse_hms("10"), None);
    }

    #[test]
    fn seitenverhaeltnis_ueberlebt_hoehe_null() {
        let mut i = MediaInfo::fallback();
        i.height = 0;
        assert!(i.aspect().is_finite() && i.aspect() > 0.0);
    }

    #[test]
    fn getrennte_tonspur_setzt_has_audio() {
        let i = MediaInfo::fallback();
        assert!(!i.has_audio);
        assert!(i.mit_tonspur(Some("https://host/audio")).has_audio);
    }

    #[test]
    fn ohne_getrennte_tonspur_bleibt_der_befund_stehen() {
        assert!(!MediaInfo::fallback().mit_tonspur(None).has_audio);
        let mut mit = MediaInfo::fallback();
        mit.has_audio = true;
        assert!(mit.mit_tonspur(None).has_audio, "darf nichts wegnehmen");
    }

    #[test]
    fn zeitlimit_beendet_haengende_prozesse() {
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
        let l = run_limited(&mut c, Duration::from_millis(300)).unwrap();
        assert!(
            !l.erfolg,
            "abgebrochener Prozess darf nicht als Erfolg gelten"
        );
        assert!(start.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn beide_ausgabestroeme_werden_eingesammelt() {
        let mut c = if cfg!(windows) {
            let mut c = Command::new("cmd");
            c.args(["/c", "echo hierhin & echo dorthin 1>&2"]);
            c
        } else {
            let mut c = Command::new("sh");
            c.args(["-c", "echo hierhin; echo dorthin 1>&2"]);
            c
        };
        let l = run_limited(&mut c, Duration::from_secs(10)).unwrap();
        assert!(l.stdout.contains("hierhin"), "stdout: {:?}", l.stdout);
        assert!(l.stderr.contains("dorthin"), "stderr: {:?}", l.stderr);
    }
}
