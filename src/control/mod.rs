//! Tastatur und Fenstergröße -> Kommandos.

pub mod clock;

use crate::render::Mode;
use crossbeam_channel::Sender;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Cmd {
    Quit,
    TogglePause,
    /// Sprung in Sekunden, relativ
    Seek(f64),
    SetMode(Mode),
    NextColor,
    ToggleBg,
    ToggleUi,
    ToggleDither,
    /// Faktor auf das Tempo
    SpeedStep(f64),
    VolumeStep(i32),
    Resize(u16, u16),
}

/// Reine Abbildung Taste -> Kommando, damit sie ohne Terminal prüfbar ist.
pub fn map_key(k: KeyEvent) -> Option<Cmd> {
    // Windows meldet Drücken *und* Loslassen. Ohne diesen Filter löst jede
    // Taste doppelt aus.
    if k.kind != KeyEventKind::Press {
        return None;
    }

    if k.modifiers.contains(KeyModifiers::CONTROL) {
        return match k.code {
            // Im Rohmodus kommt Ctrl-C als Taste an, nicht als Signal.
            KeyCode::Char('c') | KeyCode::Char('d') => Some(Cmd::Quit),
            _ => None,
        };
    }

    Some(match k.code {
        KeyCode::Char('q') | KeyCode::Esc => Cmd::Quit,
        KeyCode::Char(' ') => Cmd::TogglePause,
        KeyCode::Char('1') => Cmd::SetMode(Mode::Ramp),
        KeyCode::Char('2') => Cmd::SetMode(Mode::Glyph),
        KeyCode::Char('3') => Cmd::SetMode(Mode::Blocks),
        KeyCode::Char('4') => Cmd::SetMode(Mode::Edge),
        KeyCode::Char('c') => Cmd::NextColor,
        KeyCode::Char('b') => Cmd::ToggleBg,
        KeyCode::Char('i') => Cmd::ToggleUi,
        KeyCode::Char('d') => Cmd::ToggleDither,
        KeyCode::Left => Cmd::Seek(-5.0),
        KeyCode::Right => Cmd::Seek(5.0),
        KeyCode::Up => Cmd::VolumeStep(10),
        KeyCode::Down => Cmd::VolumeStep(-10),
        KeyCode::Char('+') | KeyCode::Char('=') => Cmd::SpeedStep(1.25),
        KeyCode::Char('-') | KeyCode::Char('_') => Cmd::SpeedStep(0.8),
        _ => return None,
    })
}

/// Liest Ereignisse, bis `stop` gesetzt wird oder der Kanal bricht.
pub fn spawn(tx: Sender<Cmd>, stop: Arc<AtomicBool>) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        while !stop.load(Ordering::Relaxed) {
            // Gepollt statt blockierend, damit der Thread beim Beenden nicht
            // auf einen letzten Tastendruck wartet.
            match event::poll(Duration::from_millis(100)) {
                Ok(true) => {}
                Ok(false) => continue,
                Err(_) => break,
            }
            let Ok(ev) = event::read() else { break };
            let cmd = match ev {
                Event::Key(k) => map_key(k),
                Event::Resize(w, h) => Some(Cmd::Resize(w, h)),
                _ => None,
            };
            if let Some(c) = cmd
                && tx.send(c).is_err()
            {
                break;
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn taste(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    fn code(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }

    #[test]
    fn modi_liegen_auf_den_zifferntasten() {
        assert_eq!(map_key(taste('1')), Some(Cmd::SetMode(Mode::Ramp)));
        assert_eq!(map_key(taste('2')), Some(Cmd::SetMode(Mode::Glyph)));
        assert_eq!(map_key(taste('3')), Some(Cmd::SetMode(Mode::Blocks)));
        assert_eq!(map_key(taste('4')), Some(Cmd::SetMode(Mode::Edge)));
    }

    #[test]
    fn beenden_ueber_q_esc_und_ctrl_c() {
        assert_eq!(map_key(taste('q')), Some(Cmd::Quit));
        assert_eq!(map_key(code(KeyCode::Esc)), Some(Cmd::Quit));
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(Cmd::Quit)
        );
    }

    #[test]
    fn ctrl_c_hat_vorrang_vor_der_farbtaste() {
        assert_eq!(map_key(taste('c')), Some(Cmd::NextColor));
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(Cmd::Quit)
        );
    }

    #[test]
    fn loslassen_loest_nichts_aus() {
        let mut k = taste('q');
        k.kind = KeyEventKind::Release;
        assert_eq!(map_key(k), None, "sonst feuert auf Windows alles doppelt");
    }

    #[test]
    fn spulen_und_tempo() {
        assert_eq!(map_key(code(KeyCode::Left)), Some(Cmd::Seek(-5.0)));
        assert_eq!(map_key(code(KeyCode::Right)), Some(Cmd::Seek(5.0)));
        assert_eq!(map_key(taste('+')), Some(Cmd::SpeedStep(1.25)));
        assert_eq!(map_key(taste('-')), Some(Cmd::SpeedStep(0.8)));
    }

    #[test]
    fn unbelegte_tasten_werden_verworfen() {
        assert_eq!(map_key(taste('z')), None);
        assert_eq!(map_key(code(KeyCode::F(5))), None);
    }
}
