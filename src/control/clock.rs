//! Die Wiedergabe-Uhr.
//!
//! Grundregel: Bilder richten sich nach der Uhr, nicht die Uhr nach den
//! Bildern. Wer hinterherhängt, überspringt -- sonst läuft das Video immer
//! langsamer, je aufwendiger das Rendern ist, und der Ton wandert davon.

use std::time::{Duration, Instant};

pub struct Clock {
    /// Wanduhr-Zeitpunkt, der `anchor_media` entspricht
    anchor_wall: Instant,
    /// Medienposition am Anker
    anchor_media: Duration,
    speed: f64,
    paused_since: Option<Instant>,
}

impl Clock {
    pub fn new(speed: f64) -> Self {
        Clock {
            anchor_wall: Instant::now(),
            anchor_media: Duration::ZERO,
            speed: sane_speed(speed),
            paused_since: None,
        }
    }

    /// Setzt die Uhr auf eine Medienposition -- nach dem Spulen und nach jedem
    /// ffmpeg-Neustart.
    pub fn seek_to(&mut self, media: Duration) {
        self.anchor_media = media;
        self.anchor_wall = Instant::now();
        if self.paused_since.is_some() {
            self.paused_since = Some(self.anchor_wall);
        }
    }

    pub fn speed(&self) -> f64 {
        self.speed
    }

    /// Verändert das Tempo, ohne dass die Position springt.
    pub fn set_speed(&mut self, speed: f64) {
        let jetzt = self.media_now();
        self.speed = sane_speed(speed);
        self.seek_to(jetzt);
    }

    pub fn is_paused(&self) -> bool {
        self.paused_since.is_some()
    }

    pub fn set_paused(&mut self, an: bool) {
        match (an, self.paused_since) {
            (true, None) => self.paused_since = Some(Instant::now()),
            (false, Some(seit)) => {
                // Die Pausendauer aus der Rechnung nehmen, statt die Position
                // vorzuspulen.
                self.anchor_wall += seit.elapsed();
                self.paused_since = None;
            }
            _ => {}
        }
    }

    pub fn toggle_pause(&mut self) {
        self.set_paused(!self.is_paused());
    }

    /// Aktuelle Medienposition.
    pub fn media_now(&self) -> Duration {
        let bezug = self.paused_since.unwrap_or_else(Instant::now);
        let verstrichen = bezug.saturating_duration_since(self.anchor_wall);
        self.anchor_media + verstrichen.mul_f64(self.speed)
    }

    /// Wann dieses Bild an der Reihe ist.
    pub fn target(&self, pts: Duration) -> Instant {
        let delta = pts.saturating_sub(self.anchor_media);
        self.anchor_wall + delta.div_f64(self.speed)
    }

    /// Wie weit ein Bild schon überfällig ist. `None`, wenn es noch kommt.
    pub fn lateness(&self, pts: Duration) -> Option<Duration> {
        if self.is_paused() {
            return None;
        }
        Instant::now().checked_duration_since(self.target(pts))
    }

    /// Wartet, bis das Bild dran ist. Kehrt sofort zurück, wenn es das schon
    /// ist -- und wartet nie in der Pause, dort blockiert der Aufrufer.
    pub fn sleep_until(&self, pts: Duration) {
        let ziel = self.target(pts);
        let jetzt = Instant::now();
        if ziel > jetzt {
            std::thread::sleep(ziel - jetzt);
        }
    }
}

/// Tempo in einem Bereich halten, in dem die Rechnung nicht entgleist.
fn sane_speed(s: f64) -> f64 {
    if s.is_finite() {
        s.clamp(0.1, 8.0)
    } else {
        1.0
    }
}

pub fn format_hms(d: Duration) -> String {
    let t = d.as_secs();
    let (h, m, s) = (t / 3600, (t % 3600) / 60, t % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frisch_gestartet_ist_das_erste_bild_faellig() {
        let c = Clock::new(1.0);
        assert!(c.lateness(Duration::ZERO).is_some());
        // Ein Bild in ferner Zukunft ist es nicht.
        assert!(c.lateness(Duration::from_secs(10)).is_none());
    }

    #[test]
    fn doppeltes_tempo_halbiert_die_wartezeit() {
        let c = Clock::new(2.0);
        let ziel = c.target(Duration::from_secs(4));
        let spanne = ziel.saturating_duration_since(c.anchor_wall);
        assert!(
            (spanne.as_secs_f64() - 2.0).abs() < 0.01,
            "erwartet 2s, war {:?}",
            spanne
        );
    }

    #[test]
    fn pause_haelt_die_position_an() {
        let mut c = Clock::new(1.0);
        c.set_paused(true);
        let a = c.media_now();
        std::thread::sleep(Duration::from_millis(60));
        let b = c.media_now();
        assert_eq!(a, b, "in der Pause darf die Position nicht laufen");
    }

    #[test]
    fn nach_der_pause_laeuft_es_ohne_sprung_weiter() {
        let mut c = Clock::new(1.0);
        std::thread::sleep(Duration::from_millis(40));
        let vor = c.media_now();
        c.set_paused(true);
        std::thread::sleep(Duration::from_millis(120));
        c.set_paused(false);
        let nach = c.media_now();
        let sprung = nach.saturating_sub(vor);
        assert!(
            sprung < Duration::from_millis(40),
            "Pause wurde nicht herausgerechnet, Sprung war {sprung:?}"
        );
    }

    #[test]
    fn tempowechsel_springt_nicht() {
        let mut c = Clock::new(1.0);
        std::thread::sleep(Duration::from_millis(50));
        let vor = c.media_now();
        c.set_speed(4.0);
        let nach = c.media_now();
        let diff = nach.saturating_sub(vor);
        assert!(diff < Duration::from_millis(10), "Sprung von {diff:?}");
        assert_eq!(c.speed(), 4.0);
    }

    #[test]
    fn spulen_setzt_die_position_neu() {
        let mut c = Clock::new(1.0);
        c.seek_to(Duration::from_secs(90));
        assert!(c.media_now() >= Duration::from_secs(90));
        // Das Bild an der neuen Position ist sofort fällig.
        assert!(c.lateness(Duration::from_secs(90)).is_some());
        assert!(c.lateness(Duration::from_secs(95)).is_none());
    }

    #[test]
    fn in_der_pause_gilt_nichts_als_ueberfaellig() {
        let mut c = Clock::new(1.0);
        c.set_paused(true);
        std::thread::sleep(Duration::from_millis(30));
        assert!(
            c.lateness(Duration::ZERO).is_none(),
            "sonst würde die Pause alle Bilder verwerfen"
        );
    }

    #[test]
    fn unsinniges_tempo_wird_gekappt() {
        assert_eq!(Clock::new(0.0).speed(), 0.1);
        assert_eq!(Clock::new(-3.0).speed(), 0.1);
        assert_eq!(Clock::new(f64::NAN).speed(), 1.0);
        assert_eq!(Clock::new(1000.0).speed(), 8.0);
    }

    #[test]
    fn zeitformat_ist_lesbar() {
        assert_eq!(format_hms(Duration::from_secs(9)), "0:09");
        assert_eq!(format_hms(Duration::from_secs(75)), "1:15");
        assert_eq!(format_hms(Duration::from_secs(3725)), "1:02:05");
    }
}
