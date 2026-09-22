//! Wo liegen ffmpeg und yt-dlp?
//!
//! Eine zentrale Stelle, weil es mehrere Möglichkeiten gibt und die
//! Reihenfolge zählt. Wird `vidinsh` mit der Eigenschaft `bundled` gebaut,
//! stecken beide gepackt in der Programmdatei selbst und werden beim ersten
//! Start einmalig entpackt. Dann läuft das Programm auf einem Rechner, auf dem
//! nichts installiert ist.
//!
//! Der Unterschied zwischen beiden: ohne ffmpeg geht gar nichts, deshalb ist
//! es Pflicht. yt-dlp braucht nur, wer Portal-Links abspielt -- fehlt es,
//! funktioniert alles andere weiter.

use anyhow::{Result, bail};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

static FFMPEG: OnceLock<PathBuf> = OnceLock::new();
static YTDLP: OnceLock<Option<PathBuf>> = OnceLock::new();

/// Legt fest, welches ffmpeg benutzt wird. Einmal pro Programmlauf.
///
/// `explizit` kommt von `--ffmpeg` und schlägt alles andere. Danach das
/// mitgelieferte, zuletzt eins aus dem PATH.
pub fn init(explizit: Option<&Path>) -> Result<&'static Path> {
    if let Some(p) = FFMPEG.get() {
        return Ok(p.as_path());
    }

    let gewaehlt = if let Some(p) = explizit {
        if !p.is_file() {
            bail!("--ffmpeg zeigt auf {}, dort liegt nichts", p.display());
        }
        p.to_path_buf()
    } else if let Some(p) = bundled::ffmpeg()? {
        p
    } else if im_pfad("ffmpeg", "-version") {
        PathBuf::from("ffmpeg")
    } else {
        bail!(
            "ffmpeg wurde nicht gefunden.\n\
             Entweder ffmpeg in den PATH legen (Test: ffmpeg -version),\n\
             oder mit --ffmpeg <pfad> eine Programmdatei angeben.\n\
             Diese Fassung von vidinsh bringt kein ffmpeg mit."
        );
    };

    // Einmal beim Start den Zwischenspeicher durchsehen. Ein
    // Verzeichnis-Scan kostet nichts und hält ihn auf einer Fassung je
    // Werkzeug.
    bundled::pflegen();

    Ok(FFMPEG.get_or_init(|| gewaehlt).as_path())
}

/// Der festgelegte ffmpeg-Pfad.
///
/// Lief `init` noch nicht, wird auf `ffmpeg` aus dem PATH zurückgefallen,
/// statt zu paniken. Eine vergessene Initialisierung soll nicht das ganze
/// Programm umbringen -- bei der mitgelieferten Fassung ruft `main` `init`
/// ohnehin als Erstes auf, und nur dann greift das Entpackte.
pub fn ffmpeg() -> &'static Path {
    FFMPEG.get_or_init(|| PathBuf::from("ffmpeg")).as_path()
}

/// yt-dlp, sofern auffindbar. Reihenfolge: mitgeliefert, `tools/` neben der
/// Programmdatei, `tools/` im Arbeitsverzeichnis, PATH.
///
/// Global installiert wird bewusst nichts -- `tools/` neben der Programmdatei
/// ist der vorgesehene Ort für die portable Fassung.
pub fn ytdlp() -> Option<&'static Path> {
    YTDLP.get_or_init(ytdlp_suchen).as_deref()
}

fn ytdlp_suchen() -> Option<PathBuf> {
    if let Some(p) = bundled::ytdlp() {
        return Some(p);
    }

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

    kandidaten
        .into_iter()
        .find(|k| k.is_file())
        .or_else(|| im_pfad(name, "--version").then(|| PathBuf::from(name)))
}

/// `flagge` unterscheidet sich: ffmpeg kennt nur `-version`, yt-dlp nur
/// `--version`. Ein gemeinsames Flag gibt es nicht.
fn im_pfad(name: &str, flagge: &str) -> bool {
    std::process::Command::new(name)
        .arg(flagge)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Wohin die mitgelieferten Programme entpackt werden.
#[cfg_attr(not(feature = "bundled"), allow(dead_code))]
fn cache_dir() -> Option<PathBuf> {
    let basis = if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
    } else {
        std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
    }?;
    Some(basis.join("vidinsh"))
}

/// Für `--verbose` und `--probe`.
pub fn beschreibung() -> String {
    let mitgeliefert = cfg!(feature = "bundled");
    let ff = match FFMPEG.get() {
        Some(p) if mitgeliefert => format!("{} (mitgeliefert)", p.display()),
        Some(p) => p.display().to_string(),
        None => "noch nicht festgelegt".into(),
    };
    match ytdlp() {
        Some(p) => format!("ffmpeg: {ff}\nyt-dlp: {}", p.display()),
        None => format!("ffmpeg: {ff}\nyt-dlp: nicht vorhanden (nur für Portal-Links nötig)"),
    }
}

// --------------------------------------------------- mitgelieferte Programme

#[cfg(feature = "bundled")]
mod bundled {
    use super::cache_dir;
    use anyhow::{Context, Result};
    use std::path::PathBuf;

    /// Ein eingepacktes Programm.
    pub struct Werkzeug {
        pub gepackt: &'static [u8],
        /// Kennzeichen des Inhalts -- steckt im Dateinamen des Entpackten,
        /// damit zwei Fassungen von vidinsh sich nicht ins Gehege kommen.
        pub kennzeichen: &'static str,
        /// Größe im entpackten Zustand, zur Prüfung auf halbe Dateien.
        pub rohgroesse: u64,
    }

    // Von build.rs erzeugt: FFMPEG und YTDLP. (Kein Doc-Kommentar -- der
    // liesse sich an eine Makro-Einbindung nicht anheften.)
    include!(concat!(env!("OUT_DIR"), "/bundled.rs"));

    pub fn ffmpeg() -> Result<Option<PathBuf>> {
        entpacken("ffmpeg", &FFMPEG).map(Some)
    }

    /// yt-dlp ist freiwillig: fehlt es im Bau oder scheitert das Entpacken,
    /// läuft alles außer Portal-Links weiter.
    pub fn ytdlp() -> Option<PathBuf> {
        entpacken("yt-dlp", YTDLP.as_ref()?).ok()
    }

    /// Wie die entpackte Datei heißt. Das Kennzeichen des Inhalts steckt im
    /// Namen, damit zwei Fassungen von vidinsh sich nicht ins Gehege kommen.
    fn dateiname(name: &str, w: &Werkzeug) -> String {
        if cfg!(windows) {
            format!("{name}-{}.exe", w.kennzeichen)
        } else {
            format!("{name}-{}", w.kennzeichen)
        }
    }

    /// Entpackt einmalig und liefert den Pfad.
    fn entpacken(name: &str, w: &Werkzeug) -> Result<PathBuf> {
        let dir = cache_dir().context("Kein Ort für den Zwischenspeicher gefunden")?;
        let datei = dateiname(name, w);
        let ziel = dir.join(&datei);

        // Größe mitprüfen: ein abgebrochenes Entpacken hinterlässt sonst eine
        // halbe Datei, die bei jedem Start als fertig gilt.
        if std::fs::metadata(&ziel).is_ok_and(|m| m.len() == w.rohgroesse) {
            return Ok(ziel);
        }

        std::fs::create_dir_all(&dir)
            .with_context(|| format!("{} lässt sich nicht anlegen", dir.display()))?;

        // Erst neben das Ziel schreiben, dann umbenennen. Zwei gleichzeitig
        // gestartete vidinsh-Prozesse zerlegen sich sonst die Datei.
        let tmp = dir.join(format!("{datei}.{}.teil", std::process::id()));
        {
            let f = std::fs::File::create(&tmp)
                .with_context(|| format!("{} lässt sich nicht schreiben", tmp.display()))?;
            let mut aus = std::io::BufWriter::with_capacity(1 << 20, f);
            zstd::stream::copy_decode(w.gepackt, &mut aus)
                .with_context(|| format!("Mitgeliefertes {name} lässt sich nicht entpacken"))?;
            std::io::Write::flush(&mut aus)?;
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755));
        }

        // Ein anderer Prozess kann uns zuvorgekommen sein -- dann ist seine
        // Datei genauso gut wie unsere.
        if std::fs::rename(&tmp, &ziel).is_err() {
            let _ = std::fs::remove_file(&tmp);
            if !ziel.is_file() {
                anyhow::bail!("{} ließ sich nicht ablegen", ziel.display());
            }
        }

        Ok(ziel)
    }

    /// Ab diesem Alter gilt eine `.teil`-Datei als Leiche und nicht mehr als
    /// laufendes Entpacken. Das dauert gemessen knapp drei Sekunden; eine
    /// Stunde ist mit reichlich Abstand sicher.
    const TEIL_LEICHE: std::time::Duration = std::time::Duration::from_secs(3600);

    /// Vorsilben, die dieses Programm im Zwischenspeicher anlegt. Alles
    /// andere dort gehört jemand anderem und wird nicht angefasst.
    const VORSILBEN: [&str; 2] = ["ffmpeg-", "yt-dlp-"];

    /// Räumt den Zwischenspeicher auf. Wird einmal beim Start gerufen.
    ///
    /// Bewusst **nicht** im Entpacken: das läuft nur, wenn das Werkzeug auch
    /// gebraucht wird. yt-dlp wird bei einer lokalen Datei nie angefasst --
    /// eine alte 17-MB-Fassung wäre also nie verschwunden. Aufräumen ist eine
    /// Frage des Zwischenspeichers, nicht des Entpackens.
    pub fn pflegen() {
        let Some(dir) = cache_dir() else {
            return;
        };
        let mut behalten = vec![dateiname("ffmpeg", &FFMPEG)];
        if let Some(y) = YTDLP.as_ref() {
            behalten.push(dateiname("yt-dlp", y));
        }

        let Ok(eintraege) = std::fs::read_dir(&dir) else {
            return;
        };
        for e in eintraege.flatten() {
            let Ok(datei) = e.file_name().into_string() else {
                continue;
            };
            let alter = e
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.elapsed().ok());
            if ist_veraltet(&datei, &behalten, alter) {
                // Fehler schlucken: Aufräumen darf die Wiedergabe nicht
                // gefährden, etwa wenn ein zweiter vidinsh-Prozess die Datei
                // gerade benutzt (Windows verweigert das Löschen dann).
                let _ = std::fs::remove_file(e.path());
            }
        }
    }

    /// Darf diese Datei weg?
    ///
    /// Eine frische `.teil`-Datei gehört einem gleichzeitig laufenden Prozess,
    /// der gerade entpackt -- sie zu löschen hieße, ihm die Arbeit unter den
    /// Händen wegzuziehen. Eine *alte* dagegen ist die Leiche eines
    /// abgebrochenen Entpackens und belegt bis zu 231 MB, die sonst nie wieder
    /// jemand anfasst. Ohne diese Unterscheidung wäre das Aufräumen entweder
    /// gefährlich oder wirkungslos.
    fn ist_veraltet(datei: &str, behalten: &[String], alter: Option<std::time::Duration>) -> bool {
        if !VORSILBEN.iter().any(|v| datei.starts_with(v)) {
            return false;
        }
        if behalten.iter().any(|b| b == datei) {
            return false;
        }
        if datei.ends_with(".teil") {
            // Ohne lesbares Alter lieber stehen lassen.
            return alter.is_some_and(|a| a > TEIL_LEICHE);
        }
        true
    }

    #[cfg(test)]
    mod tests {
        use super::{TEIL_LEICHE, ist_veraltet};
        use std::time::Duration;

        const TEIL: &str = "ffmpeg-bbbb.exe.12345.teil";
        const FRISCH: Option<Duration> = Some(Duration::from_secs(2));
        const ALT: Option<Duration> = Some(Duration::from_secs(7200));

        fn behalten() -> Vec<String> {
            vec!["ffmpeg-aaaa.exe".into(), "yt-dlp-cccc.exe".into()]
        }

        #[test]
        fn alte_fassungen_werden_erkannt() {
            assert!(ist_veraltet("ffmpeg-bbbb.exe", &behalten(), ALT));
            assert!(
                ist_veraltet("yt-dlp-dddd.exe", &behalten(), ALT),
                "auch yt-dlp, nicht nur ffmpeg -- genau das fehlte"
            );
        }

        #[test]
        fn die_aktuellen_fassungen_bleiben() {
            for b in behalten() {
                assert!(!ist_veraltet(&b, &behalten(), ALT), "{b}");
            }
        }

        #[test]
        fn fremde_dateien_bleiben_unberuehrt() {
            assert!(!ist_veraltet("notizen.txt", &behalten(), ALT));
            assert!(
                !ist_veraltet("ffmpeg.exe", &behalten(), ALT),
                "ohne Bindestrich"
            );
            assert!(!ist_veraltet("irgendwas-aaaa.exe", &behalten(), ALT));
        }

        #[test]
        fn frische_halbfertige_dateien_bleiben() {
            // Die gehört einem Prozess, der gerade entpackt.
            assert!(!ist_veraltet(TEIL, &behalten(), FRISCH));
        }

        #[test]
        fn alte_halbfertige_dateien_werden_weggeraeumt() {
            // Leiche eines abgebrochenen Entpackens -- bis zu 231 MB.
            assert!(ist_veraltet(TEIL, &behalten(), ALT));
        }

        #[test]
        fn ohne_lesbares_alter_bleibt_die_halbfertige_datei() {
            assert!(!ist_veraltet(TEIL, &behalten(), None));
        }

        #[test]
        fn die_schwelle_liegt_weit_ueber_der_entpackdauer() {
            // Gemessen knapp 3 s; alles darunter darf nie als Leiche gelten.
            assert!(TEIL_LEICHE > Duration::from_secs(600));
        }
    }
}

#[cfg(not(feature = "bundled"))]
mod bundled {
    use anyhow::Result;
    use std::path::PathBuf;

    pub fn ffmpeg() -> Result<Option<PathBuf>> {
        Ok(None)
    }
    pub fn ytdlp() -> Option<PathBuf> {
        None
    }
    /// Ohne mitgelieferte Programme gibt es auch keinen Zwischenspeicher.
    pub fn pflegen() {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_liegt_unter_einem_eigenen_ordner() {
        let d = cache_dir().expect("eine Umgebung ohne HOME gibt es hier nicht");
        assert!(d.ends_with("vidinsh"), "{}", d.display());
    }

    #[test]
    fn ohne_init_wird_der_pfad_benutzt_statt_zu_paniken() {
        // Reihenfolge der Tests ist nicht garantiert; entscheidend ist nur,
        // dass ein Aufruf ohne vorheriges init nicht abstürzt.
        assert!(!ffmpeg().as_os_str().is_empty());
    }

    #[test]
    fn ffmpeg_ist_im_pfad_auffindbar() {
        // Auf der Entwicklungsmaschine muss das gelten; sonst laufen die
        // übrigen Tests ohnehin nicht.
        assert!(im_pfad("ffmpeg", "-version"), "ffmpeg kennt nur -version");
        assert!(
            !im_pfad("ffmpeg", "--version"),
            "sonst wäre die Unterscheidung überflüssig"
        );
        assert!(!im_pfad("ein-programm-das-es-nicht-gibt", "--version"));
    }

    #[test]
    fn ytdlp_wird_nicht_global_gesucht_bevor_im_projekt() {
        // Die Reihenfolge ist die Zusage: nichts global installieren.
        let Some(p) = ytdlp() else { return };
        let t = p.to_string_lossy().to_lowercase();
        assert!(
            t.contains("tools") || t.contains("vidinsh") || p.components().count() == 1,
            "unerwarteter Fundort: {}",
            p.display()
        );
    }

    #[test]
    fn beschreibung_nennt_beide_werkzeuge() {
        let b = beschreibung();
        assert!(b.contains("ffmpeg:"), "{b}");
        assert!(b.contains("yt-dlp:"), "{b}");
    }
}
