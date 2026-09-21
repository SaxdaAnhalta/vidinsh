//! Tonwiedergabe: ffmpeg liefert rohes PCM, cpal gibt es aus.
//!
//! Vorher lief das über einen ffplay-Prozess. Das hatte zwei Nachteile, die
//! beide hier verschwinden:
//!
//! * ffplay kennt keine Steuerung von außen. Pause, Spulen und Lautstärke
//!   mussten den Prozess neu starten -- hörbar als Lücke. Jetzt sind es
//!   Zahlen in einem gemeinsamen Zustand und wirken sofort.
//! * ffplay ist eine eigene Programmdatei von gut 230 MB. Für eine Fassung,
//!   die ffmpeg mitbringt, hätte sie die Größe fast verdoppelt.
//!
//! Der Aufbau ist ein Ringpuffer zwischen zwei Seiten: ein Lesethread schiebt
//! das PCM aus der ffmpeg-Pipe hinein, der Audio-Rückruf der Soundkarte holt
//! es heraus. Läuft der Puffer voll, blockiert der Lesethread -- das ist die
//! Bremse, die verhindert, dass ffmpeg das ganze Stück in den Speicher lädt.

use crate::source::input::{Input, Kind};
use crate::source::tools;
use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::collections::VecDeque;
use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Wie viel Ton höchstens vorgehalten wird. Eine Sekunde federt Aussetzer ab,
/// ohne dass die Pause spürbar nachläuft.
const PUFFER_SEKUNDEN: f32 = 1.0;

/// Was sich Lesethread, Audio-Rückruf und Hauptschleife teilen.
struct Shared {
    ring: Mutex<VecDeque<f32>>,
    /// ausgegebene Bildrahmen (nicht Einzelwerte), also Zeit * Rate
    gespielt: AtomicU64,
    pause: AtomicBool,
    /// 0..100
    volume: AtomicU32,
    /// Der Lesethread soll sich beenden.
    stopp: AtomicBool,
    kapazitaet: usize,
}

impl Shared {
    fn lautstaerke(&self) -> f32 {
        // Wahrnehmungsnäher als linear: bei 50 % soll es halb so laut
        // *klingen*, nicht halbe Amplitude haben.
        let v = self.volume.load(Ordering::Relaxed) as f32 / 100.0;
        v * v
    }
}

pub struct Audio {
    /// Muss am Leben bleiben -- beim Fallenlassen endet die Wiedergabe.
    _stream: cpal::Stream,
    shared: Arc<Shared>,
    child: Arc<Mutex<Option<Child>>>,
    input: Input,
    rate: u32,
    kanaele: u16,
    /// Medienposition, an der dieser Abschnitt begann
    basis: f64,
}

/// `None`, wenn für diese Quelle kein Ton in Frage kommt oder die Soundkarte
/// sich nicht öffnen lässt. Ton ist Beiwerk -- ohne ihn läuft das Bild weiter.
pub fn start(input: &Input, from: f64, volume: u32) -> Option<Audio> {
    if matches!(input.kind, Kind::Camera | Kind::Stdin) {
        // Die Kamera liefert keinen Ton, und stdin lässt sich nicht zweimal
        // lesen -- ein zweiter Prozess würde dem Bild die Daten wegnehmen.
        return None;
    }
    Audio::neu(input, from, volume).ok()
}

/// Wie `start`, aber mit Begründung -- für `--verbose`.
pub fn start_verbose(input: &Input, from: f64, volume: u32) -> Result<Option<Audio>> {
    if matches!(input.kind, Kind::Camera | Kind::Stdin) {
        return Ok(None);
    }
    Audio::neu(input, from, volume).map(Some)
}

impl Audio {
    fn neu(input: &Input, from: f64, volume: u32) -> Result<Audio> {
        let host = cpal::default_host();
        let geraet = host
            .default_output_device()
            .context("Keine Tonausgabe gefunden")?;
        let vorgabe = geraet
            .default_output_config()
            .context("Tonausgabe meldet keine brauchbare Einstellung")?;

        let rate = vorgabe.sample_rate();
        let kanaele = vorgabe.channels();
        let format = vorgabe.sample_format();
        let config: cpal::StreamConfig = vorgabe.into();

        let kapazitaet = (rate as f32 * PUFFER_SEKUNDEN) as usize * kanaele as usize;
        let shared = Arc::new(Shared {
            ring: Mutex::new(VecDeque::with_capacity(kapazitaet)),
            gespielt: AtomicU64::new(0),
            pause: AtomicBool::new(false),
            volume: AtomicU32::new(volume.min(100)),
            stopp: AtomicBool::new(false),
            kapazitaet,
        });

        let stream = baue_stream(&geraet, config, format, Arc::clone(&shared), kanaele)?;
        stream
            .play()
            .context("Tonausgabe lässt sich nicht starten")?;

        let audio = Audio {
            _stream: stream,
            shared,
            child: Arc::new(Mutex::new(None)),
            input: input.clone(),
            rate,
            kanaele,
            basis: from,
        };
        audio.fuettern(from);
        Ok(audio)
    }

    /// Startet ffmpeg und den Lesethread für einen Abschnitt ab `from`.
    fn fuettern(&self, from: f64) {
        self.kill();
        self.shared.ring.lock().unwrap().clear();
        self.shared.gespielt.store(0, Ordering::SeqCst);
        self.shared.stopp.store(false, Ordering::SeqCst);

        let quelle = self
            .input
            .audio_input
            .as_deref()
            .unwrap_or(&self.input.ffmpeg_input);

        let mut c = Command::new(tools::ffmpeg());
        c.args(["-hide_banner", "-loglevel", "error", "-nostdin"]);
        c.args(&self.input.pre_args);
        if from > 0.0 {
            c.args(["-ss", &format!("{from:.3}")]);
        }
        c.args(["-i", quelle]);
        c.args(["-vn", "-sn", "-dn"]);
        c.args(["-f", "f32le", "-acodec", "pcm_f32le"]);
        c.args(["-ar", &self.rate.to_string()]);
        c.args(["-ac", &self.kanaele.to_string()]);
        c.arg("pipe:1");

        let Ok(mut kind) = c
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
        else {
            return;
        };
        let Some(mut aus) = kind.stdout.take() else {
            return;
        };
        *self.child.lock().unwrap() = Some(kind);

        let shared = Arc::clone(&self.shared);
        std::thread::spawn(move || {
            // Vier Bytes je Wert; der Puffer ist ein Vielfaches davon, damit
            // nie ein halber Wert übrig bleibt.
            let mut roh = vec![0u8; 16 * 1024];
            let mut rest = Vec::new();
            loop {
                if shared.stopp.load(Ordering::Relaxed) {
                    break;
                }
                let n = match aus.read(&mut roh) {
                    Ok(0) => break,
                    Ok(n) => n,
                    Err(_) => break,
                };
                rest.extend_from_slice(&roh[..n]);
                let ganze = rest.len() / 4 * 4;

                // Warten, bis Platz ist -- das bremst ffmpeg auf Abspieltempo.
                loop {
                    if shared.stopp.load(Ordering::Relaxed) {
                        return;
                    }
                    let frei = {
                        let r = shared.ring.lock().unwrap();
                        shared.kapazitaet.saturating_sub(r.len())
                    };
                    if frei >= ganze / 4 {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }

                let mut r = shared.ring.lock().unwrap();
                for w in rest[..ganze].chunks_exact(4) {
                    r.push_back(f32::from_le_bytes([w[0], w[1], w[2], w[3]]));
                }
                drop(r);
                rest.drain(..ganze);
            }
        });
    }

    fn kill(&self) {
        self.shared.stopp.store(true, Ordering::SeqCst);
        if let Some(mut c) = self.child.lock().unwrap().take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }

    /// Abspielposition im Medium. Das ist die verlässlichste Uhr, die es hier
    /// gibt: sie zählt, was die Soundkarte tatsächlich abgeholt hat.
    pub fn position(&self) -> f64 {
        self.basis + self.shared.gespielt.load(Ordering::Relaxed) as f64 / self.rate as f64
    }

    pub fn set_paused(&mut self, pause: bool) {
        self.shared.pause.store(pause, Ordering::SeqCst);
    }

    pub fn set_volume(&mut self, volume: u32) {
        self.shared.volume.store(volume.min(100), Ordering::Relaxed);
    }

    /// Nach dem Spulen an der neuen Stelle neu ansetzen.
    pub fn seek(&mut self, pos: f64) {
        self.basis = pos;
        self.fuettern(pos);
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

/// Baut den Ausgabestrom für das Format, das die Soundkarte will.
fn baue_stream(
    geraet: &cpal::Device,
    config: cpal::StreamConfig,
    format: cpal::SampleFormat,
    shared: Arc<Shared>,
    kanaele: u16,
) -> Result<cpal::Stream> {
    let fehler = |e| eprintln!("Tonausgabe: {e}");

    macro_rules! strom {
        ($typ:ty, $wandeln:expr) => {{
            let sh = Arc::clone(&shared);
            geraet.build_output_stream(
                config.clone(),
                move |aus: &mut [$typ], _: &cpal::OutputCallbackInfo| {
                    fuellen(&sh, kanaele, aus.len(), $wandeln, aus);
                },
                fehler,
                None,
            )
        }};
    }

    let s = match format {
        cpal::SampleFormat::F32 => strom!(f32, |v: f32| v),
        cpal::SampleFormat::I16 => strom!(i16, |v: f32| (v.clamp(-1.0, 1.0) * 32767.0) as i16),
        cpal::SampleFormat::U16 => {
            strom!(u16, |v: f32| ((v.clamp(-1.0, 1.0) * 0.5 + 0.5) * 65535.0)
                as u16)
        }
        anderes => anyhow::bail!("Tonformat {anderes:?} wird nicht unterstützt"),
    };
    s.context("Tonausgabe lässt sich nicht einrichten")
}

/// Der eigentliche Rückruf. Läuft im Audio-Thread und muss zügig sein --
/// deshalb wird der Ring nur einmal kurz gesperrt und sonst nichts getan.
fn fuellen<T: Copy>(
    shared: &Shared,
    kanaele: u16,
    laenge: usize,
    wandeln: impl Fn(f32) -> T,
    aus: &mut [T],
) {
    let stumm = wandeln(0.0);
    if shared.pause.load(Ordering::Relaxed) {
        aus.fill(stumm);
        return;
    }

    let lautstaerke = shared.lautstaerke();
    let mut geholt = 0usize;
    {
        let mut r = shared.ring.lock().unwrap();
        while geholt < laenge {
            match r.pop_front() {
                Some(v) => {
                    aus[geholt] = wandeln(v * lautstaerke);
                    geholt += 1;
                }
                None => break,
            }
        }
    }
    // Leergelaufen: Rest mit Stille auffüllen, statt zu knacken.
    aus[geholt..].fill(stumm);

    // Nur zählen, was wirklich Ton war -- sonst liefe die Uhr in einer
    // Pufferunterdeckung davon und das Bild zöge nach.
    shared
        .gespielt
        .fetch_add((geholt / kanaele.max(1) as usize) as u64, Ordering::Relaxed);
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

    fn shared(kapazitaet: usize) -> Shared {
        Shared {
            ring: Mutex::new(VecDeque::new()),
            gespielt: AtomicU64::new(0),
            pause: AtomicBool::new(false),
            volume: AtomicU32::new(100),
            stopp: AtomicBool::new(false),
            kapazitaet,
        }
    }

    #[test]
    fn lautstaerke_ist_wahrnehmungsnah_und_begrenzt() {
        let s = shared(16);
        assert_eq!(s.lautstaerke(), 1.0);
        s.volume.store(0, Ordering::Relaxed);
        assert_eq!(s.lautstaerke(), 0.0);
        s.volume.store(50, Ordering::Relaxed);
        let halb = s.lautstaerke();
        assert!(halb > 0.0 && halb < 0.5, "erwartet quadratisch, war {halb}");
    }

    #[test]
    fn pause_gibt_stille_und_haelt_die_uhr_an() {
        let s = shared(16);
        s.ring.lock().unwrap().extend([0.5f32; 8]);
        s.pause.store(true, Ordering::SeqCst);

        let mut aus = [9.0f32; 8];
        fuellen(&s, 2, 8, |v| v, &mut aus);
        assert_eq!(aus, [0.0; 8], "in der Pause muss Stille kommen");
        assert_eq!(s.gespielt.load(Ordering::Relaxed), 0);
        assert_eq!(s.ring.lock().unwrap().len(), 8, "nichts verbrauchen");
    }

    #[test]
    fn leerlauf_fuellt_mit_stille_statt_zu_knacken() {
        let s = shared(16);
        s.ring.lock().unwrap().extend([1.0f32; 4]);
        let mut aus = [9.0f32; 8];
        fuellen(&s, 2, 8, |v| v, &mut aus);
        assert_eq!(&aus[..4], &[1.0; 4]);
        assert_eq!(&aus[4..], &[0.0; 4], "Rest muss still sein");
        // Nur die vier echten Werte zaehlen, sonst laeuft die Uhr davon.
        assert_eq!(s.gespielt.load(Ordering::Relaxed), 2, "4 Werte / 2 Kanäle");
    }

    #[test]
    fn die_uhr_zaehlt_rahmen_nicht_einzelwerte() {
        let s = shared(64);
        s.ring.lock().unwrap().extend([0.25f32; 32]);
        let mut aus = [0.0f32; 32];
        fuellen(&s, 2, 32, |v| v, &mut aus);
        assert_eq!(s.gespielt.load(Ordering::Relaxed), 16);
    }

    #[test]
    fn lautstaerke_wirkt_auf_die_werte() {
        let s = shared(16);
        s.volume.store(50, Ordering::Relaxed);
        s.ring.lock().unwrap().extend([1.0f32; 4]);
        let mut aus = [0.0f32; 4];
        fuellen(&s, 2, 4, |v| v, &mut aus);
        assert!(aus[0] > 0.0 && aus[0] < 1.0, "war {}", aus[0]);
    }

    #[test]
    fn ganzzahlformate_werden_gewandelt() {
        let s = shared(16);
        s.ring.lock().unwrap().extend([1.0f32, -1.0, 0.0, 2.0]);
        let mut aus = [0i16; 4];
        fuellen(
            &s,
            2,
            4,
            |v: f32| (v.clamp(-1.0, 1.0) * 32767.0) as i16,
            &mut aus,
        );
        assert_eq!(aus[0], 32767);
        assert_eq!(aus[1], -32767);
        assert_eq!(aus[2], 0);
        assert_eq!(aus[3], 32767, "Übersteuerung muss begrenzt werden");
    }
}
