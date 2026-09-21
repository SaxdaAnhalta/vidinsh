//! Packt ffmpeg in die Programmdatei -- nur bei `--features bundled`.
//!
//! Ergebnis ist eine einzelne Exe, die auf einem Rechner läuft, auf dem nichts
//! installiert ist. Ohne die Eigenschaft passiert hier gar nichts und vidinsh
//! bleibt bei rund 1,5 MB.
//!
//! Welches ffmpeg eingepackt wird, bestimmt `VIDINSH_FFMPEG`; ohne die
//! Variable wird das aus dem PATH genommen.

use std::io::Write;
use std::path::{Path, PathBuf};

/// Kompressionsstufe. 12 ist der Punkt, an dem zstd bei einer 230-MB-Datei
/// noch in wenigen Minuten fertig wird; darüber wächst die Bauzeit stark,
/// ohne dass die Datei nennenswert kleiner wird.
const STUFE: i32 = 12;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=VIDINSH_FFMPEG");

    if std::env::var_os("CARGO_FEATURE_BUNDLED").is_none() {
        return;
    }

    let quelle = match ffmpeg_finden() {
        Some(p) => p,
        None => {
            println!(
                "cargo:warning=--features bundled verlangt ein ffmpeg zum Einpacken. \
                 Keines gefunden -- entweder in den PATH legen oder VIDINSH_FFMPEG \
                 auf die Programmdatei zeigen lassen."
            );
            std::process::exit(1);
        }
    };
    println!("cargo:rerun-if-changed={}", quelle.display());

    let roh = std::fs::read(&quelle)
        .unwrap_or_else(|e| panic!("{} lässt sich nicht lesen: {e}", quelle.display()));
    let rohgroesse = roh.len() as u64;

    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR fehlt"));
    let ziel = out.join("ffmpeg.zst");

    // Nicht bei jedem Bau neu packen -- das dauert Minuten.
    let kennzeichen = format!("{:016x}", fnv1a(&roh));
    let marke = out.join("ffmpeg.marke");
    let aktuell = std::fs::read_to_string(&marke).unwrap_or_default();

    if aktuell.trim() != kennzeichen || !ziel.is_file() {
        println!(
            "cargo:warning=Packe {} ({:.0} MB) in die Programmdatei -- das dauert einige Minuten.",
            quelle.display(),
            rohgroesse as f64 / 1_048_576.0
        );
        let gepackt = zstd::stream::encode_all(&roh[..], STUFE).expect("zstd schlug fehl");
        std::fs::write(&ziel, &gepackt).expect("gepacktes ffmpeg lässt sich nicht ablegen");
        std::fs::write(&marke, &kennzeichen).ok();
        println!(
            "cargo:warning=Fertig: {:.0} MB -> {:.0} MB",
            rohgroesse as f64 / 1_048_576.0,
            gepackt.len() as f64 / 1_048_576.0
        );
    }

    let mut f = std::fs::File::create(out.join("bundled.rs")).expect("bundled.rs");
    write!(
        f,
        "/// Das gepackte ffmpeg.\n\
         pub const GEPACKT: &[u8] = include_bytes!(concat!(env!(\"OUT_DIR\"), \"/ffmpeg.zst\"));\n\
         /// Kennzeichen des Inhalts -- steckt im Dateinamen des Entpackten,\n\
         /// damit zwei Fassungen von vidinsh sich nicht ins Gehege kommen.\n\
         pub const KENNZEICHEN: &str = \"{kennzeichen}\";\n\
         /// Größe im entpackten Zustand, zur Prüfung auf halbe Dateien.\n\
         pub const ROHGROESSE: u64 = {rohgroesse};\n"
    )
    .expect("bundled.rs schreiben");
}

fn ffmpeg_finden() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("VIDINSH_FFMPEG") {
        let p = PathBuf::from(p);
        return p.is_file().then_some(p);
    }
    let name = if cfg!(windows) {
        "ffmpeg.exe"
    } else {
        "ffmpeg"
    };
    std::env::var_os("PATH")
        .map(|pfad| std::env::split_paths(&pfad).collect::<Vec<_>>())?
        .into_iter()
        .map(|d| d.join(name))
        .find(|p| p.is_file())
        .filter(|p| p.as_path() != Path::new(""))
}

/// FNV-1a. Reicht hier vollkommen: es geht darum, eine andere Datei von einer
/// gleichen zu unterscheiden, nicht um Manipulationsschutz.
fn fnv1a(daten: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in daten {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}
