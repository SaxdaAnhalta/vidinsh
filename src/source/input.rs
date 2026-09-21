//! Erkennt, was für eine Quelle da vorne angegeben wurde, und baut daraus die
//! Argumente, die ffmpeg vor `-i` braucht.
//!
//! HLS, DASH, RTSP, RTMP, SRT und UDP kann ffmpeg selbst -- die URL wandert
//! unverändert hinein. Nur Portale wie YouTube brauchen yt-dlp, das die
//! eigentliche Medien-URL auflöst.

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    File,
    /// direkt von ffmpeg lesbare URL
    Url,
    /// muss erst über yt-dlp aufgelöst werden
    Portal,
    Camera,
    Stdin,
}

#[derive(Clone, Debug)]
pub struct Input {
    pub kind: Kind,
    /// was am Ende hinter `-i` steht (das Bild)
    pub ffmpeg_input: String,
    /// getrennte Tonspur, falls die Quelle Bild und Ton nicht gemuxt liefert
    /// -- bei YouTube ist das der Normalfall
    pub audio_input: Option<String>,
    /// Argumente vor `-i` (Demuxer, Reconnect, Zeitlimits)
    pub pre_args: Vec<String>,
    /// laufende Quelle ohne festes Ende -- kein Spulen, kein Fortschritt
    pub is_live: bool,
    /// Anzeigename für die Statuszeile
    pub label: String,
}

const LIVE_SCHEMES: [&str; 6] = ["rtsp", "rtmp", "rtmps", "srt", "udp", "rtp"];

/// Portale, bei denen die URL keine Mediendatei ist. Die Liste ist bewusst
/// kurz: alles andere wird erst direkt versucht und fällt nur bei Bedarf auf
/// yt-dlp zurück.
const PORTALS: [&str; 8] = [
    "youtube.com",
    "youtu.be",
    "vimeo.com",
    "twitch.tv",
    "dailymotion.com",
    "twitter.com",
    "x.com",
    "tiktok.com",
];

pub fn classify(raw: &str) -> Input {
    if raw == "-" {
        return Input {
            kind: Kind::Stdin,
            ffmpeg_input: "pipe:0".into(),
            audio_input: None,
            pre_args: vec![],
            is_live: true,
            label: "stdin".into(),
        };
    }

    if let Some(rest) = raw.strip_prefix("cam:") {
        return camera(rest);
    }

    if let Some((scheme, rest)) = raw.split_once("://") {
        let scheme = scheme.to_ascii_lowercase();
        let host = rest
            .split(['/', '?', '#'])
            .next()
            .unwrap_or("")
            .trim_start_matches("www.")
            .to_ascii_lowercase();

        if LIVE_SCHEMES.contains(&scheme.as_str()) {
            return Input {
                kind: Kind::Url,
                ffmpeg_input: raw.into(),
                audio_input: None,
                pre_args: net_args(true),
                is_live: true,
                label: host,
            };
        }

        if scheme == "http" || scheme == "https" {
            let portal = PORTALS
                .iter()
                .any(|p| host == *p || host.ends_with(&format!(".{p}")));
            let pfad = rest.split(['?', '#']).next().unwrap_or("");
            let streaming = pfad.ends_with(".m3u8") || pfad.ends_with(".mpd");
            return Input {
                kind: if portal { Kind::Portal } else { Kind::Url },
                ffmpeg_input: raw.into(),
                audio_input: None,
                pre_args: net_args(streaming),
                is_live: streaming,
                label: host,
            };
        }

        // file://, data:// und was ffmpeg sonst noch kennt.
        return Input {
            kind: Kind::Url,
            ffmpeg_input: raw.into(),
            audio_input: None,
            pre_args: vec![],
            is_live: false,
            label: raw.into(),
        };
    }

    let p = PathBuf::from(raw);
    let label = p
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| raw.to_string());
    Input {
        kind: Kind::File,
        ffmpeg_input: raw.into(),
        audio_input: None,
        pre_args: vec![],
        is_live: false,
        label,
    }
}

/// Netzwerkoptionen. Ohne Wiederverbinden endet jeder längere Stream beim
/// ersten Aussetzer, und ohne Zeitlimit hängt ffmpeg still vor sich hin.
fn net_args(streamed: bool) -> Vec<String> {
    let mut a = vec![
        "-reconnect".into(),
        "1".into(),
        "-reconnect_delay_max".into(),
        "5".into(),
        "-rw_timeout".into(),
        "10000000".into(),
    ];
    if streamed {
        a.push("-reconnect_streamed".into());
        a.push("1".into());
    }
    a
}

fn camera(spec: &str) -> Input {
    let spec = spec.trim();
    #[cfg(windows)]
    {
        // dshow spricht Geräte über den Namen an; eine Zahl heißt "der n-te
        // Eintrag aus --list-devices" und wird vom Aufrufer aufgelöst.
        Input {
            kind: Kind::Camera,
            ffmpeg_input: format!("video={spec}"),
            audio_input: None,
            pre_args: vec!["-f".into(), "dshow".into()],
            is_live: true,
            label: format!("Kamera {spec}"),
        }
    }
    #[cfg(target_os = "macos")]
    {
        Input {
            kind: Kind::Camera,
            ffmpeg_input: spec.to_string(),
            audio_input: None,
            pre_args: vec!["-f".into(), "avfoundation".into()],
            is_live: true,
            label: format!("Kamera {spec}"),
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let dev = if spec.starts_with('/') {
            spec.to_string()
        } else {
            format!("/dev/video{spec}")
        };
        Input {
            kind: Kind::Camera,
            ffmpeg_input: dev,
            audio_input: None,
            pre_args: vec!["-f".into(), "v4l2".into()],
            is_live: true,
            label: format!("Kamera {spec}"),
        }
    }
}

// ------------------------------------------------------------------- yt-dlp

/// Sucht yt-dlp erst neben dem Programm, dann im Projektordner, dann im PATH.
/// Global installiert wird bewusst nichts.
pub fn find_ytdlp() -> Option<PathBuf> {
    let name = if cfg!(windows) {
        "yt-dlp.exe"
    } else {
        "yt-dlp"
    };

    let mut kandidaten: Vec<PathBuf> = Vec::new();
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        kandidaten.push(dir.join("tools").join(name));
        // cargo run legt die exe unter target/debug ab
        if let Some(up) = dir.parent().and_then(Path::parent) {
            kandidaten.push(up.join("tools").join(name));
        }
    }
    kandidaten.push(PathBuf::from("tools").join(name));

    for k in kandidaten {
        if k.is_file() {
            return Some(k);
        }
    }

    // Im PATH als letzte Möglichkeit -- falls es doch jemand global hat.
    let probe = Command::new(name).arg("--version").output().ok()?;
    probe.status.success().then(|| PathBuf::from(name))
}

/// Bild- und optionale Tonspur, wie yt-dlp sie ausgibt.
pub struct Aufgeloest {
    pub video: String,
    pub audio: Option<String>,
}

/// Löst eine Portal-URL in direkt abspielbare Medien-URLs auf.
///
/// Portale liefern Bild und Ton oft **getrennt** -- bei YouTube ist das seit
/// Jahren der Normalfall, weil die gemuxten Formate nur noch in niedriger
/// Auflösung existieren (und ohne JavaScript-Runtime teils gar nicht mehr
/// auftauchen). Der Formatselektor nimmt deshalb eine gemuxte Spur, wenn es
/// sie gibt, und sonst die beste Kombination aus getrenntem Bild und Ton.
///
/// Bei `-f A+B` gibt yt-dlp die Adressen in der Reihenfolge des Selektors
/// aus: erst Bild, dann Ton.
pub fn resolve_portal(url: &str, max_height: u32, limit: Duration) -> Result<Aufgeloest> {
    let exe = find_ytdlp().context(
        "Für diese URL wird yt-dlp gebraucht, das hier nicht gefunden wurde.\n\
         Erwartet wird es als tools/yt-dlp.exe im Projektordner (portabel, \
         keine globale Installation).\n\
         Direkte Datei-, HLS-, DASH-, RTSP- und RTMP-URLs funktionieren ohne.",
    )?;

    let mut c = Command::new(exe);
    c.args([
        "-f",
        &format!("b[height<={max_height}]/bv[height<={max_height}]+ba/b"),
        "--no-playlist",
        "-g",
        url,
    ]);
    let lauf = super::probe::run_limited(&mut c, limit)?;
    let mut urls = lauf
        .stdout
        .lines()
        .filter(|l| l.starts_with("http"))
        .map(str::to_string);

    let Some(video) = urls.next().filter(|_| lauf.erfolg) else {
        bail!("yt-dlp konnte aus dieser URL keine abspielbare Adresse gewinnen");
    };
    Ok(Aufgeloest {
        audio: urls.next(),
        video,
    })
}

/// Die Videogeräte des Systems, in der Reihenfolge, die `cam:<n>` meint.
pub fn video_devices() -> Result<Vec<String>> {
    let roh = list_devices_raw()?;
    let mut out = Vec::new();

    for zeile in roh.lines() {
        #[cfg(windows)]
        {
            // dshow meldet:  [in#0 @ ...] "Gerätename" (video)
            // Die Zeilen mit "Alternative name" gehören zum Eintrag davor.
            if !zeile.contains("(video)") || zeile.contains("Alternative name") {
                continue;
            }
            if let (Some(a), Some(b)) = (zeile.find('"'), zeile.rfind('"'))
                && b > a + 1
            {
                out.push(zeile[a + 1..b].to_string());
            }
        }
        #[cfg(target_os = "macos")]
        {
            // avfoundation meldet:  [AVFoundation ...] [0] FaceTime HD Camera
            if let Some(p) = zeile.find("] [") {
                if let Some(q) = zeile[p + 3..].find("] ") {
                    out.push(zeile[p + 3 + q + 2..].trim().to_string());
                }
            }
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            let z = zeile.trim();
            if z.starts_with("/dev/video") {
                out.push(z.to_string());
            }
        }
    }
    Ok(out)
}

/// Löst `cam:0` in einen Gerätenamen auf. Ein Name wird unverändert
/// durchgereicht -- nur reine Zahlen sind ein Index in die Geräteliste.
pub fn resolve_camera(input: &mut Input) -> Result<()> {
    if input.kind != Kind::Camera {
        return Ok(());
    }
    let spec = input
        .ffmpeg_input
        .strip_prefix("video=")
        .unwrap_or(&input.ffmpeg_input)
        .to_string();

    let Ok(idx) = spec.parse::<usize>() else {
        return Ok(()); // schon ein Name oder ein Gerätepfad
    };

    let geraete = video_devices()?;
    let name = geraete.get(idx).ok_or_else(|| {
        let liste = if geraete.is_empty() {
            "  (keine gefunden)".to_string()
        } else {
            geraete
                .iter()
                .enumerate()
                .map(|(i, g)| format!("  cam:{i}  {g}"))
                .collect::<Vec<_>>()
                .join("\n")
        };
        anyhow::anyhow!("Es gibt keine Kamera mit Nummer {idx}. Verfügbar:\n{liste}")
    })?;

    input.label = format!("Kamera: {name}");
    #[cfg(windows)]
    {
        input.ffmpeg_input = format!("video={name}");
    }
    #[cfg(not(windows))]
    {
        input.ffmpeg_input = name.clone();
    }
    Ok(())
}

/// Kameras auflisten. ffmpeg schreibt die Liste als Fehlermeldung auf stderr
/// und endet mit einem Fehlercode -- das ist so vorgesehen.
fn list_devices_raw() -> Result<String> {
    // Unter Linux gibt es keinen Geräte-Auflister in ffmpeg; dort sind die
    // Kameras schlicht Dateien.
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut cmd = {
        let mut c = Command::new("sh");
        c.args(["-c", "ls -1 /dev/video* 2>/dev/null"]);
        c
    };

    #[cfg(not(all(unix, not(target_os = "macos"))))]
    let mut cmd = {
        let mut c = Command::new(super::tools::ffmpeg());
        c.args(["-hide_banner", "-nostdin"]);
        #[cfg(windows)]
        c.args(["-f", "dshow", "-list_devices", "true", "-i", "dummy"]);
        #[cfg(target_os = "macos")]
        c.args(["-f", "avfoundation", "-list_devices", "true", "-i", ""]);
        c
    };

    let lauf = super::probe::run_limited(&mut cmd, Duration::from_secs(15))
        .context("Geräteliste lässt sich nicht abrufen")?;
    Ok(format!("{}{}", lauf.stderr, lauf.stdout))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dateien_werden_als_datei_erkannt() {
        let i = classify("testdata/test.mp4");
        assert_eq!(i.kind, Kind::File);
        assert!(!i.is_live);
        assert_eq!(i.label, "test.mp4");
        assert!(i.pre_args.is_empty());
    }

    #[test]
    fn windows_pfade_werden_nicht_fuer_urls_gehalten() {
        // "C:\..." enthält einen Doppelpunkt, aber kein "://".
        let i = classify(r"C:\projects\vidinsh\testdata\test.mp4");
        assert_eq!(i.kind, Kind::File);
    }

    #[test]
    fn hls_und_dash_gelten_als_live() {
        for u in [
            "https://example.com/stream/master.m3u8",
            "https://example.com/x.mpd?token=1",
        ] {
            let i = classify(u);
            assert_eq!(i.kind, Kind::Url, "{u}");
            assert!(i.is_live, "{u}");
            assert!(i.pre_args.iter().any(|a| a == "-reconnect_streamed"));
        }
    }

    #[test]
    fn rtsp_und_verwandte_gehen_direkt_an_ffmpeg() {
        for u in [
            "rtsp://cam.local/live",
            "srt://host:9000",
            "udp://239.0.0.1:1234",
        ] {
            let i = classify(u);
            assert_eq!(i.kind, Kind::Url, "{u}");
            assert!(i.is_live, "{u}");
        }
    }

    #[test]
    fn portale_werden_fuer_ytdlp_markiert() {
        for u in [
            "https://www.youtube.com/watch?v=abc",
            "https://youtu.be/abc",
            "https://m.youtube.com/watch?v=abc",
            "https://twitch.tv/someone",
        ] {
            assert_eq!(classify(u).kind, Kind::Portal, "{u}");
        }
    }

    #[test]
    fn normale_http_datei_braucht_kein_ytdlp() {
        let i = classify("https://example.com/video.mp4");
        assert_eq!(i.kind, Kind::Url);
        assert!(!i.is_live);
    }

    #[test]
    fn stdin_und_kamera_werden_erkannt() {
        assert_eq!(classify("-").kind, Kind::Stdin);
        let c = classify("cam:0");
        assert_eq!(c.kind, Kind::Camera);
        assert!(c.is_live);
        assert!(!c.pre_args.is_empty(), "Kamera braucht einen Demuxer");
    }

    #[test]
    fn kameraname_wird_unveraendert_durchgereicht() {
        let mut i = classify("cam:Logitech HD");
        resolve_camera(&mut i).unwrap();
        assert!(
            i.ffmpeg_input.contains("Logitech HD"),
            "ein Name darf nicht als Index gelesen werden: {}",
            i.ffmpeg_input
        );
    }

    #[test]
    fn nicht_kamera_quellen_bleiben_unberuehrt() {
        let mut i = classify("testdata/test.mp4");
        let vorher = i.ffmpeg_input.clone();
        resolve_camera(&mut i).unwrap();
        assert_eq!(i.ffmpeg_input, vorher);
    }

    #[test]
    fn unsinnige_kameranummer_nennt_die_verfuegbaren() {
        let _ = super::super::tools::init(None);
        let mut i = classify("cam:99");
        // Auf einem System mit 100 Kameras wäre das kein Fehler -- deshalb
        // wird nur der Fehlerfall geprüft.
        if let Err(e) = resolve_camera(&mut i) {
            let t = format!("{e:#}");
            assert!(t.contains("99"), "{t}");
            assert!(t.contains("Verfügbar"), "{t}");
        }
    }

    #[test]
    fn gewoehnliche_quellen_haben_keine_getrennte_tonspur() {
        for q in ["film.mp4", "https://host/x.m3u8", "rtsp://host/live", "-"] {
            assert!(classify(q).audio_input.is_none(), "{q}");
        }
    }

    #[test]
    fn yt_dlp_wird_im_projektordner_gesucht_bevor_im_pfad() {
        // Die Reihenfolge ist die Zusage: nichts global installieren.
        let Some(p) = find_ytdlp() else { return };
        let t = p.to_string_lossy().to_lowercase();
        assert!(
            t.contains("tools") || p.components().count() == 1,
            "unerwarteter Fundort: {}",
            p.display()
        );
    }

    #[test]
    fn host_wird_ohne_www_beschriftet() {
        assert_eq!(
            classify("https://www.example.com/a/b.mp4").label,
            "example.com"
        );
    }
}
