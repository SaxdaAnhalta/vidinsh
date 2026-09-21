//! Terminalzustand: Betreten, Verlassen, und die Zusicherung, dass beides
//! auch bei einer Panik oder Ctrl-C zusammenpasst.

pub mod caps;
pub mod ui;
pub mod writer;

use anyhow::Result;
use std::io::{Write, stdout};
use std::sync::atomic::{AtomicBool, Ordering};

/// Autowrap aus. Sonst scrollt das Terminal, sobald wir in die letzte Spalte
/// der letzten Zeile schreiben, und das ganze Bild rutscht hoch.
const DECAWM_OFF: &str = "\x1b[?7l";
const DECAWM_ON: &str = "\x1b[?7h";

static ACTIVE: AtomicBool = AtomicBool::new(false);

/// Bringt das Terminal in den Wiedergabezustand und wieder heraus.
///
/// Der Teardown läuft über `Drop`, über den Panik-Haken und über den
/// Ctrl-C-Handler -- alle drei rufen dieselbe Funktion, die mehrfaches
/// Aufrufen verträgt. Bleibt der Alternativschirm oder der Rohmodus stehen,
/// ist die Shell danach unbenutzbar; das ist den doppelten Boden wert.
pub struct TermGuard {
    old_codepage: Option<u32>,
}

impl TermGuard {
    pub fn enter() -> Result<Self> {
        let old_codepage = platform::set_utf8();
        platform::enable_vt();

        crossterm::terminal::enable_raw_mode()?;
        let mut o = stdout();
        crossterm::execute!(
            o,
            crossterm::terminal::EnterAlternateScreen,
            crossterm::cursor::Hide
        )?;
        o.write_all(DECAWM_OFF.as_bytes())?;
        o.flush()?;

        ACTIVE.store(true, Ordering::SeqCst);
        Ok(TermGuard { old_codepage })
    }
}

impl Drop for TermGuard {
    fn drop(&mut self) {
        restore();
        if let Some(cp) = self.old_codepage.take() {
            platform::restore_codepage(cp);
        }
    }
}

/// Idempotenter Teardown. Fehler werden geschluckt: wir sind hier
/// möglicherweise schon im Absturz, und eine zweite Fehlermeldung hilft
/// niemandem mehr.
pub fn restore() {
    if !ACTIVE.swap(false, Ordering::SeqCst) {
        return;
    }
    let mut o = stdout();
    let _ = o.write_all(DECAWM_ON.as_bytes());
    let _ = o.write_all(b"\x1b[0m");
    let _ = crossterm::execute!(
        o,
        crossterm::cursor::Show,
        crossterm::terminal::LeaveAlternateScreen
    );
    let _ = crossterm::terminal::disable_raw_mode();
    let _ = o.flush();
}

/// Hängt den Teardown vor den normalen Panik-Bericht. Ohne das erscheint die
/// Meldung auf dem Alternativschirm und verschwindet sofort wieder mit ihm.
pub fn install_panic_hook() {
    let vorher = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore();
        vorher(info);
    }));
}

// ---------------------------------------------------------------- Plattform

#[cfg(windows)]
pub mod platform {
    use windows_sys::Win32::System::Console::{
        CONSOLE_FONT_INFOEX, ENABLE_VIRTUAL_TERMINAL_PROCESSING, GetConsoleMode,
        GetConsoleOutputCP, GetCurrentConsoleFontEx, GetStdHandle, STD_OUTPUT_HANDLE,
        SetConsoleMode, SetConsoleOutputCP,
    };

    const CP_UTF8: u32 = 65001;

    fn out_handle() -> *mut core::ffi::c_void {
        unsafe { GetStdHandle(STD_OUTPUT_HANDLE) }
    }

    /// Schaltet die Verarbeitung von Escape-Sequenzen ein. Gelingt das, kann
    /// die Konsole ANSI -- das ist auf Windows der eigentliche Test, nicht
    /// irgendeine Umgebungsvariable.
    pub fn enable_vt() -> bool {
        unsafe {
            let h = out_handle();
            let mut mode = 0u32;
            if GetConsoleMode(h, &mut mode) == 0 {
                // Kein Konsolenhandle -- Umleitung in eine Datei oder Pipe.
                return false;
            }
            if mode & ENABLE_VIRTUAL_TERMINAL_PROCESSING != 0 {
                return true;
            }
            SetConsoleMode(h, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING) != 0
        }
    }

    /// Stellt die Ausgabe auf UTF-8 um und liefert die vorherige Codepage.
    pub fn set_utf8() -> Option<u32> {
        unsafe {
            let alt = GetConsoleOutputCP();
            if alt == CP_UTF8 {
                return None;
            }
            if SetConsoleOutputCP(CP_UTF8) != 0 {
                Some(alt)
            } else {
                None
            }
        }
    }

    pub fn restore_codepage(cp: u32) {
        unsafe {
            SetConsoleOutputCP(cp);
        }
    }

    /// Die alte Rasterschrift der Konsole kann keine Blockzeichen darstellen.
    /// Statt Kästchen anzuzeigen, fallen wir dann auf ASCII zurück.
    pub fn has_raster_font() -> bool {
        unsafe {
            let mut info: CONSOLE_FONT_INFOEX = core::mem::zeroed();
            info.cbSize = core::mem::size_of::<CONSOLE_FONT_INFOEX>() as u32;
            if GetCurrentConsoleFontEx(out_handle(), 0, &mut info) == 0 {
                return false; // nicht feststellbar -> nicht schlechter machen als nötig
            }
            let ende = info
                .FaceName
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(info.FaceName.len());
            let name = String::from_utf16_lossy(&info.FaceName[..ende]);
            name.is_empty() || name == "Terminal"
        }
    }
}

#[cfg(not(windows))]
pub mod platform {
    pub fn enable_vt() -> bool {
        true
    }
    pub fn set_utf8() -> Option<u32> {
        None
    }
    pub fn restore_codepage(_cp: u32) {}
    pub fn has_raster_font() -> bool {
        false
    }
}
