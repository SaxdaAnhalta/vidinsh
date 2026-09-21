# vidinsh

Spielt Videos, Streams und Kamerabilder als farbige Zeichengrafik im Terminal ab.

```
vidinsh video.mp4
vidinsh --mode glyph --color truecolor video.mp4
vidinsh "https://example.com/live/master.m3u8"
vidinsh cam:0
```

**Die Shell ist egal.** `vidinsh` ist eine Binärdatei, die ANSI-Bytes nach stdout
schreibt — ob PowerShell, cmd, bash oder fish sie gestartet hat, bemerkt sie
nicht einmal. Was zählt, ist der Terminal-Emulator drumherum. `vidinsh --probe`
zeigt, was deiner kann.

---

## Bauen

Gebraucht werden **Rust** (ab 1.85 wegen Edition 2024) und **ffmpeg** im PATH.

```bash
cargo build --release
./target/release/vidinsh --probe
```

Ergibt eine Programmdatei von rund 1,6 MB, die ffmpeg vom System benutzt.

### Eine Datei, die überall läuft

```bash
cargo build --release --features bundled
```

Packt ffmpeg in die Programmdatei. Das Ergebnis sind rund **72 MB** — eine
einzelne Datei, die auf einem Rechner läuft, auf dem **nichts** installiert
ist. Beim ersten Start wird ffmpeg einmalig nach `%LOCALAPPDATA%idinsh\`
entpackt (unter Linux und macOS nach `~/.cache/vidinsh/`) — gemessen 2,7
Sekunden. Jeder weitere Start liegt bei 0,1 Sekunden.

Welches ffmpeg eingepackt wird, bestimmt `VIDINSH_FFMPEG`; ohne die Variable
wird das aus dem PATH genommen. Das Packen dauert einige Minuten und wird
zwischengespeichert — der zweite Bau ist wieder schnell.

Ohne diese Eigenschaft bleibt alles wie gehabt: 1,6 MB, ffmpeg vom System.
Mit `--ffmpeg <pfad>` lässt sich in beiden Fassungen eine bestimmte
Programmdatei erzwingen.

Für Glyph-Masken wird eine Schrift des Systems *gelesen* (unter Windows
`CascadiaMono.ttf`), nicht kopiert. Nur für YouTube-Links wird zusätzlich
`yt-dlp` gebraucht — siehe unten.

---

## Quellen

| Eingabe | Beispiel |
|---|---|
| Datei | `vidinsh film.mp4` |
| Bild, GIF | `vidinsh bild.png --once` · `vidinsh anim.gif --loop` |
| HLS, DASH | `vidinsh "https://host/master.m3u8"` |
| RTSP, RTMP, SRT, UDP | `vidinsh rtsp://kamera.local/live` |
| gewöhnliche URL | `vidinsh "https://host/video.mp4"` |
| YouTube und andere Portale | `vidinsh "https://youtu.be/…"` — braucht `tools/yt-dlp.exe` |
| Kamera | `vidinsh --list-devices`, dann `vidinsh cam:0` |
| stdin | `cat film.mp4 \| vidinsh -` |

HLS, DASH, RTSP, RTMP, SRT und UDP kann ffmpeg selbst; die URL wandert
unverändert hinein, zusammen mit Optionen zum Wiederverbinden. Nur Portale, bei
denen die URL keine Mediendatei ist, brauchen `yt-dlp`.

### yt-dlp (nur für Portale)

`yt-dlp` ist eine einzelne portable Datei und liegt hier:

```
vidinsh/tools/yt-dlp.exe
```

**Nicht global installiert** — gesucht wird in dieser Reihenfolge: `tools/`
neben der Programmdatei, `tools/` im Arbeitsverzeichnis, dann der PATH. Fehlt
die Datei, funktionieren alle anderen Quellenarten unverändert; nur Portal-Links
melden verständlich, dass sie gebraucht wird. Aktualisieren geht mit
`tools\yt-dlp.exe -U`.

YouTube liefert Bild und Ton in aller Regel **getrennt**; die gemuxten Formate
gibt es nur noch in niedriger Auflösung. `vidinsh` nimmt deshalb eine gemuxte
Spur, wenn es sie gibt, und sonst die beste Kombination aus getrenntem Bild und
Ton — das Bild geht an ffmpeg, der Ton an die Tonwiedergabe.

yt-dlp meldet beim Start `No supported JavaScript runtime could be found`. Das
ist eine Warnung, keine Fehlermeldung: ohne JS-Runtime fehlen einige
hochauflösende Formate, die übrigen funktionieren. Wer die volle Auswahl will,
installiert Deno — dafür ist hier bewusst nichts vorbereitet.

---

## Modi

Umschaltbar mit den Tasten `1`–`4`, auch mitten in der Wiedergabe.

| Taste | `--mode` | Verfahren |
|---|---|---|
| `1` | `ramp` | Helligkeit je Zelle → Zeichen aus einer Rampe. Schnell, klassischer Look. |
| `2` | `glyph` | Zeichen per Formvergleich gegen die Schrift des Terminals. Beste Zeichenqualität. |
| `3` | `blocks` | Blockzeichen mit zwei Farben je Zelle. Schärfstes Bild, kein „ASCII". |
| `4` | `edge` | Konturen als `\| / - \`, Flächen als Rampe. Stilisiert. |

`blocks` kennt vier Feinheitsstufen über `--charset`:

| `--charset` | Unterteilung | Anmerkung |
|---|---|---|
| `half` | 1×2 | `▀`, zwei Farben. In jeder Schrift vorhanden. |
| `quad` | 2×2 | 16 Quadrantenzeichen. In fast jeder Schrift vorhanden. |
| `sextant` | 2×3 | U+1FB00. Cascadia Mono hat sie; viele andere Schriften nicht. |
| `braille` | 2×4 | Feinste Struktur, aber nur **eine** Farbe je Zelle. |

---

## Tasten

| Taste | Wirkung | | Taste | Wirkung |
|---|---|---|---|---|
| `1`–`4` | Modus | | `Leer` | Pause |
| `c` | Farbtiefe durchschalten | | `←` `→` | ±5 s spulen |
| `b` | Hintergrundfarbe an/aus | | `↑` `↓` | Lautstärke |
| `d` | Dithering an/aus | | `+` `-` | Geschwindigkeit |
| `i` | Statuszeile | | `q` `Esc` | Ende |

---

## Flags

`vidinsh --help` zeigt alles. Das Wichtigste:

```
Optik
  -m, --mode <MODUS>       ramp | glyph | blocks | edge          [ramp]
      --charset <SATZ>     ascii | extended | half | quad | sextant | braille
      --ramp <STR>         Zeichenrampe                          [" .:-=+*#%@"]
  -c, --color <MODUS>      truecolor | 256 | 16 | mono           [erkannt]
      --dither             Bayer-Dithering, hebt 256 und 16 deutlich
      --bg                 Hintergrundfarbe mitfärben
      --invert             Rampe umkehren, für helle Terminals
      --ascii              Kurzform für --charset ascii
      --font <PFAD>        TTF für die Glyph-Masken
      --edge-threshold <F> ab welcher Kantenstärke edge ein Zeichen setzt [0.10]

Geometrie
  -s, --size <BxH>         Raster erzwingen                      [Terminalgröße]
      --cell-aspect <F>    Höhe/Breite einer Zelle               [2.0]
      --fit <MODUS>        contain | cover | stretch             [contain]
      --supersample <BxH>  Abtastung je Zelle                    [4x8]

Zeit
  -f, --fps <N>            Ziel-Framerate                        [wie Quelle]
      --speed <F> --ss <ZEIT> --to <ZEIT> --loop --once

Bild
      --brightness <F> --contrast <F> --saturation <F>
      --auto-contrast      Helligkeit je Bild normalisieren
      --gamma-correct      in linearem Licht skalieren (braucht libzimg)
      --scaler <F>         area | bilinear | bicubic | lanczos   [area]

Ton
      --no-audio  --volume <0-100>

Sonstiges
      --probe              Terminal-Fähigkeiten + Testbild, dann Ende
      --stats              fps, verworfene Bilder, Bandbreite
      --write <DATEI>      ANSI-Strom in eine Datei statt ins Terminal
      --ffmpeg <PFAD>      bestimmte ffmpeg-Programmdatei benutzen
      --list-devices  --no-ui  -v/--verbose
```

Zeitangaben gehen als `90`, `12.5`, `1:30` oder `1:02:03`.

Umgebungsvariablen: `NO_COLOR` erzwingt `mono`; `COLORTERM`, `TERM`,
`WT_SESSION` und `TMUX` fließen in die Erkennung ein.

---

## Terminals

Es gibt kein „läuft / läuft nicht", sondern eine Leiter nach unten:

| Terminal | Farbe | Zeichen |
|---|---|---|
| Windows Terminal, kitty, WezTerm, iTerm2, Alacritty, VS Code | truecolor + Sync | alles |
| conhost (Win10 1511+), gnome-terminal, mintty/Git Bash | truecolor | Blöcke, Braille |
| macOS Terminal.app, tmux ohne RGB-Konfiguration | 256 + Dithering | Halbblöcke |
| PuTTY (alt), GNU screen | 16 + Dithering | ASCII |
| conhost mit Rasterschrift, `TERM=dumb` | 16 | nur ASCII |

Ganz unten läuft es überall, bis in alte SSH-Sitzungen.

Erkannt wird über Umgebungsvariablen und, unter Windows, über den Erfolg von
`SetConsoleMode` — bewusst **keine** Abfrage per Escape-Sequenz, weil Terminals,
die darauf nicht antworten, Müll auf dem Schirm hinterlassen oder blockieren.
Eine ausdrückliche Angabe mit `--color` oder `--charset` schlägt die Erkennung
immer.

---

## Wenn etwas klemmt

**Über SSH ruckelt es.** Truecolor bei 200×60 sind rund 0,8 MB/s. Über eine
schmale Leitung hilft `--color 256 --fps 15` — das drückt es auf etwa ein
Zehntel. Mit `--stats` siehst du die tatsächliche Bandbreite.

**Kästchen statt Blockzeichen.** Der Schrift fehlen die Zeichen. `--probe` zeigt
je eine Zeile pro Zeichensatz; was dort als Kästchen erscheint, fehlt. `--ascii`
oder `--charset quad` ist dann der Ausweg.

**Das Bild ist zu hoch oder zu breit.** Dein Terminal hat ein anderes
Zellverhältnis als die angenommenen 2.0. `--cell-aspect 1.8` oder `2.2`
probieren.

**Dunkles Material sieht nach nichts aus.** `--auto-contrast` spreizt die
Helligkeit je Bild.

**Spulen bei YouTube dauert lange.** Das ist keine Macke, sondern eine Grenze
der Quelle: googlevideo-Adressen beantworten keine Sprunganfragen. `vidinsh`
merkt das nach fünf Sekunden und stellt auf sequenzielles Überspulen um — es
lädt dann von der aktuellen Stelle bis zum Ziel durch. Die Statuszeile zeigt
`...` währenddessen. Bei lokalen Dateien und den meisten Streams springt es
dagegen sofort. Kommt gar nichts, bricht `vidinsh` nach 90 Sekunden mit einer
Meldung ab, statt ein stehendes Bild zu zeigen.

**Kein Ton.** Die Statuszeile sagt es dir: `Ton 100%` heißt an, `Ton stumm`
heißt auf 0 gedreht (mit `↑` wieder hoch), `ohne Ton` heißt, dass für diese
Quelle kein Ton in Frage kommt. Mit `-v` steht beim Start eine Zeile, die die
Entscheidung aufschlüsselt. Kamera und stdin haben nie Ton, `--once` und
`--write` schalten ihn ab.

**Keine Tonausgabe gefunden.** Mit `-v` steht die Begründung da. `vidinsh`
nimmt das Standard-Ausgabegerät des Systems; gibt es keins, läuft das Bild
ohne Ton weiter statt abzubrechen.

**Es läuft gar nicht an.** `-v` zeigt das gebaute ffmpeg-Kommando und die
erkannten Fähigkeiten. Das Kommando lässt sich direkt in der Shell nachstellen.

---

## Bekannte Grenzen

**Spulen bei YouTube ist langsam.** googlevideo-Adressen beantworten keine
Sprunganfragen; es wird sequenziell überspult. Siehe oben.

**Sextanten hängen an der Schrift.** Cascadia Mono hat sie, viele andere nicht.

**Nicht überall geprüft.** Getestet wurde unter Windows in Windows Terminal,
conhost, VS Code, Git Bash und WSL. PuTTY, macOS Terminal.app, kitty und echtes
tmux konnten nicht geprüft werden — die Abstufung ist so gebaut, dass unbekannte
Terminals auf 256 oder 16 Farben landen statt kaputtzugehen. `--probe` sagt es
dir in einer Sekunde.

---

Wie das Ganze innen funktioniert, steht in [docs/ARCHITEKTUR.md](docs/ARCHITEKTUR.md).
