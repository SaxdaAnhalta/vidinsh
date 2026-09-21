//! Farbräume, Quantisierung und ANSI-Farbcodes.
//!
//! Alle Mittelungen im Renderer laufen in linearem Licht, nicht auf sRGB-Werten.
//! Wer acht sRGB-Bytes addiert und durch acht teilt, bekommt ein zu dunkles
//! Ergebnis -- bei Videomaterial mit weichen Verläufen ist das gut sichtbar.

use std::sync::LazyLock;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Rgb(pub u8, pub u8, pub u8);

#[derive(Clone, Copy, PartialEq, Eq, Debug, clap::ValueEnum)]
pub enum ColorMode {
    /// 24-Bit, ESC[38;2;r;g;bm
    Truecolor,
    /// 256er-Palette, ESC[38;5;nm
    #[value(name = "256")]
    Ansi256,
    /// 16 Grundfarben, ESC[30-37m / ESC[90-97m
    #[value(name = "16")]
    Ansi16,
    /// gar keine Farbe
    Mono,
}

impl ColorMode {
    pub fn label(self) -> &'static str {
        match self {
            ColorMode::Truecolor => "truecolor",
            ColorMode::Ansi256 => "256",
            ColorMode::Ansi16 => "16",
            ColorMode::Mono => "mono",
        }
    }

    /// Reihenfolge für die Taste `c`.
    pub fn next(self) -> Self {
        match self {
            ColorMode::Truecolor => ColorMode::Ansi256,
            ColorMode::Ansi256 => ColorMode::Ansi16,
            ColorMode::Ansi16 => ColorMode::Mono,
            ColorMode::Mono => ColorMode::Truecolor,
        }
    }
}

// ---------------------------------------------------------------- Gammakurven

/// sRGB-Byte -> lineares Licht.
pub static TO_LINEAR: LazyLock<[f32; 256]> = LazyLock::new(|| {
    std::array::from_fn(|i| {
        let c = i as f32 / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    })
});

const REV: usize = 4096;

/// Lineares Licht -> sRGB-Byte. Tabelle statt `powf`, weil das pro Zelle und
/// Kanal einmal anfällt -- bei 200x60 sind das 36000 Aufrufe pro Frame.
static TO_SRGB: LazyLock<[u8; REV + 1]> = LazyLock::new(|| {
    std::array::from_fn(|i| {
        let c = i as f32 / REV as f32;
        let s = if c <= 0.0031308 {
            c * 12.92
        } else {
            1.055 * c.powf(1.0 / 2.4) - 0.055
        };
        (s * 255.0 + 0.5).clamp(0.0, 255.0) as u8
    })
});

#[inline(always)]
pub fn to_linear(c: u8) -> f32 {
    TO_LINEAR[c as usize]
}

#[inline(always)]
pub fn to_srgb(v: f32) -> u8 {
    TO_SRGB[(v.clamp(0.0, 1.0) * REV as f32) as usize]
}

/// Rec.709-Luminanz, erwartet lineares Licht.
#[inline(always)]
pub fn luma_linear(r: f32, g: f32, b: f32) -> f32 {
    0.2126 * r + 0.7152 * g + 0.0722 * b
}

// ------------------------------------------------------------- Quantisierung

const CUBE: [u8; 6] = [0, 95, 135, 175, 215, 255];

#[inline]
fn cube_idx(v: u8) -> usize {
    let mut best = 0usize;
    let mut bd = i32::MAX;
    for (i, &lv) in CUBE.iter().enumerate() {
        let d = (lv as i32 - v as i32).abs();
        if d < bd {
            bd = d;
            best = i;
        }
    }
    best
}

#[inline]
fn dist2(a: Rgb, b: Rgb) -> i32 {
    let dr = a.0 as i32 - b.0 as i32;
    let dg = a.1 as i32 - b.1 as i32;
    let db = a.2 as i32 - b.2 as i32;
    dr * dr + dg * dg + db * db
}

/// Nächster Index in der 256er-Palette: 6x6x6-Würfel (16..231) oder
/// Graustufenrampe (232..255), je nachdem was näher liegt.
pub fn to_256(c: Rgb) -> u8 {
    let (ri, gi, bi) = (cube_idx(c.0), cube_idx(c.1), cube_idx(c.2));
    let cube_err = dist2(c, Rgb(CUBE[ri], CUBE[gi], CUBE[bi]));

    let avg = (c.0 as i32 + c.1 as i32 + c.2 as i32) / 3;
    let gidx = (((avg - 8) + 5) / 10).clamp(0, 23);
    let gv = (8 + 10 * gidx) as u8;
    let gray_err = dist2(c, Rgb(gv, gv, gv));

    if gray_err < cube_err {
        232 + gidx as u8
    } else {
        (16 + 36 * ri + 6 * gi + bi) as u8
    }
}

/// Die kanonische 16er-Palette. Terminals weichen davon ab, aber als Zielpunkt
/// für die Nächster-Nachbar-Suche ist sie brauchbar.
const ANSI16: [Rgb; 16] = [
    Rgb(0, 0, 0),
    Rgb(128, 0, 0),
    Rgb(0, 128, 0),
    Rgb(128, 128, 0),
    Rgb(0, 0, 128),
    Rgb(128, 0, 128),
    Rgb(0, 128, 128),
    Rgb(192, 192, 192),
    Rgb(128, 128, 128),
    Rgb(255, 0, 0),
    Rgb(0, 255, 0),
    Rgb(255, 255, 0),
    Rgb(0, 0, 255),
    Rgb(255, 0, 255),
    Rgb(0, 255, 255),
    Rgb(255, 255, 255),
];

pub fn to_16(c: Rgb) -> u8 {
    let mut best = 0u8;
    let mut bd = i32::MAX;
    for (i, &p) in ANSI16.iter().enumerate() {
        let d = dist2(c, p);
        if d < bd {
            bd = d;
            best = i as u8;
        }
    }
    best
}

// ------------------------------------------------------------------ Dithering

/// Geordnete Bayer-Matrix 8x8. Werte 0..63 in der Reihenfolge, die das
/// typische Kreuzmuster ergibt.
#[rustfmt::skip]
const BAYER8: [u8; 64] = [
     0, 32,  8, 40,  2, 34, 10, 42,
    48, 16, 56, 24, 50, 18, 58, 26,
    12, 44,  4, 36, 14, 46,  6, 38,
    60, 28, 52, 20, 62, 30, 54, 22,
     3, 35, 11, 43,  1, 33,  9, 41,
    51, 19, 59, 27, 49, 17, 57, 25,
    15, 47,  7, 39, 13, 45,  5, 37,
    63, 31, 55, 23, 61, 29, 53, 21,
];

/// Wie weit eine Farbe vor der Quantisierung verschoben werden darf, grob die
/// halbe Stufenbreite der jeweiligen Palette. Bei Truecolor wird nichts
/// quantisiert, also gibt es auch nichts zu verteilen.
fn dither_amplitude(mode: ColorMode) -> f32 {
    match mode {
        ColorMode::Truecolor | ColorMode::Mono => 0.0,
        ColorMode::Ansi256 => 36.0,
        ColorMode::Ansi16 => 80.0,
    }
}

/// Verschiebt eine Farbe ortsabhängig, damit der Quantisierungsfehler sich
/// über die Fläche verteilt statt als Streifen sichtbar zu werden.
///
/// Die Verschiebung hängt nur an der Zellposition, nicht am Bildinhalt --
/// eine unveränderte Zelle ergibt deshalb weiterhin dieselben Bytes und das
/// Diffing bleibt wirksam.
pub fn dither(c: Rgb, mode: ColorMode, x: usize, y: usize) -> Rgb {
    let amp = dither_amplitude(mode);
    if amp == 0.0 {
        return c;
    }
    let schwelle = BAYER8[(y % 8) * 8 + (x % 8)] as f32 / 64.0 - 0.5;
    let d = schwelle * amp;
    let shift = |v: u8| (v as f32 + d).clamp(0.0, 255.0) as u8;
    Rgb(shift(c.0), shift(c.1), shift(c.2))
}

// ----------------------------------------------------------------- SGR-Codes

/// Dezimalzahl direkt als Bytes anhängen. `format!` wäre hier pro Zelle ein
/// Heap-Allokat -- das summiert sich bei 12000 Zellen pro Frame.
#[inline(always)]
fn push_num(out: &mut Vec<u8>, n: u8) {
    if n >= 100 {
        out.push(b'0' + n / 100);
        out.push(b'0' + (n / 10) % 10);
        out.push(b'0' + n % 10);
    } else if n >= 10 {
        out.push(b'0' + n / 10);
        out.push(b'0' + n % 10);
    } else {
        out.push(b'0' + n);
    }
}

pub fn push_fg(out: &mut Vec<u8>, c: Rgb, mode: ColorMode) {
    match mode {
        ColorMode::Mono => {}
        ColorMode::Truecolor => {
            out.extend_from_slice(b"\x1b[38;2;");
            push_num(out, c.0);
            out.push(b';');
            push_num(out, c.1);
            out.push(b';');
            push_num(out, c.2);
            out.push(b'm');
        }
        ColorMode::Ansi256 => {
            out.extend_from_slice(b"\x1b[38;5;");
            push_num(out, to_256(c));
            out.push(b'm');
        }
        ColorMode::Ansi16 => {
            let i = to_16(c);
            out.extend_from_slice(b"\x1b[");
            push_num(out, if i < 8 { 30 + i } else { 82 + i });
            out.push(b'm');
        }
    }
}

pub fn push_bg(out: &mut Vec<u8>, c: Rgb, mode: ColorMode) {
    match mode {
        ColorMode::Mono => {}
        ColorMode::Truecolor => {
            out.extend_from_slice(b"\x1b[48;2;");
            push_num(out, c.0);
            out.push(b';');
            push_num(out, c.1);
            out.push(b';');
            push_num(out, c.2);
            out.push(b'm');
        }
        ColorMode::Ansi256 => {
            out.extend_from_slice(b"\x1b[48;5;");
            push_num(out, to_256(c));
            out.push(b'm');
        }
        ColorMode::Ansi16 => {
            let i = to_16(c);
            out.extend_from_slice(b"\x1b[");
            push_num(out, if i < 8 { 40 + i } else { 92 + i });
            out.push(b'm');
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gammakurven_sind_zueinander_invers() {
        for v in [0u8, 1, 17, 64, 128, 200, 254, 255] {
            let back = to_srgb(to_linear(v));
            assert!(
                (back as i32 - v as i32).abs() <= 1,
                "{v} -> {back} weicht zu stark ab"
            );
        }
    }

    #[test]
    fn palette_trifft_eigene_farben_exakt() {
        assert_eq!(to_256(Rgb(255, 0, 0)), 196);
        assert_eq!(to_256(Rgb(0, 0, 0)), 16);
        assert_eq!(to_256(Rgb(255, 255, 255)), 231);
        assert_eq!(to_16(Rgb(255, 0, 0)), 9);
        assert_eq!(to_16(Rgb(0, 0, 0)), 0);
    }

    #[test]
    fn sgr_16_nutzt_den_hellen_bereich() {
        let mut out = Vec::new();
        push_fg(&mut out, Rgb(255, 0, 0), ColorMode::Ansi16);
        assert_eq!(out, b"\x1b[91m"); // 9 -> 82+9
        out.clear();
        push_bg(&mut out, Rgb(255, 0, 0), ColorMode::Ansi16);
        assert_eq!(out, b"\x1b[101m"); // 9 -> 92+9
    }

    #[test]
    fn dithering_laesst_truecolor_und_mono_unberuehrt() {
        let c = Rgb(100, 150, 200);
        for m in [ColorMode::Truecolor, ColorMode::Mono] {
            for (x, y) in [(0, 0), (3, 5), (7, 7)] {
                assert_eq!(dither(c, m, x, y), c);
            }
        }
    }

    #[test]
    fn dithering_verschiebt_benachbarte_zellen_unterschiedlich() {
        let c = Rgb(120, 120, 120);
        let a = dither(c, ColorMode::Ansi256, 0, 0);
        let b = dither(c, ColorMode::Ansi256, 1, 0);
        assert_ne!(
            a, b,
            "sonst entstehen genau die Streifen, die es vermeiden soll"
        );
    }

    #[test]
    fn dithering_ist_ortsfest() {
        // Voraussetzung dafür, dass das Diffing im Writer weiter greift.
        let c = Rgb(77, 88, 99);
        assert_eq!(
            dither(c, ColorMode::Ansi256, 5, 3),
            dither(c, ColorMode::Ansi256, 5, 3)
        );
        assert_eq!(
            dither(c, ColorMode::Ansi256, 5, 3),
            dither(c, ColorMode::Ansi256, 13, 11),
            "die Matrix wiederholt sich alle 8 Zellen"
        );
    }

    #[test]
    fn dithering_laeuft_an_den_raendern_nicht_ueber() {
        // Die Verschiebung wird geklemmt, nicht gerechnet: Schwarz darf nicht
        // nach Weiß umschlagen und umgekehrt.
        let amp = dither_amplitude(ColorMode::Ansi16);
        for (x, y) in [(0, 0), (7, 7), (4, 2), (3, 6)] {
            let schwarz = dither(Rgb(0, 0, 0), ColorMode::Ansi16, x, y);
            let weiss = dither(Rgb(255, 255, 255), ColorMode::Ansi16, x, y);
            assert!(
                (schwarz.0 as f32) <= amp / 2.0,
                "Schwarz bei ({x},{y}) auf {} gesprungen",
                schwarz.0
            );
            assert!(
                (weiss.0 as f32) >= 255.0 - amp / 2.0,
                "Weiß bei ({x},{y}) auf {} gefallen",
                weiss.0
            );
        }
    }

    #[test]
    fn bayer_matrix_ist_eine_permutation() {
        let mut v = BAYER8.to_vec();
        v.sort_unstable();
        assert_eq!(v, (0u8..64).collect::<Vec<_>>());
    }

    #[test]
    fn mono_schreibt_nichts() {
        let mut out = Vec::new();
        push_fg(&mut out, Rgb(1, 2, 3), ColorMode::Mono);
        push_bg(&mut out, Rgb(1, 2, 3), ColorMode::Mono);
        assert!(out.is_empty());
    }
}
