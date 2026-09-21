//! Quellen: ffmpeg als Kindprozess, Erkennung der Eingabeart, ffprobe.

pub mod ffmpeg;
pub mod input;
pub mod probe;

use crate::render::geometry::Layout;
use std::time::Duration;

/// Ein dekodiertes Bild in rgb24, bereits auf das Supersample-Raster skaliert.
#[derive(Clone)]
pub struct Frame {
    pub w: u32,
    pub h: u32,
    /// `w * h * 3` Bytes
    pub data: Vec<u8>,
    /// laufende Nummer seit dem letzten ffmpeg-Start
    pub index: u64,
}

impl Frame {
    /// Position im Video. Wir zwingen ffmpeg zu konstanter Bildrate, deshalb
    /// ist der Zeitstempel schlicht die Bildnummer -- kein PTS-Parsen nötig.
    pub fn pts(&self, fps: f64) -> Duration {
        if fps > 0.0 {
            Duration::from_secs_f64(self.index as f64 / fps)
        } else {
            Duration::ZERO
        }
    }

    /// Passt das Bild zum aktuellen Raster? Nach einem Resize sind noch Frames
    /// der alten Größe unterwegs; die zu rendern ergäbe Pixelsalat.
    pub fn matches(&self, layout: &Layout) -> bool {
        self.w == layout.px_w && self.h == layout.px_h
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::geometry::{self, Fit};

    fn frame(w: u32, h: u32) -> Frame {
        Frame {
            w,
            h,
            data: vec![0; (w * h * 3) as usize],
            index: 0,
        }
    }

    #[test]
    fn zeitstempel_folgt_der_bildnummer() {
        let mut f = frame(4, 4);
        f.index = 30;
        assert_eq!(f.pts(30.0), Duration::from_secs(1));
    }

    #[test]
    fn nullrate_ergibt_nullzeit_statt_division_durch_null() {
        let f = frame(4, 4);
        assert_eq!(f.pts(0.0), Duration::ZERO);
    }

    #[test]
    fn frame_erkennt_veraltete_groesse() {
        let l = geometry::compute(100, 50, 1.0, 2.0, Fit::Stretch, (4, 8), None);
        assert!(frame(l.px_w, l.px_h).matches(&l));
        assert!(!frame(l.px_w, l.px_h + 8).matches(&l));
    }
}
