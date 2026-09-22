//! Packt die Hilfsprogramme in die Programmdatei -- nur bei
//! `--features bundled`.
//!
//! Ergebnis ist eine einzelne Exe, die auf einem Rechner läuft, auf dem nichts
//! installiert ist. Ohne die Eigenschaft passiert hier gar nichts und vidinsh
//! bleibt bei rund 1,6 MB.
//!
//! * **ffmpeg** ist Pflicht. Fehlt es, bricht der Bau ab -- eine mitgelieferte
//!   Fassung ohne ffmpeg wäre eine Mogelpackung.
//! * **yt-dlp** ist freiwillig. Fehlt es, entsteht eine Exe, die alles außer
//!   Portal-Links kann, und sagt das auch.
//!
//! Welche Dateien eingepackt werden, bestimmen `VIDINSH_FFMPEG` und
//! `VIDINSH_YTDLP`; ohne die Variablen wird gesucht.

use std::io::Write;
use std::path::PathBuf;

/// Kompressionsstufe. 12 ist der Punkt, an dem zstd bei einer 230-MB-Datei in
/// gut einer halben Minute fertig wird; darüber wächst die Bauzeit stark,
/// ohne dass die Datei nennenswert kleiner wird.
const STUFE: i32 = 12;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=VIDINSH_FFMPEG");
    println!("cargo:rerun-if-env-changed=VIDINSH_YTDLP");

    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR fehlt"));

    if std::env::var_os("CARGO_FEATURE_BUNDLED").is_none() {
        return;
    }

    let ffmpeg = suchen("VIDINSH_FFMPEG", "ffmpeg", &[]).unwrap_or_else(|| {
        println!(
            "cargo:warning=--features bundled verlangt ein ffmpeg zum Einpacken. \
             Keines gefunden -- entweder in den PATH legen oder VIDINSH_FFMPEG \
             auf die Programmdatei zeigen lassen."
        );
        std::process::exit(1);
    });

    // yt-dlp liegt üblicherweise im Projektordner unter tools/.
    let ytdlp = suchen("VIDINSH_YTDLP", "yt-dlp", &["tools"]);
    if ytdlp.is_none() {
        println!(
            "cargo:warning=Kein yt-dlp gefunden -- die Exe entsteht ohne. \
             Alles außer YouTube und anderen Portalen funktioniert; für die \
             yt-dlp nach tools/ legen und neu bauen."
        );
    }

    let f = packen(&out, "ffmpeg", &ffmpeg);
    let y = ytdlp.as_ref().map(|p| packen(&out, "ytdlp", p));

    let mut datei = std::fs::File::create(out.join("bundled.rs")).expect("bundled.rs");
    writeln!(
        datei,
        "/// Das gepackte ffmpeg.\n\
         pub const FFMPEG: Werkzeug = Werkzeug {{\n\
         \x20   gepackt: include_bytes!(concat!(env!(\"OUT_DIR\"), \"/ffmpeg.zst\")),\n\
         \x20   kennzeichen: \"{}\",\n\
         \x20   rohgroesse: {},\n\
         }};",
        f.0, f.1
    )
    .unwrap();

    match y {
        Some((kennzeichen, rohgroesse)) => writeln!(
            datei,
            "/// Das gepackte yt-dlp, sofern beim Bau eines vorlag.\n\
             pub const YTDLP: Option<Werkzeug> = Some(Werkzeug {{\n\
             \x20   gepackt: include_bytes!(concat!(env!(\"OUT_DIR\"), \"/ytdlp.zst\")),\n\
             \x20   kennzeichen: \"{kennzeichen}\",\n\
             \x20   rohgroesse: {rohgroesse},\n\
             }});"
        ),
        None => writeln!(datei, "pub const YTDLP: Option<Werkzeug> = None;"),
    }
    .unwrap();
}

/// Packt eine Datei nach `OUT_DIR/<name>.zst` und liefert Kennzeichen und
/// Rohgröße. Das Ergebnis wird zwischengespeichert -- der zweite Bau
/// überspringt das Packen.
fn packen(out: &std::path::Path, name: &str, quelle: &std::path::Path) -> (String, u64) {
    println!("cargo:rerun-if-changed={}", quelle.display());

    let roh = std::fs::read(quelle)
        .unwrap_or_else(|e| panic!("{} lässt sich nicht lesen: {e}", quelle.display()));
    let rohgroesse = roh.len() as u64;
    let kennzeichen = format!("{:016x}", fnv1a(&roh));

    let ziel = out.join(format!("{name}.zst"));
    let marke = out.join(format!("{name}.marke"));

    if std::fs::read_to_string(&marke).unwrap_or_default().trim() == kennzeichen && ziel.is_file() {
        return (kennzeichen, rohgroesse);
    }

    println!(
        "cargo:warning=Packe {} ({:.0} MB) in die Programmdatei.",
        quelle.display(),
        rohgroesse as f64 / 1_048_576.0
    );
    let gepackt = zstd::stream::encode_all(&roh[..], STUFE).expect("zstd schlug fehl");
    std::fs::write(&ziel, &gepackt).expect("gepacktes Werkzeug lässt sich nicht ablegen");
    std::fs::write(&marke, &kennzeichen).ok();
    println!(
        "cargo:warning={name}: {:.0} MB -> {:.0} MB",
        rohgroesse as f64 / 1_048_576.0,
        gepackt.len() as f64 / 1_048_576.0
    );

    (kennzeichen, rohgroesse)
}

/// Sucht eine Programmdatei: erst die Umgebungsvariable, dann die angegebenen
/// Ordner relativ zum Projekt, zuletzt den PATH.
fn suchen(variable: &str, name: &str, ordner: &[&str]) -> Option<PathBuf> {
    if let Some(p) = std::env::var_os(variable) {
        let p = PathBuf::from(p);
        return p.is_file().then_some(p);
    }
    let datei = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    };

    for o in ordner {
        let p = PathBuf::from(o).join(&datei);
        if p.is_file() {
            return Some(p);
        }
    }

    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|d| d.join(&datei))
        .find(|p| p.is_file())
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
