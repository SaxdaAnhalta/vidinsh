//! vidinsh -- Video als farbiges ASCII im Terminal.

mod audio;
mod cli;
mod control;
mod render;
mod source;
mod term;

use anyhow::{Context, Result, bail};
use clap::Parser;
use cli::Args;
use control::{Cmd, clock::Clock};
use crossbeam_channel::{Receiver, bounded, unbounded};
use render::geometry::{self, Layout};
use render::{Charset, Grid, Mode, RenderOpts, Renderer, color::ColorMode};
use source::Frame;
use source::ffmpeg::{self, FfmpegSource};
use source::input::{self, Kind};
use source::probe::{self, MediaInfo};
use std::io::Write;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use term::caps::Caps;

/// Wie lange ffprobe und yt-dlp höchstens brauchen dürfen.
const PROBE_LIMIT: Duration = Duration::from_secs(20);

/// Wie lange auf das erste Bild nach einem ffmpeg-Neustart gewartet wird,
/// bevor auf sequenzielles Spulen umgestellt wird. Manche Server -- jede
/// aufgelöste YouTube-Adresse -- beantworten die Range-Anfrage nicht,
/// sondern lassen die Verbindung offen stehen: ffmpeg wartet dann ewig.
const SPUL_GEDULD: Duration = Duration::from_secs(5);

/// Harte Obergrenze. Kommt bis dahin nichts, wird abgebrochen statt
/// unbegrenzt ein stehendes Bild zu zeigen.
const WARTE_GRENZE: Duration = Duration::from_secs(90);

/// Ab welcher Abweichung vom Ton die Bild-Uhr nachgezogen wird. Kleiner wäre
/// unruhig -- Puffergrößen schwanken ohnehin um einige Zehntelsekunden --,
/// größer wäre als Versatz zwischen Bild und Ton wahrnehmbar.
const TON_DRIFT: f64 = 0.15;

fn main() -> ExitCode {
    term::install_panic_hook();
    let args = Args::parse();

    match run(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            term::restore();
            eprintln!("Fehler: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: Args) -> Result<()> {
    args.validate()?;

    if args.list_devices {
        source::tools::init(args.ffmpeg.as_deref())?;
        let geraete = input::video_devices()?;
        if geraete.is_empty() {
            println!("Keine Kameras gefunden.");
        } else {
            println!("Verfügbare Kameras:");
            for (i, g) in geraete.iter().enumerate() {
                println!("  cam:{i}  {g}");
            }
        }
        return Ok(());
    }

    let caps = term::caps::detect(args.color);

    if args.probe {
        return show_probe(&caps);
    }

    // Ab hier wird ffmpeg gebraucht. Einmal festlegen, welches -- bei einer
    // Fassung mit mitgeliefertem ffmpeg wird es hier einmalig entpackt.
    source::tools::init(args.ffmpeg.as_deref())?;
    if args.verbose {
        eprintln!("ffmpeg: {}", source::tools::beschreibung());
    }

    let roh = args.input.clone().expect("clap erzwingt die Eingabe");
    let mut quelle = input::classify(&roh);
    input::resolve_camera(&mut quelle)?;

    if quelle.kind == Kind::Portal {
        let a = input::resolve_portal(&quelle.ffmpeg_input, args.max_height, PROBE_LIMIT)?;
        let label = quelle.label.clone();
        // Die aufgelösten Adressen sind gewöhnliche HTTPS-URLs; classify
        // versieht sie mit den passenden Reconnect-Optionen.
        let audio = a.audio;
        quelle = input::classify(&a.video);
        quelle.audio_input = audio;
        quelle.label = label;
    }

    let info = match probe::probe(&quelle.ffmpeg_input, &quelle.pre_args, PROBE_LIMIT) {
        Ok(i) => i,
        Err(e) if quelle.is_live => {
            // Manche Live-Quellen lassen sich nicht vorab befragen. Wir
            // versuchen es trotzdem und lernen die Maße beim ersten Bild.
            if args.verbose {
                eprintln!("Hinweis: ffprobe ohne Ergebnis ({e}), nehme Standardwerte");
            }
            MediaInfo::fallback()
        }
        Err(e) => return Err(e).context("Quelle lässt sich nicht öffnen"),
    };

    let info = info.mit_tonspur(quelle.audio_input.as_deref());

    play(args, caps, quelle, info)
}

// ------------------------------------------------------------------- --probe

fn show_probe(caps: &Caps) -> Result<()> {
    let mut o = std::io::stdout().lock();
    writeln!(o, "vidinsh --probe\n")?;
    writeln!(o, "  Terminal   {}", caps.terminal)?;
    writeln!(o, "  Farbe      {}", caps.color.label())?;
    writeln!(o, "  Unicode    {}", ja(caps.unicode))?;
    writeln!(o, "  Sync       {}", ja(caps.sync))?;
    writeln!(o, "  TTY        {}", ja(caps.is_tty))?;
    for n in &caps.notes {
        writeln!(o, "  Hinweis    {n}")?;
    }

    writeln!(o, "\n  Farbverlauf in allen Modi:")?;
    for m in [
        ColorMode::Truecolor,
        ColorMode::Ansi256,
        ColorMode::Ansi16,
        ColorMode::Mono,
    ] {
        let mut zeile = Vec::new();
        for i in 0..48 {
            let t = i as f32 / 47.0;
            let c = render::color::Rgb(
                (255.0 * t) as u8,
                (255.0 * (1.0 - (t - 0.5).abs() * 2.0)) as u8,
                (255.0 * (1.0 - t)) as u8,
            );
            render::color::push_bg(&mut zeile, c, m);
            zeile.push(b' ');
        }
        zeile.extend_from_slice(b"\x1b[0m");
        write!(o, "  {:<10} ", m.label())?;
        o.write_all(&zeile)?;
        writeln!(o)?;
    }

    writeln!(
        o,
        "\n  Zeichensätze -- was hier als Kästchen erscheint, fehlt der Schrift:"
    )?;
    let proben: [(&str, &str); 5] = [
        ("ascii", " .:-=+*#%@"),
        ("extended", " .:-=+*#%@░▒▓█"),
        ("half", "▀▄█ ▀▄█ ▀▄█"),
        ("quad", "▘▝▖▗▚▞▙▟▛▜█"),
        ("sextant", "\u{1FB00}\u{1FB10}\u{1FB20}\u{1FB30}\u{1FB3B}"),
    ];
    for (name, s) in proben {
        writeln!(o, "  {name:<10} {s}")?;
    }
    writeln!(o, "  braille    ⠁⠃⠇⠏⠟⠿⡿⣿")?;

    writeln!(
        o,
        "\n  Die Shell selbst spielt keine Rolle -- dieselbe Ausgabe entsteht in\n  \
         cmd, PowerShell und bash. Entscheidend ist das Terminal darum herum."
    )?;
    o.flush()?;
    Ok(())
}

fn ja(b: bool) -> &'static str {
    if b { "ja" } else { "nein" }
}

// --------------------------------------------------------------- Wiedergabe

enum Msg {
    Bild(Box<Frame>),
    /// Strom zu Ende; trägt mit, was ffmpeg zuletzt gemeldet hat
    Ende(Box<source::ffmpeg::Abschluss>),
    Fehler(String),
}

/// Startet ffmpeg und einen Thread, der Bilder in den Kanal schiebt.
/// Der Thread endet von selbst, sobald der Empfänger fallengelassen wird --
/// dabei wird die Quelle aufgeräumt und ffmpeg beendet.
fn start_reader(cfg: &ffmpeg::Config, layout: &Layout) -> Result<(Receiver<Msg>, String)> {
    let mut src = FfmpegSource::spawn(cfg, layout)?;
    let cmdline = src.command_line.clone();
    let (tx, rx) = bounded::<Msg>(3);

    std::thread::spawn(move || {
        loop {
            let m = match src.next_frame() {
                Ok(Some(f)) => Msg::Bild(Box::new(f)),
                Ok(None) => {
                    let _ = tx.send(Msg::Ende(Box::new(src.finish())));
                    break;
                }
                Err(e) => {
                    let _ = tx.send(Msg::Fehler(format!("{e:#}")));
                    break;
                }
            };
            if tx.send(m).is_err() {
                break; // Empfänger weg: Neustart oder Programmende
            }
        }
    });

    Ok((rx, cmdline))
}

fn make_renderer(
    mode: Mode,
    charset: Charset,
    font: Option<&std::path::Path>,
    verbose: bool,
) -> Result<Box<dyn Renderer>> {
    Ok(match mode {
        Mode::Ramp => Box::new(render::ramp::RampRenderer::new()),
        Mode::Blocks => Box::new(render::blocks::BlocksRenderer::new()),
        Mode::Edge => Box::new(render::edge::EdgeRenderer::new()),
        // Nur glyph kann scheitern: dafür wird eine TrueType-Schrift gebraucht.
        Mode::Glyph => {
            let g = render::glyph::GlyphRenderer::new(font, charset)?;
            if verbose {
                eprintln!("glyph: Masken aus {}", g.quelle);
            }
            Box::new(g)
        }
    })
}

struct State {
    mode: Mode,
    color: ColorMode,
    charset: Charset,
    opts: RenderOpts,
    ui: bool,
    volume: u32,
    dither: bool,
}

/// Baut die Statuszeile. Eigene Funktion, weil sie an zwei Stellen gebraucht
/// wird: beim gezeigten Bild und beim Warten auf eines -- ohne die zweite
/// sähe eine lahmende Quelle aus wie ein Absturz.
#[allow(clippy::too_many_arguments)]
fn statuszeile(
    quelle: &input::Input,
    st: &State,
    layout: &Layout,
    clock: &Clock,
    info: &MediaInfo,
    warten: bool,
    ton: bool,
    breite: u16,
    stats: Option<term::ui::Stats>,
) -> String {
    term::ui::render(
        &term::ui::Status {
            quelle: &quelle.label,
            mode: st.mode,
            color: st.color,
            charset: st.charset,
            grid: (layout.grid_w, layout.grid_h),
            pos: clock.media_now(),
            dauer: info.duration,
            paused: clock.is_paused(),
            speed: clock.speed(),
            live: quelle.is_live,
            volume: ton.then_some(st.volume),
            warten,
            stats,
        },
        breite,
    )
}

fn play(args: Args, caps: Caps, quelle: input::Input, info: MediaInfo) -> Result<()> {
    let mut notes = caps.notes.clone();
    let charset = term::caps::resolve_charset(
        args.mode,
        args.effective_charset(),
        caps.unicode,
        &mut notes,
    );

    let ins_terminal = args.write.is_none();
    let (term_w, term_h) = if ins_terminal && caps.is_tty {
        crossterm::terminal::size().unwrap_or((120, 30))
    } else {
        args.size.unwrap_or((120, 30))
    };

    let mut st = State {
        mode: args.mode,
        color: caps.color,
        charset,
        opts: RenderOpts {
            ramp: args.ramp.chars().collect(),
            charset,
            invert: args.invert,
            bg: args.bg,
            brightness: args.brightness,
            contrast: args.contrast,
            saturation: args.saturation,
            auto_contrast: args.auto_contrast,
            edge_threshold: args.edge_threshold,
            edge_sigma: args.edge_sigma,
        },
        ui: !args.no_ui && ins_terminal,
        volume: args.volume,
        dither: args.dither,
    };

    let mut renderer = make_renderer(st.mode, st.charset, args.font.as_deref(), args.verbose)?;
    let fps = args.fps.unwrap_or(info.fps).clamp(0.1, 240.0);
    let frame_dur = Duration::from_secs_f64(1.0 / fps);

    let nutzbare_hoehe = if st.ui {
        term_h.saturating_sub(1).max(1)
    } else {
        term_h
    };
    let mut layout = geometry::compute(
        term_w,
        nutzbare_hoehe,
        info.aspect(),
        args.cell_aspect,
        args.fit,
        args.supersample,
        args.size,
    );

    let mut base = args.ss.unwrap_or(0.0);
    let mut cfg = ffmpeg::Config {
        input: quelle.clone(),
        fps,
        start: args.ss,
        end: args.to,
        scaler: args.scaler,
        fit: args.fit,
        gamma_correct: args.gamma_correct,
        loop_forever: args.loop_forever,
        seekable: true,
    };

    if args.verbose {
        eprintln!("{}", caps.summary());
        for n in &notes {
            eprintln!("Hinweis: {n}");
        }
        // Den echten Pfad zeigen, nicht das Wort "ffmpeg" -- sonst sieht die
        // Zeile bei der mitgelieferten Fassung nach PATH aus, obwohl sie es
        // nicht ist, und man sucht den Fehler an der falschen Stelle.
        eprintln!(
            "{} {}",
            source::tools::ffmpeg().display(),
            ffmpeg::build_args(&cfg, &layout).join(" ")
        );
    }

    let (mut rx, _cmd) = start_reader(&cfg, &layout)?;

    // Ausgabeziel: Terminal oder Datei.
    let sink: Box<dyn Write + Send> =
        match &args.write {
            Some(p) => Box::new(std::fs::File::create(p).with_context(|| {
                format!("Ausgabedatei {} lässt sich nicht anlegen", p.display())
            })?),
            None => Box::new(std::io::stdout()),
        };
    let mut writer = term::writer::Writer::new(sink, caps.sync && ins_terminal, args.bench_naive);

    // Ab hier ist der Terminalzustand verändert; der Guard stellt ihn in
    // jedem Fall wieder her.
    let _guard = if ins_terminal && caps.is_tty && !args.once {
        Some(term::TermGuard::enter()?)
    } else {
        None
    };

    let stop = Arc::new(AtomicBool::new(false));
    let (tx_cmd, rx_cmd) = unbounded::<Cmd>();
    let _eingabe = if _guard.is_some() {
        Some(control::spawn(tx_cmd, Arc::clone(&stop)))
    } else {
        None
    };

    // Ton wird erst mit dem ersten Bild gestartet, nicht schon hier: bis
    // eine Netzquelle das erste Bild liefert, vergehen leicht Sekunden.
    let mut audio: Option<audio::Audio> = None;
    let ton_gewuenscht = args.want_audio() && info.has_audio && _guard.is_some();
    if args.verbose {
        eprintln!(
            "Ton: {} (Quelle hat Tonspur: {}, getrennte Adresse: {}, --no-audio: {})",
            if ton_gewuenscht { "an" } else { "aus" },
            ja(info.has_audio),
            ja(quelle.audio_input.is_some()),
            ja(args.no_audio),
        );
    }

    let mut clock = Clock::new(args.speed);
    clock.seek_to(Duration::from_secs_f64(base));

    // Nach jedem ffmpeg-Start wird die Uhr auf das *erste tatsächlich
    // eingetroffene* Bild gesetzt. Ohne das läuft sie schon, während der
    // Prozess noch anläuft oder eine Netzquelle puffert -- und dann gilt
    // alles, was danach kommt, als überfällig und wird verworfen.
    let mut warte_auf_erstes_bild = true;
    // Zuletzt gesehene Abspielposition des Tons -- daran wird erkannt, ob er
    // überhaupt läuft.
    let mut letzte_tonposition = -1.0f64;
    let mut warten_seit = Instant::now();
    // Wurde für diesen Sprung schon auf sequenzielles Überspulen umgestellt?
    let mut sequenziell_versucht = false;

    let mut grid = Grid::new(layout.grid_w, layout.grid_h);
    let mut dropped: u64 = 0;
    let mut gezeigt: u64 = 0;
    let mut bytes_gesamt: u64 = 0;
    let mut fps_fenster = (Instant::now(), 0u64, 0.0f64);
    let mut term_size = (term_w, term_h);

    'wiedergabe: loop {
        // Kommandos zuerst -- sie sollen auch in der Pause wirken.
        while let Ok(c) = rx_cmd.try_recv() {
            match c {
                Cmd::Quit => break 'wiedergabe,
                Cmd::TogglePause => {
                    clock.toggle_pause();
                    if let Some(a) = &mut audio {
                        a.set_paused(clock.is_paused());
                    }
                }
                Cmd::NextColor => {
                    st.color = st.color.next();
                    writer.force_redraw();
                }
                Cmd::ToggleBg => {
                    st.opts.bg = !st.opts.bg;
                    writer.force_redraw();
                }
                Cmd::ToggleUi => {
                    st.ui = !st.ui;
                    writer.force_redraw();
                    term_size = (0, 0); // Neuberechnung erzwingen
                }
                Cmd::ToggleDither => {
                    st.dither = !st.dither;
                    writer.force_redraw();
                }
                Cmd::SetMode(m) => {
                    let cs = term::caps::resolve_charset(
                        m,
                        args.effective_charset(),
                        caps.unicode,
                        &mut notes,
                    );
                    // Scheitert der Wechsel (glyph ohne Schrift), bleibt der
                    // bisherige Renderer stehen statt die Wiedergabe zu beenden.
                    match make_renderer(m, cs, args.font.as_deref(), args.verbose) {
                        Ok(r) => {
                            renderer = r;
                            st.mode = m;
                            st.charset = cs;
                            st.opts.charset = cs;
                            writer.force_redraw();
                        }
                        Err(e) => notes.push(format!("{e:#}")),
                    }
                }
                Cmd::SpeedStep(f) => clock.set_speed(clock.speed() * f),
                Cmd::VolumeStep(d) => {
                    st.volume = (st.volume as i32 + d).clamp(0, 100) as u32;
                    if let Some(a) = &mut audio {
                        a.set_volume(st.volume);
                    }
                }
                Cmd::Seek(d) => {
                    if quelle.is_live {
                        continue;
                    }
                    let jetzt = clock.media_now().as_secs_f64();
                    let ziel = (jetzt + d).max(0.0);
                    let ziel = match info.duration {
                        Some(dur) => ziel.min((dur - 0.5).max(0.0)),
                        None => ziel,
                    };
                    base = ziel;
                    cfg.start = Some(ziel);
                    let (neu, _) = start_reader(&cfg, &layout)?;
                    rx = neu;
                    clock.seek_to(Duration::from_secs_f64(ziel));
                    warte_auf_erstes_bild = true;
                    warten_seit = Instant::now();
                    sequenziell_versucht = false;
                    if let Some(a) = &mut audio {
                        a.seek(ziel);
                    }
                    writer.force_redraw();
                }
                Cmd::Resize(w, h) => term_size = (w, h),
            }
        }

        // Fenstergröße geändert? Raster neu rechnen und ffmpeg neu starten.
        let aktuell = if _guard.is_some() {
            crossterm::terminal::size().unwrap_or(term_size)
        } else {
            term_size
        };
        if aktuell != term_size && aktuell.0 > 0 {
            term_size = aktuell;
            let nutzbar = if st.ui {
                aktuell.1.saturating_sub(1).max(1)
            } else {
                aktuell.1
            };
            let neu = geometry::compute(
                aktuell.0,
                nutzbar,
                info.aspect(),
                args.cell_aspect,
                args.fit,
                args.supersample,
                args.size,
            );
            if neu != layout {
                layout = neu;
                grid.resize(layout.grid_w, layout.grid_h);
                base = clock.media_now().as_secs_f64();
                if !quelle.is_live {
                    cfg.start = Some(base);
                }
                let (r, _) = start_reader(&cfg, &layout)?;
                rx = r;
                clock.seek_to(Duration::from_secs_f64(base));
                warte_auf_erstes_bild = true;
                warten_seit = Instant::now();
                writer.force_redraw();
            }
        }

        if clock.is_paused() {
            std::thread::sleep(Duration::from_millis(20));
            continue;
        }

        let frame = match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(Msg::Bild(f)) => f,
            Ok(Msg::Ende(a)) => {
                // Ein Strom, der nie ein Bild geliefert hat, ist kein
                // erfolgreiches Ende -- sonst sieht der Aufrufer nur einen
                // leeren Schirm und Rückgabewert 0.
                if a.bilder == 0 {
                    bail!(
                        "Die Quelle hat kein einziges Bild geliefert.
{}",
                        a.meldungen
                    );
                }
                if !a.erfolg {
                    bail!(
                        "ffmpeg endete mit einem Fehler.
{}",
                        a.meldungen
                    );
                }
                break 'wiedergabe;
            }
            Ok(Msg::Fehler(e)) => bail!("{e}"),
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                if warte_auf_erstes_bild {
                    let gewartet = warten_seit.elapsed();

                    // Erster Ausweg: sequenziell überspringen statt springen.
                    if gewartet > SPUL_GEDULD && !sequenziell_versucht && cfg.seekable {
                        cfg.seekable = false;
                        sequenziell_versucht = true;
                        notes.push(
                            "Diese Quelle beantwortet keine Sprunganfragen -- es wird                              sequenziell überspult, das dauert länger."
                                .into(),
                        );
                        let (r, _) = start_reader(&cfg, &layout)?;
                        rx = r;
                        warten_seit = Instant::now();
                    } else if gewartet > WARTE_GRENZE {
                        bail!(
                            "Seit {} Sekunden kein Bild von der Quelle. Abgebrochen,                              statt ein stehendes Bild zu zeigen.",
                            gewartet.as_secs()
                        );
                    }

                    // Statuszeile weiterzeichnen, sonst sieht es aus wie ein Absturz.
                    if st.ui {
                        let z = statuszeile(
                            &quelle,
                            &st,
                            &layout,
                            &clock,
                            &info,
                            true,
                            ton_gewuenscht,
                            term_size.0,
                            None,
                        );
                        writer.draw(&grid, &layout, st.color, st.dither, Some(&z), term_size.1)?;
                    }
                }
                continue;
            }
            Err(_) => break 'wiedergabe,
        };

        let pts = Duration::from_secs_f64(base) + frame.pts(fps);

        if warte_auf_erstes_bild {
            clock.seek_to(pts);
            warte_auf_erstes_bild = false;
            if ton_gewuenscht && audio.is_none() {
                audio = if args.verbose {
                    // Mit -v soll man sehen, *warum* kein Ton kommt.
                    match audio::start_verbose(&quelle, pts.as_secs_f64(), st.volume) {
                        Ok(a) => a,
                        Err(e) => {
                            eprintln!("Ton lässt sich nicht starten: {e:#}");
                            None
                        }
                    }
                } else {
                    audio::start(&quelle, pts.as_secs_f64(), st.volume)
                };
            }
        }

        // Ton als Leit-Uhr. Die Soundkarte zählt, was sie wirklich abgeholt
        // hat -- das ist die verlässlichste Zeitquelle im Programm. Das Bild
        // wird nachgezogen, sobald es spürbar abweicht.
        //
        // Nur, solange der Ton auch tatsächlich läuft: stockt er, darf er das
        // Bild nicht mitreißen. Deshalb die Prüfung, ob die Position seit dem
        // letzten Durchgang überhaupt gewachsen ist.
        if let Some(a) = &audio {
            let ton = a.position();
            if !clock.is_paused() && ton > letzte_tonposition + 0.001 {
                let drift = clock.media_now().as_secs_f64() - ton;
                if drift.abs() > TON_DRIFT {
                    clock.seek_to(Duration::from_secs_f64(ton));
                }
            }
            letzte_tonposition = ton;
        }

        // Zu spät? Verwerfen, bevor gerendert wird -- das ist die Stelle, an
        // der das Verwerfen tatsächlich etwas spart.
        if clock.lateness(pts).is_some_and(|l| l > frame_dur) {
            dropped += 1;
            continue;
        }
        clock.sleep_until(pts);

        renderer.render(&frame, &layout, &st.opts, &mut grid);
        gezeigt += 1;

        // Gleitende Bildrate über ein kurzes Fenster.
        if fps_fenster.0.elapsed() >= Duration::from_millis(500) {
            fps_fenster.2 =
                (gezeigt - fps_fenster.1) as f64 / fps_fenster.0.elapsed().as_secs_f64();
            fps_fenster = (Instant::now(), gezeigt, fps_fenster.2);
        }

        let zeile = st.ui.then(|| {
            statuszeile(
                &quelle,
                &st,
                &layout,
                &clock,
                &info,
                false,
                ton_gewuenscht,
                term_size.0,
                args.stats.then_some(term::ui::Stats {
                    fps: fps_fenster.2,
                    dropped,
                    bytes: writer.stats.bytes,
                    cells_written: writer.stats.cells_written,
                    cells_total: writer.stats.cells_total,
                }),
            )
        });

        writer.draw(
            &grid,
            &layout,
            st.color,
            st.dither,
            zeile.as_deref(),
            term_size.1,
        )?;
        bytes_gesamt += writer.stats.bytes as u64;

        if args.once {
            break 'wiedergabe;
        }
    }

    stop.store(true, Ordering::Relaxed);
    drop(rx);
    if let Some(a) = &mut audio {
        a.stop();
    }
    drop(_guard);

    if args.stats {
        let schnitt = bytes_gesamt.checked_div(gezeigt).unwrap_or(0);
        eprintln!(
            "{gezeigt} Bilder gezeigt, {dropped} verworfen, im Schnitt {:.1} KB/Bild \
             ({:.2} MB/s bei {fps:.0} fps)",
            schnitt as f64 / 1024.0,
            schnitt as f64 * fps / (1024.0 * 1024.0),
        );
    }
    Ok(())
}
