//! Was kann dieses Terminal?
//!
//! Die Shell spielt dabei keine Rolle -- `vidinsh` schreibt ANSI-Bytes nach
//! stdout und merkt nicht, ob PowerShell, cmd oder bash sie gestartet hat.
//! Was variiert, ist der Terminal-Emulator dahinter.
//!
//! Erkannt wird über Umgebungsvariablen und, auf Windows, über den Erfolg von
//! `SetConsoleMode`. Bewusst *keine* aktive Abfrage per Escape-Sequenz: die
//! ist langsam, und Terminals die nicht antworten hinterlassen Müll auf dem
//! Schirm oder blockieren.

use super::platform;
use crate::render::{Charset, Mode, color::ColorMode};
use std::env;
use std::io::IsTerminal;

#[derive(Clone, Debug)]
pub struct Caps {
    pub color: ColorMode,
    pub sync: bool,
    pub unicode: bool,
    pub is_tty: bool,
    /// Name des erkannten Terminals, nur für Ausgabe und Fehlersuche
    pub terminal: String,
    /// Wurde eine Fähigkeit herabgestuft, steht hier warum
    pub notes: Vec<String>,
}

fn var(k: &str) -> String {
    env::var(k).unwrap_or_default()
}

/// Ermittelt die Fähigkeiten. `wunsch_color` überschreibt die Erkennung.
pub fn detect(wunsch_color: Option<ColorMode>) -> Caps {
    let mut notes = Vec::new();
    let is_tty = std::io::stdout().is_terminal();

    let wt = env::var_os("WT_SESSION").is_some();
    let conemu = var("ConEmuANSI").eq_ignore_ascii_case("on");
    let prog = var("TERM_PROGRAM");
    let term = var("TERM");
    let colorterm = var("COLORTERM");
    let kitty = env::var_os("KITTY_WINDOW_ID").is_some() || term == "xterm-kitty";
    let in_tmux = env::var_os("TMUX").is_some() || term.starts_with("screen");

    // Auf Windows ist das der eigentliche Test: geht SetConsoleMode durch,
    // versteht die Konsole Escape-Sequenzen.
    let vt = platform::enable_vt();

    let terminal = if wt {
        "Windows Terminal".into()
    } else if kitty {
        "kitty".into()
    } else if conemu {
        "ConEmu".into()
    } else if !prog.is_empty() {
        prog.clone()
    } else if !term.is_empty() {
        term.clone()
    } else if cfg!(windows) {
        "conhost".into()
    } else {
        "unbekannt".into()
    };

    // Apple_Terminal fehlt hier bewusst: Terminal.app kann bis heute kein
    // 24-Bit und stellt Truecolor-Sequenzen falsch dar.
    let truecolor_prog = matches!(
        prog.as_str(),
        "vscode" | "iTerm.app" | "WezTerm" | "ghostty"
    );
    let truecolor_env =
        colorterm.eq_ignore_ascii_case("truecolor") || colorterm.eq_ignore_ascii_case("24bit");

    let erkannt = if env::var_os("NO_COLOR").is_some() {
        notes.push("NO_COLOR ist gesetzt -> mono".into());
        ColorMode::Mono
    } else if in_tmux && !truecolor_env {
        notes.push(
            "tmux/screen erkannt -> 256. Für RGB in tmux: \
             set -as terminal-features \",*:RGB\""
                .into(),
        );
        ColorMode::Ansi256
    } else if wt || kitty || conemu || truecolor_prog || truecolor_env {
        ColorMode::Truecolor
    } else if cfg!(windows) {
        if vt {
            // conhost ab Win10 1511 kann 24 Bit.
            ColorMode::Truecolor
        } else {
            notes.push(
                "Konsole versteht keine Escape-Sequenzen (SetConsoleMode \
                 abgelehnt) -> 16 Farben"
                    .into(),
            );
            ColorMode::Ansi16
        }
    } else if term.contains("256color") {
        ColorMode::Ansi256
    } else if term.is_empty() || term == "dumb" {
        notes.push("TERM ist leer oder 'dumb' -> mono".into());
        ColorMode::Mono
    } else {
        notes.push(format!("TERM='{term}' unbekannt -> 16 Farben"));
        ColorMode::Ansi16
    };

    let color = match wunsch_color {
        Some(c) => c,
        None => erkannt,
    };

    // Synchronisierte Ausgabe (DECSET 2026). Unbekannte Terminals ignorieren
    // den Modus normalerweise stillschweigend -- "normalerweise" reicht hier
    // nicht, also nur bei belegter Unterstützung.
    let sync = wt
        || kitty
        || matches!(prog.as_str(), "WezTerm" | "iTerm.app" | "ghostty")
        || term == "foot"
        || term.starts_with("alacritty");

    let unicode = if cfg!(windows) {
        if platform::has_raster_font() {
            notes.push(
                "Konsole benutzt die Rasterschrift 'Terminal', die keine \
                 Blockzeichen kennt -> ASCII"
                    .into(),
            );
            false
        } else {
            vt
        }
    } else {
        let l = format!("{}{}{}", var("LC_ALL"), var("LC_CTYPE"), var("LANG"));
        let ok =
            l.to_ascii_uppercase().contains("UTF-8") || l.to_ascii_uppercase().contains("UTF8");
        if !ok {
            notes.push("Locale meldet kein UTF-8 -> ASCII".into());
        }
        ok
    };

    if !is_tty {
        notes.push(
            "Ausgabe ist kein Terminal. Rastergröße und Farbtiefe lassen sich \
             nicht ermitteln -- --size und --color setzen."
                .into(),
        );
    }

    Caps {
        color,
        sync,
        unicode,
        is_tty,
        terminal,
        notes,
    }
}

/// Wählt den Zeichensatz.
///
/// Eine ausdrückliche Angabe gilt immer -- auch dann, wenn die Erkennung kein
/// Unicode sieht. Das ist Absicht: die Erkennung kann nur raten (bei Ausgabe
/// in eine Datei etwa weiß sie gar nichts über das Terminal, das die Datei
/// später anzeigt), und wer `--charset quad` tippt, hat entschieden. Gewarnt
/// wird trotzdem. Heruntergestuft wird nur die *automatische* Wahl -- dort
/// ist ein gröberes Bild besser als eine Wand aus Ersatzkästchen.
pub fn resolve_charset(
    mode: Mode,
    wunsch: Option<Charset>,
    unicode: bool,
    notes: &mut Vec<String>,
) -> Charset {
    if let Some(c) = wunsch {
        if c.needs_unicode() && !unicode {
            notes.push(format!(
                "--charset {} braucht Unicode, das hier nicht erkannt wurde -- \
                 wird trotzdem benutzt. Bei Ersatzkästchen hilft --ascii.",
                c.label()
            ));
        }
        return c;
    }

    let automatisch = match mode {
        Mode::Blocks => Charset::Half,
        _ => Charset::Extended,
    };
    if automatisch.needs_unicode() && !unicode {
        Charset::Ascii
    } else {
        automatisch
    }
}

impl Caps {
    /// Einzeiler für die Statuszeile und `--verbose`.
    pub fn summary(&self) -> String {
        format!(
            "{} | Farbe {} | Unicode {} | Sync {} | TTY {}",
            self.terminal,
            self.color.label(),
            if self.unicode { "ja" } else { "nein" },
            if self.sync { "ja" } else { "nein" },
            if self.is_tty { "ja" } else { "nein" },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wunsch_schlaegt_erkennung() {
        let c = detect(Some(ColorMode::Mono));
        assert_eq!(c.color, ColorMode::Mono);
        let c = detect(Some(ColorMode::Truecolor));
        assert_eq!(c.color, ColorMode::Truecolor);
    }

    #[test]
    fn automatische_wahl_faellt_ohne_unicode_auf_ascii() {
        let mut n = Vec::new();
        assert_eq!(
            resolve_charset(Mode::Blocks, None, false, &mut n),
            Charset::Ascii
        );
        assert_eq!(
            resolve_charset(Mode::Ramp, None, false, &mut n),
            Charset::Ascii
        );
    }

    #[test]
    fn ausdrueckliche_wahl_gilt_auch_ohne_erkanntes_unicode() {
        // Bei Ausgabe in eine Datei weiß die Erkennung nichts über das
        // Terminal, das die Datei später anzeigt -- die Angabe muss gelten.
        let mut n = Vec::new();
        assert_eq!(
            resolve_charset(Mode::Blocks, Some(Charset::Quad), false, &mut n),
            Charset::Quad
        );
        assert!(!n.is_empty(), "das Risiko muss benannt werden");
        assert!(
            n[0].contains("--ascii"),
            "der Ausweg muss dabeistehen: {}",
            n[0]
        );
    }

    #[test]
    fn charset_default_haengt_am_modus() {
        let mut n = Vec::new();
        assert_eq!(
            resolve_charset(Mode::Blocks, None, true, &mut n),
            Charset::Half
        );
        assert_eq!(
            resolve_charset(Mode::Ramp, None, true, &mut n),
            Charset::Extended
        );
        assert!(n.is_empty(), "der Normalfall braucht keine Anmerkung");
    }

    #[test]
    fn ascii_wird_nie_beanstandet() {
        let mut n = Vec::new();
        assert_eq!(
            resolve_charset(Mode::Ramp, Some(Charset::Ascii), false, &mut n),
            Charset::Ascii
        );
        assert!(n.is_empty());
    }

    #[test]
    fn erkennung_liefert_immer_einen_terminalnamen() {
        let c = detect(None);
        assert!(!c.terminal.is_empty());
    }
}
