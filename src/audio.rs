//! Ton über ffplay als eigenen Prozess.
//!
//! **Einordnung, ehrlich:** ffplay hat eine eigene Uhr, die sich von außen
//! weder auslesen noch steuern lässt. Getragen wird das Ganze davon, dass
//! beide Seiten dieselbe Quelle mit konstanter Bildrate lesen und das Video
//! sich strikt an seine eigene Uhr hält -- es verwirft Bilder bei Rückstand,
//! statt sie nachzuholen. Über Minuten ist die Abweichung nicht wahrnehmbar.
//!
//! Der saubere Weg wäre: Ton als zweite ffmpeg-Pipe als rohes PCM holen, über
//! cpal ausgeben und die Abspielposition zur Leit-Uhr machen. Das ist die
//! richtige Architektur und bewusst zurückgestellt, nicht vergessen.
//!
//! Weil ffplay keine Steuerung von außen kennt, werden Pause, Spulen und
//! Lautstärke durch einen Neustart an der passenden Stelle umgesetzt.

use crate::source::input::{Input, Kind};
use std::process::{Child, Command, Stdio};

pub struct Audio {
    child: Option<Child>,
    input: Input,
    volume: u32,
    paused: bool,
}

/// `None`, wenn für diese Quelle kein Ton in Frage kommt.
pub fn start(input: &Input, from: f64, volume: u32) -> Option<Audio> {
    if matches!(input.kind, Kind::Camera | Kind::Stdin) {
        // Die Kamera liefert keinen Ton, und stdin lässt sich nicht zweimal
        // lesen -- ein zweiter Prozess würde dem Video die Daten wegnehmen.
        return None;
    }
    let mut a = Audio {
        child: None,
        input: input.clone(),
        volume,
        paused: false,
    };
    a.spawn(from);
    a.child.is_some().then_some(a)
}

impl Audio {
    fn spawn(&mut self, from: f64) {
        self.kill();
        if self.paused {
            return;
        }

        let mut c = Command::new("ffplay");
        c.args(["-nodisp", "-autoexit", "-loglevel", "quiet", "-vn"]);
        c.args(["-volume", &self.volume.to_string()]);
        c.args(&self.input.pre_args);
        if from > 0.0 {
            c.args(["-ss", &format!("{from:.3}")]);
        }
        c.args(["-i", &self.input.ffmpeg_input]);

        self.child = c
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .ok();
    }

    fn kill(&mut self) {
        if let Some(mut ch) = self.child.take() {
            let _ = ch.kill();
            let _ = ch.wait();
        }
    }

    pub fn set_paused(&mut self, paused: bool, pos: f64) {
        if paused == self.paused {
            return;
        }
        self.paused = paused;
        if paused {
            self.kill();
        } else {
            self.spawn(pos);
        }
    }

    pub fn set_volume(&mut self, volume: u32, pos: f64) {
        let volume = volume.min(100);
        if volume == self.volume {
            return;
        }
        self.volume = volume;
        if !self.paused {
            self.spawn(pos);
        }
    }

    /// Nach dem Spulen an der neuen Stelle neu ansetzen.
    pub fn seek(&mut self, pos: f64) {
        self.spawn(pos);
    }

    pub fn stop(&mut self) {
        self.kill();
    }
}

impl Drop for Audio {
    fn drop(&mut self) {
        self.kill();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::input;

    #[test]
    fn kamera_und_stdin_bekommen_keinen_ton() {
        assert!(start(&input::classify("cam:0"), 0.0, 100).is_none());
        assert!(
            start(&input::classify("-"), 0.0, 100).is_none(),
            "stdin lässt sich nicht von zwei Prozessen lesen"
        );
    }
}
