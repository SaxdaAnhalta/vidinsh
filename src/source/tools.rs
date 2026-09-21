//! Wo liegt ffmpeg?
//!
//! Eine zentrale Stelle, weil es drei Möglichkeiten gibt und die Reihenfolge
//! zählt. Wird `vidinsh` mit der Eigenschaft `bundled` gebaut, steckt ein
//! gepacktes ffmpeg in der Programmdatei selbst; es wird beim ersten Start
//! einmalig entpackt. Dann läuft das Programm auf einem Rechner, auf dem
//! nichts installiert ist.

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

static PFAD: OnceLock<PathBuf> = OnceLock::new();

/// Legt fest, welches ffmpeg benutzt wird. Einmal pro Programmlauf.
///
/// `explizit` kommt von `--ffmpeg` und schlägt alles andere. Danach das
/// mitgelieferte, zuletzt eins aus dem PATH.
pub fn init(explizit: Option<&Path>) -> Result<&'static Path> {
    if let Some(p) = PFAD.get() {
        return Ok(p.as_path());
    }

    let gewaehlt = if let Some(p) = explizit {
        if !p.is_file() {
            bail!("--ffmpeg zeigt auf {}, dort liegt nichts", p.display());
        }
        p.to_path_buf()
    } else if let Some(p) = bundled::entpacken()? {
        p
    } else if im_pfad("ffmpeg") {
        PathBuf::from("ffmpeg")
    } else {
        bail!(
            "ffmpeg wurde nicht gefunden.\n\
             Entweder ffmpeg in den PATH legen (Test: ffmpeg -version),\n\
             oder mit --ffmpeg <pfad> eine Programmdatei angeben.\n\
             Diese Fassung von vidinsh bringt kein ffmpeg mit."
        );
    };

    Ok(PFAD.get_or_init(|| gewaehlt).as_path())
}

/// Der festgelegte Pfad.
///
/// Lief `init` noch nicht, wird auf `ffmpeg` aus dem PATH zurückgefallen,
/// statt zu paniken. Eine vergessene Initialisierung soll nicht das ganze
/// Programm umbringen -- bei der mitgelieferten Fassung ruft `main` `init`
/// ohnehin als Erstes auf, und nur dann greift das Entpackte.
pub fn ffmpeg() -> &'static Path {
    PFAD.get_or_init(|| PathBuf::from("ffmpeg")).as_path()
}

fn im_pfad(name: &str) -> bool {
    std::process::Command::new(name)
        .arg("-version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Wohin das mitgelieferte ffmpeg entpackt wird.
#[cfg_attr(not(feature = "bundled"), allow(dead_code))]
fn cache_dir() -> Result<PathBuf> {
    let basis = if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
    } else {
        std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
    }
    .context("Kein Ort für den Zwischenspeicher gefunden (LOCALAPPDATA bzw. HOME)")?;
    Ok(basis.join("vidinsh"))
}

// ------------------------------------------------------- mitgeliefertes ffmpeg

#[cfg(feature = "bundled")]
mod bundled {
    use super::cache_dir;
    use anyhow::{Context, Result};
    use std::path::PathBuf;

    /// Von build.rs erzeugt: das gepackte ffmpeg und sein Kennzeichen.
    include!(concat!(env!("OUT_DIR"), "/bundled.rs"));

    /// Entpackt einmalig und liefert den Pfad.
    ///
    /// Der Dateiname trägt das Kennzeichen des Inhalts. Dadurch holt eine neue
    /// Fassung von vidinsh automatisch ihr eigenes ffmpeg heraus, statt ein
    /// altes weiterzubenutzen -- und mehrere Fassungen stören sich nicht.
    pub fn entpacken() -> Result<Option<PathBuf>> {
        let dir = cache_dir()?;
        let name = if cfg!(windows) {
            format!("ffmpeg-{KENNZEICHEN}.exe")
        } else {
            format!("ffmpeg-{KENNZEICHEN}")
        };
        let ziel = dir.join(&name);

        // Größe mitprüfen: ein abgebrochenes Entpacken hinterlässt sonst eine
        // halbe Datei, die bei jedem Start als fertig gilt.
        if let Ok(m) = std::fs::metadata(&ziel) {
            if m.len() == ROHGROESSE {
                return Ok(Some(ziel));
            }
        }

        std::fs::create_dir_all(&dir)
            .with_context(|| format!("{} lässt sich nicht anlegen", dir.display()))?;

        // Erst neben das Ziel schreiben, dann umbenennen. Zwei gleichzeitig
        // gestartete vidinsh-Prozesse zerlegen sich sonst die Datei.
        let tmp = dir.join(format!("{name}.{}.teil", std::process::id()));
        {
            let datei = std::fs::File::create(&tmp)
                .with_context(|| format!("{} lässt sich nicht schreiben", tmp.display()))?;
            let mut aus = std::io::BufWriter::with_capacity(1 << 20, datei);
            zstd::stream::copy_decode(GEPACKT, &mut aus)
                .context("Das mitgelieferte ffmpeg lässt sich nicht entpacken")?;
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
        Ok(Some(ziel))
    }
}

#[cfg(not(feature = "bundled"))]
mod bundled {
    use anyhow::Result;
    use std::path::PathBuf;

    pub fn entpacken() -> Result<Option<PathBuf>> {
        Ok(None)
    }
}

/// Für `--verbose` und `--probe`.
pub fn beschreibung() -> String {
    let p = PFAD.get().map(|p| p.display().to_string());
    match (cfg!(feature = "bundled"), p) {
        (true, Some(p)) => format!("{p} (mitgeliefert)"),
        (false, Some(p)) => p,
        (_, None) => "noch nicht festgelegt".into(),
    }
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
        let p = ffmpeg();
        assert!(!p.as_os_str().is_empty());
    }

    #[test]
    fn ffmpeg_ist_im_pfad_auffindbar() {
        // Auf der Entwicklungsmaschine muss das gelten; sonst laufen die
        // übrigen Tests ohnehin nicht.
        assert!(im_pfad("ffmpeg"));
        assert!(!im_pfad("ein-programm-das-es-nicht-gibt"));
    }
}
