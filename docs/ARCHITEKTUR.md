# Wie vidinsh funktioniert

Das Naheliegende an dieser Aufgabe — ein Video dekodieren und Zeichen ausgeben —
ist der einfache Teil. ffmpeg nimmt einem das Dekodieren komplett ab. Schwierig
sind drei andere Dinge, und um die herum ist alles gebaut:

1. **Durchsatz.** Ein Raster von 200×60 in Truecolor sind naiv **250 KB pro
   Bild**. Bei 30 fps wären das 7,3 MB/s durch den Terminal-Parser. Daran
   scheitern die meisten Bastellösungen — sie ruckeln bei 10 fps und man hält
   das für ein CPU-Problem, obwohl es ein Bandbreitenproblem ist.
2. **Zeichenwahl.** Eine Helligkeitsrampe ist schnell erklärt und sieht flau
   aus. Formerkennung und Blockzeichen sehen deutlich besser aus — welches
   Verfahren gewinnt, hängt aber am Material.
3. **Takt.** Bilder richten sich nach der Uhr, nicht die Uhr nach den Bildern.

---

## Die Pipeline

```
 ffmpeg (Kindprozess)        Hauptschleife                  Writer
 ────────────────────        ─────────────                  ──────
 dekodieren + skalieren ──►  zu spät? verwerfen      ──►    Diff + SGR-Lauflängen
 rawvideo rgb24              sonst: Frame ──► Raster        ein write_all, geklammert
        │                            (rayon über Zeilen)    in synchronisierte Ausgabe
        │ Lesethread
        └── bounded(3) ──────────────┘

 Eingabethread (crossterm) ──► Kommandos
 ffmpeg (PCM) ──► Ringpuffer ──► cpal ──► Ton (und Leit-Uhr)
```

Der Lesethread liegt bewusst getrennt: sonst dekodiert ffmpeg nur dann weiter,
wenn wir gerade lesen, und die Pipe läuft im Takt unserer Renderzeit leer. Der
Kanal ist auf drei Bilder begrenzt — genug, um Schwankungen aufzufangen, wenig
genug, dass der Speicher konstant bleibt und wir nie veraltete Bilder anhäufen.

Das Rendern läuft in der Hauptschleife und parallelisiert intern über `rayon`
(ein Zeilenblock je Kern). Eine vierte Stufe, die Rendern und Schreiben
voneinander entkoppelt, wäre denkbar — bei den gemessenen Zeiten (siehe unten)
bringt sie nichts, deshalb steht sie nicht drin.

---

## Die zentrale Entscheidung: Supersampling

ffmpeg skaliert **nicht** auf `Spalten × Zeilen`, sondern auf
`Spalten·4 × Zeilen·8` — die feinste Abtastung, die irgendein Modus braucht.
Jeder Renderer mittelt sich daraus herunter, was er benötigt:

| Modus | braucht je Zelle | rechnet herunter aus 4×8 |
|---|---|---|
| `ramp` | 1×1 | Mittel über alle 32 Felder |
| `blocks --charset half` | 1×2 | zwei Mittel |
| `blocks --charset quad` | 2×2 | vier Mittel |
| `blocks --charset sextant` | 2×3 | ganzzahlig geteilt: 3/3/2 Zeilen |
| `blocks --charset braille` | 2×4 | acht Mittel |
| `edge` | 1×1 + Nachbarzellen | Sobel/DoG auf dem Helligkeitsraster |
| `glyph` | 4×8 | unverändert |

**Warum das die richtige Entscheidung ist:** Moduswechsel und Farbwechsel
bleiben damit reine Render-Operationen. Kein ffmpeg-Neustart, keine
Unterbrechung, kein schwarzer Schirm beim Drücken von `2`. Hätten wir nur
`Spalten × Zeilen` angefordert, müsste bei jedem Tastendruck der ganze
Dekodierprozess neu starten.

Die Kosten sind gering: 200×60 → 800×480 rgb24 = 1,15 MB je Bild durch die
Pipe. Bei 60 fps sind das 69 MB/s — für eine Pipe nichts, und swscale skaliert
SIMD-optimiert weit schneller als alles, was sich in Rust mit vertretbarem
Aufwand nachbauen ließe.

ffmpeg wird nur bei **Fenstergrößenänderung** und beim **Spulen** neu gestartet.

`--supersample 2x4` halbiert den Aufwand, wenn `glyph` nicht gebraucht wird.

---

## Der Writer: wo die Bandbreite verschwindet

Gemessen an `testdata/test.mp4`, 200×60, Truecolor, 60 Bilder:

| | Bytes je Bild | bei 30 fps |
|---|---|---|
| naiv (`--bench-naive`) | **250 KB** | 7,3 MB/s |
| mit Diffing + Lauflängen | **27,4 KB** | 0,80 MB/s |

Faktor **9,1**. Drei Maßnahmen tragen das:

**1. SGR-Lauflängen.** Der größte Einzelhebel. Ein Truecolor-Farbpaar sind 39
Bytes, das Zeichen selbst eines. Der Farbcode wird nur ausgegeben, wenn er sich
vom vorigen Zeichen unterscheidet. Bei Videomaterial mit Flächen gleicher Farbe
fällt damit der Großteil der Bytes weg.

**2. Diffing.** Das vorige Raster wird behalten; nur geänderte Zellen werden
ausgegeben, unveränderte per Cursorsprung übergangen. Die Schwelle steht in
`SKIP_MIN` (3): ein Sprung kostet etwa 8 Bytes, eine mitgeschriebene Zelle je
nach Farbwechsel 1 bis 40 — darunter lohnt der Sprung nicht. Bei einem harten
Schnitt fällt das sauber auf „alles neu" zurück. Bei stehendem Bild geht es auf
**0,1 KB je Bild** herunter.

Vollbild erzwungen wird nach Fenstergrößenänderung, Moduswechsel, und bei
**jeder Änderung von Farbtiefe oder Dithering** — eine unveränderte Zelle ergäbe
sonst andere Bytes als beim letzten Mal, und alte Farben blieben stehen.

**3. Ein `write_all` je Bild**, geklammert in synchronisierte Ausgabe
(`ESC[?2026h` … `ESC[?2026l`). Das Terminal zeichnet erst, wenn alles da ist —
beseitigt Tearing für 16 Bytes. Gesendet wird die Klammer nur bei belegter
Unterstützung; Terminals, die den Modus nicht kennen, ignorieren ihn zwar
normalerweise stillschweigend, aber „normalerweise" reicht hier nicht.

`--bench-naive` schaltet 1 und 2 ab. Das ist keine Altlast, sondern der
Vergleichsmaßstab — ohne ihn ließe sich nicht belegen, dass die Optimierung
wirkt, und eine Regression fiele niemandem auf.

---

## Die vier Modi

Alle rechnen in **linearem Licht**. Wer acht sRGB-Bytes addiert und durch acht
teilt, bekommt ein zu dunkles Ergebnis; bei weichen Verläufen sieht man das
sofort. Umgerechnet wird über Tabellen, weil `powf` sonst 36 000-mal je Bild
anfiele.

### `ramp`

Tile mitteln → Helligkeit → Index in die Zeichenrampe. Zwei Durchgänge, weil
`--auto-contrast` erst die Verteilung des ganzen Bildes braucht; die wird über
ein 256-Eimer-Histogramm bestimmt und am 1.- und 99.-Perzentil beschnitten —
ohne diesen Puffer reicht ein einzelnes Glanzlicht, um die Normalisierung für
das ganze Bild unbrauchbar zu machen.

Die Rampe wird über der **gamma-kodierten** Helligkeit indiziert, nicht über der
linearen: eine Zeichenrampe ist nach optischer Dichte sortiert, nicht nach
Lichtmenge.

### `blocks`

Eine Terminalzelle trägt zwei Farben. Bei Halbblöcken passt das genau: `▀` mit
Vordergrundfarbe oben, Hintergrundfarbe unten. Sind beide Hälften gleich, wird
ein Leerzeichen mit Hintergrund gesetzt — das spart im Writer den kompletten
Vordergrund-Farbcode.

Ab Quadranten reichen zwei Farben nicht mehr für vier bis acht Unterfelder.
Dann wird an der mittleren Helligkeit in zwei Gruppen geteilt, jede Gruppe
gemittelt, und das Zeichen mit der passenden Bitmaske gewählt — Block Truncation
Coding. Bei Sextanten sind die vier Muster ausgespart, für die es schon Zeichen
gibt (leer, linke Hälfte, rechte Hälfte, voll); die Nummerierung ab U+1FB00
überspringt sie.

Braille trägt 2×4 Punkte, aber nur **eine** Farbe — feinste Struktur, flachere
Farben.

Ohne Unicode fällt `blocks` auf die Rampe zurück. Ein gröberes Bild ist besser
als eine Wand aus Ersatzkästchen.

### `edge`

Zwei Fragen, zwei Werkzeuge:

* **Wo ist eine Kante?** Difference of Gaussians — zweimal weichzeichnen, einmal
  mit σ, einmal mit 1,6·σ, Differenz nehmen. Das reagiert auf echte Konturen und
  lässt weiche Verläufe in Ruhe, anders als ein roher Sobel-Betrag, der auch in
  jedem Rauschen anschlägt.
* **In welche Richtung?** Sobel liefert den Helligkeitsgradienten; die Kante
  steht senkrecht darauf, also ist ihre Richtung `(-gy, gx)`. Gerechnet wird in
  Bildschirmkoordinaten, in denen y nach unten wächst — deshalb ist
  rechts-und-runter ein `\` und nicht ein `/`.

Unter der Schwelle wird die Rampe benutzt.

### `glyph`

Der Qualitätsmodus. Jede Kandidaten-Glyphe wird einmalig aus der Schrift
gerastert, die das Terminal tatsächlich anzeigt (`CascadiaMono.ttf`), und auf
die Maskenauflösung heruntergerechnet. Damit entspricht das, was der Vergleich
für richtig hält, dem, was am Ende auf dem Schirm steht.

**Das Maß ist nicht das naheliegende.** Man würde die Maske gegen den
normalisierten Helligkeitsausschnitt halten und die Fehlerquadrate summieren.
Das ist aber nicht die Frage: Vorder- und Hintergrundfarbe sind frei wählbar und
werden ohnehin aus dem Ausschnitt gemittelt. Der Fehler der fertigen Zelle ist
deshalb genau die Streuung **innerhalb** der beiden Gruppen, die die Maske
aufteilt. Gesucht wird die Maske mit der kleinsten Summe der Gruppenvarianzen —
dieselbe Zielgröße wie bei einer Zweiteilung nach k-Means, nur dass die
erlaubten Aufteilungen durch den Zeichensatz vorgegeben sind.

Eine Folge davon, die anfangs überrascht: **eine Maske und ihr Komplement sind
gleichwertig.** Ob die Tinte die helle oder die dunkle Hälfte trägt, ergibt
dasselbe Bild — nur mit vertauschten Farben. Deshalb ist `--invert` in diesem
Modus wirkungslos; es ergäbe schlicht ein Negativ. Die Rampe braucht `--invert`,
weil sie nur die Vordergrundfarbe setzt und die Zeichendichte zum Untergrund des
Terminals passen muss.

Naiv wären das ~34 Millionen Operationen je Bild. Zwei Maßnahmen drücken das:
die Masken sind nach Deckungsgrad sortiert, und gesucht wird nur in einem
Fenster um den Helligkeitsanteil der Zelle (aus ~95 Kandidaten werden ~15);
dazu läuft alles parallel über Zeilen.

### Gemessen

`testdata/test.mp4`, 200×60 Zellen, Truecolor, 60 fps, 16 Kerne, 240 Bilder:

| Modus | verworfen | Bytes je Bild |
|---|---|---|
| `ramp` | 3 | 14,3 KB |
| `blocks --charset quad` | 2 | 29,4 KB |
| `edge` | 3 | 14,3 KB |
| `glyph` | 3 | 27,8 KB |

Alle vier halten 60 fps; die zwei bis drei verworfenen Bilder fallen beim
Anlauf an.

---

## Farbe und Dithering

Vier Stufen: Truecolor (`ESC[38;2;r;g;bm`), 256 (6×6×6-Würfel plus
Graustufenrampe, je nachdem was näher liegt), 16 (nächster Nachbar in der
kanonischen Palette), mono.

`--dither` verschiebt jede Farbe vor der Quantisierung ortsabhängig um bis zu
eine halbe Stufenbreite, nach einer Bayer-Matrix 8×8. Die Verschiebung hängt
**nur an der Zellposition, nicht am Bildinhalt** — eine unveränderte Zelle ergibt
deshalb weiterhin dieselben Bytes, und das Diffing bleibt wirksam.

Der Zielkonflikt ist real und sollte bekannt sein. `bars.mp4`, 120×40, 256
Farben, ein Bild:

| | Bytes | verschiedene Palettenfarben |
|---|---|---|
| ohne `--dither` | 10 118 | 31 |
| mit `--dither` | 55 281 | 63 |

Doppelte Farbvielfalt, aber das 5,5-fache an Bytes — benachbarte Zellen bekommen
nun unterschiedliche Farbcodes und brechen die SGR-Lauflängen. In 256 und 16
Farben lohnt sich das fast immer; bei Truecolor gibt es nichts zu quantisieren,
dort ist `--dither` wirkungslos.

---

## Takt und Verwerfen

Die Uhr rechnet zwischen Medienposition und Wanduhr um. Weil ffmpeg zu
konstanter Bildrate gezwungen wird (`-r`), ist der Zeitstempel eines Bildes
schlicht seine laufende Nummer geteilt durch die Bildrate — kein PTS aus der
Pipe zu fischen.

Geprüft wird **bevor** gerendert wird: liegt die Zielzeit mehr als eine
Bilddauer zurück, wird verworfen. Das ist die einzige Stelle, an der Verwerfen
tatsächlich etwas spart.

**Die Uhr wird auf das erste tatsächlich eingetroffene Bild gesetzt**, nicht auf
den Programmstart. Ohne das läuft sie schon, während ffmpeg anläuft oder eine
Netzquelle puffert — und dann gilt alles, was danach kommt, als überfällig. Bei
einem HLS-Teststream waren das 72 von 72 Bildern; nach der Korrektur 0. Dasselbe
gilt nach jedem Neustart, also auch nach Spulen und Fenstergrößenänderung.

Pause wird aus der Rechnung herausgenommen, statt die Position vorzuspulen.
Tempowechsel setzen den Anker neu, damit die Position nicht springt.

### Spulen, und warum es drei Vorkehrungen braucht

Gespult wird durch einen ffmpeg-Neustart mit neuem `-ss`. Bei googlevideo --
also jeder aufgelösten YouTube-Adresse -- fror das Bild dabei minutenlang ein.
Die Suche nach dem Grund förderte drei getrennte Fehler zutage.

**1. Die Ursache: offene Bereichsanfragen.** Zum Spulen stellt ffmpeg eine
HTTP-Anfrage mit `Range: bytes=N-` -- ohne Endpunkt. Gemessen an einer
aufgelösten YouTube-Adresse:

| Anfrage | Antwort |
|---|---|
| `Range: bytes=1000000-1100000` | HTTP 206 in **0,09 s** |
| `Range: bytes=1000000-` | **keine Antwort**, Verbindung bleibt offen |

Der Server ist also nicht überlastet und die Adresse nicht abgelaufen -- er
beantwortet schlicht keine offenen Bereiche. ffmpeg wartet daraufhin ewig,
liefert nie ein Bild und beendet sich auch nicht. Weder ein anderer
User-Agent noch `-multiple_requests 1` noch `-http_seekable 1` ändern daran
etwas; nur `-seekable 0` (oder `-ss` hinter `-i`) hilft, und beide lesen
sequenziell von vorn.

Deshalb trägt `Input` ein Feld `offene_bereiche`. Für `*.googlevideo.com` steht
es auf `false`, und dann wird der aussichtslose Versuch übersprungen. Eine
kurze, benannte Liste statt einer Heuristik -- bei allen anderen Servern greift
der Ausweg zur Laufzeit.

**2. Der eigentliche Zeitfresser: Prozessleichen.** Der Lesethread steckt bei
einem hängenden ffmpeg in `read()` auf einer Pipe, die nie etwas liefert. Er
merkt deshalb *nie*, dass der Empfänger längst fallengelassen wurde -- und
beendet den Prozess nicht. Bei jedem Sprung blieb ein ffmpeg zurück, das mit
`-reconnect` weiter am Netz zerrte. Ein Sprung auf Sekunde 12 kostete dadurch
67 Sekunden statt 6.

`FfmpegSource::abbruch()` gibt jetzt einen Griff heraus, mit dem die
Hauptschleife den alten Prozess beendet, bevor sie den neuen startet. Auf den
Lesethread zu bauen war die falsche Annahme: er kann von einer toten Pipe
nichts lernen.

**3. Der Wachhund.** Für alles, was nicht auf der Liste steht: kommt nach
`SPUL_GEDULD` (4 s) kein Bild, wird mit `-seekable 0` neu gestartet, und die
Quelle merkt sich das. Nach `WARTE_GRENZE` (90 s) wird abgebrochen -- ein
stehendes Bild ohne Erklärung ist das schlechteste aller Ergebnisse.

Dazu zeichnet die Statuszeile während des Wartens weiter und zeigt `...` statt
`>`. Ohne das sieht auch ein funktionierender, nur langsamer Sprung aus wie ein
Absturz.

Gemessen nach allen drei Korrekturen, Zeit bis zum ersten Bild nach dem Sprung:

| Quelle | vorher | jetzt |
|---|---|---|
| lokale Datei | 0,2 s | 0,2 s |
| YouTube, Ziel Sekunde 12 | Hänger, faktisch 67 s | **5,6 s** |

Die verbleibenden 5,6 Sekunden sind kein Fehler mehr, sondern der Preis des
sequenziellen Überspulens -- er wächst mit der Zielposition. Schneller ginge
es nur mit einem eigenen Zwischenserver, der offene Bereiche in begrenzte
zerlegt; das wäre ein HTTPS-Client samt TLS-Bibliothek und steht in keinem
Verhältnis.

Dieselbe Falle gilt für den Ton: `audio.rs` setzt `-seekable 0` unter derselben
Bedingung. Ohne das hinge nach jedem Sprung der Ton-Prozess still vor sich hin.

---

## Terminal-Fähigkeiten

Die Shell ist für dieses Programm unsichtbar — es schreibt ANSI-Bytes nach
stdout. Was variiert, ist der Terminal-Emulator.

Erkannt wird über Umgebungsvariablen (`WT_SESSION`, `COLORTERM`, `TERM`,
`TMUX`, `NO_COLOR`) und, unter Windows, über den Erfolg von `SetConsoleMode` mit
`ENABLE_VIRTUAL_TERMINAL_PROCESSING` — das ist dort der eigentliche Test, nicht
irgendeine Variable.

**Bewusst keine aktive Abfrage per Escape-Sequenz.** Die ist langsam, und
Terminals, die nicht antworten, hinterlassen entweder Müll auf dem Schirm oder
blockieren. Umgebungsvariablen plus überschreibende Flags decken die Realität
besser ab.

Eine **ausdrückliche** Angabe mit `--color` oder `--charset` schlägt die
Erkennung immer — gewarnt wird trotzdem. Das ist wichtig, weil die Erkennung nur
raten kann: bei `--write` in eine Datei weiß sie überhaupt nichts über das
Terminal, das die Datei später anzeigt. Heruntergestuft wird nur die
*automatische* Wahl.

Unter Windows wird zusätzlich die Ausgabe-Codepage auf UTF-8 gestellt (und beim
Beenden zurückgesetzt) und geprüft, ob die alte Rasterschrift „Terminal" aktiv
ist — die kann keine Blockzeichen, dann fällt der Zeichensatz auf ASCII zurück
statt Kästchen zu zeigen.

### Aufräumen

Rohmodus, Alternativschirm, Cursor und Zeilenumbruch müssen **immer**
zurückgesetzt werden, sonst ist die Shell danach unbenutzbar. Drei Wege führen
zur selben idempotenten Funktion: `Drop` des Guards, der Panik-Haken, und das
normale Programmende. Der Panik-Haken räumt **vor** dem Bericht auf — sonst
erschiene die Meldung auf dem Alternativschirm und verschwände sofort mit ihm.

Ctrl-C kommt im Rohmodus als gewöhnlicher Tastendruck an, nicht als Signal, und
wird dort behandelt.

---

## Ton

ffmpeg liefert rohes PCM in einen Ringpuffer, `cpal` gibt es aus. Der
Lesethread blockiert, wenn der Puffer voll ist -- das bremst ffmpeg auf
Abspieltempo, statt das ganze Stück in den Speicher zu laden.

Angefordert wird genau das Format, das die Soundkarte ohnehin will: ihre
Abtastrate, ihre Kanalzahl. Damit muss hier nichts umgerechnet werden; das
erledigt ffmpeg, das es besser kann.

**Der Ton ist die Leit-Uhr.** Der Audio-Rückruf zählt, was die Soundkarte
tatsächlich abgeholt hat -- das ist die verlässlichste Zeitquelle im Programm,
weil sie an echter Hardware hängt und nicht an einer Schätzung. Weicht die
Bild-Uhr um mehr als 150 ms davon ab, wird sie nachgezogen. Kleiner wäre
unruhig (Puffergrößen schwanken ohnehin), größer wäre als Versatz sichtbar.

Nachgezogen wird aber **nur, solange der Ton auch läuft**: Geprüft wird, ob die
Abspielposition seit dem letzten Durchgang überhaupt gewachsen ist. Ohne diese
Bedingung würde ein stockender oder leergelaufener Ton das Bild mitreißen und
einfrieren lassen -- man hätte einen Fehler gegen einen schlimmeren getauscht.

Läuft der Puffer leer, füllt der Rückruf mit Stille auf, statt zu knacken, und
zählt die Stille **nicht** mit. Sonst liefe die Uhr in einer Unterdeckung davon
und das Bild zöge nach.

Pause, Spulen und Lautstärke sind Zahlen in einem gemeinsamen Zustand und
wirken sofort. Das war vorher anders: über einen `ffplay`-Prozess, dessen Uhr
sich von außen weder auslesen noch steuern lässt, mussten alle drei den Prozess
neu starten -- hörbar als Lücke. Der Umbau war ohnehin nötig, weil ffplay eine
eigene Programmdatei von 231 MB ist und die mitgelieferte Fassung sonst doppelt
so groß geworden wäre.

Kamera und stdin bekommen keinen Ton: die Kamera hat keinen, und stdin lässt
sich nicht von zwei Prozessen lesen.

Portale liefern Bild und Ton getrennt — bei YouTube ist das der Normalfall.
`Input` trägt deshalb ein eigenes Feld `audio_input`; das Bild geht an ffmpeg,
der Ton an die Tonausgabe. Bei `-f A+B` gibt yt-dlp die Adressen in der Reihenfolge des
Selektors aus, also erst Bild, dann Ton.

Daran hing eine Falle, die erst beim Zuhören auffiel: `probe` befragt die
*Bild*-Adresse, und die trägt bei YouTube naturgemäß keine Tonspur. `has_audio`
blieb deshalb `false` und der Ton wurde nie gestartet — das Video lief stumm,
ohne jede Fehlermeldung. `MediaInfo::mit_tonspur` korrigiert das: eine zweite
Adresse gibt es nur, weil der Selektor mit `ba` ausdrücklich Ton angefordert
hat, sie ist also der verlässlichere Hinweis als das Probe-Ergebnis.

Lehre daraus für die Statuszeile: sie unterscheidet jetzt `Ton 100%`,
`Ton stumm` und `ohne Ton`. Ein stummes Video ohne jede Anzeige lässt den
Benutzer im Dunkeln, woran es liegt.

---

## Wo man ansetzt

**Einen neuen Modus ergänzen** — vier Stellen:

1. `src/render/<name>.rs`: `Renderer` implementieren. Erste Zeile im `render`
   ist immer `if !frame.matches(layout) { return; }` — nach einer
   Größenänderung sind noch Bilder der alten Größe unterwegs, und die zu
   rendern ergäbe Pixelsalat.
2. `src/render/mod.rs`: Modul eintragen, Variante in `Mode` ergänzen.
3. `src/main.rs`: `make_renderer` erweitern.
4. `src/control/mod.rs`: Taste in `map_key` belegen.

Die Bausteine in `render/mod.rs` nehmen die Arbeit ab: `tile_mean_linear`,
`subtile_means` (teilt eine Zelle ganzzahlig, auch krumm wie 8 auf 3),
`adjust_linear`, `perceptual_luma`, `ramp_char`, `to_rgb`.

**Einen Zeichensatz ergänzen:** `Charset` in `render/mod.rs`, Unterteilung in
`grid_of` und die Zeichenwahl in `render/blocks.rs`.

**Eine Quellenart ergänzen:** `classify` in `src/source/input.rs`. Alles Weitere
folgt aus `pre_args` und `is_live`.

**Am Durchsatz schrauben:** `SKIP_MIN` in `src/term/writer.rs`, und mit
`--stats` und `--bench-naive` nachmessen statt raten.

---

## Eine Datei, die überall läuft

`cargo build --release --features bundled` packt ffmpeg und yt-dlp in die
Programmdatei. Ergebnis: eine Datei, die auf einem Rechner läuft, auf dem
nichts installiert ist -- einschließlich YouTube. Ohne die Eigenschaft bleibt
es bei 1,6 MB.

Die beiden sind unterschiedlich verbindlich: **ohne ffmpeg geht gar nichts**,
deshalb bricht der Bau ohne es ab. **yt-dlp braucht nur, wer Portal-Links
abspielt** -- fehlt es, entsteht eine Exe ohne, die das beim Bau meldet und zur
Laufzeit nur bei Portal-Links etwas sagt.

Gemessen wurde vorher, was das kostet:

| | |
|---|---|
| ffmpeg.exe (Gyan full build) | 231 MB |
| dasselbe zstd-gepackt | 71 MB (38 s bei Stufe 12) |
| yt-dlp.exe | 17 MB, gepackt kaum kleiner (intern schon komprimiert) |
| **fertige Exe mit beidem** | **89 MB** |
| ffprobe.exe + ffplay.exe zusätzlich | +464 MB |

Die letzte Zeile ist der Grund, warum vorher zwei Abhängigkeiten
verschwinden mussten: mit drei Programmdateien wäre das Einpacken sinnlos
gewesen. ffprobe ist durch das Parsen von `ffmpeg -i` ersetzt, ffplay durch
cpal. Übrig bleibt eine Datei.

Der Ablauf:

* `build.rs` packt ffmpeg mit zstd (Stufe 12 -- darüber wächst die Bauzeit
  stark, ohne dass die Datei nennenswert schrumpft) und erzeugt daneben ein
  Kennzeichen des Inhalts. Das Ergebnis wird zwischengespeichert; der zweite
  Bau überspringt das Packen.
* Beim ersten Start wird nach `%LOCALAPPDATA%idinshfmpeg-<kennzeichen>.exe`
  entpackt: gemessen 2,7 Sekunden, jeder weitere Start 0,1 Sekunden. Das Kennzeichen im Dateinamen sorgt dafür, dass eine neue Fassung
  ihr eigenes ffmpeg herausholt statt ein altes weiterzubenutzen.
* Geschrieben wird erst daneben, dann umbenannt -- zwei gleichzeitig gestartete
  vidinsh-Prozesse zerlegen sich sonst die Datei. Gewinnt der andere das
  Rennen, ist seine Datei genauso gut.
* Geprüft wird auch die Größe, nicht nur die Existenz: ein abgebrochenes
  Entpacken hinterlässt sonst eine halbe Datei, die bei jedem Start als fertig
  gilt.

Die Reihenfolge beim Suchen steht in `tools.rs`: `--ffmpeg` schlägt alles,
dann das mitgelieferte, zuletzt der PATH. Für yt-dlp entsprechend:
mitgeliefert, `tools/` neben der Programmdatei, `tools/` im
Arbeitsverzeichnis, PATH.

**Beim Prüfen beide Bauformen übersetzen.** Der Code hinter
`#[cfg(feature = "bundled")]` wird vom schlanken Bau gar nicht angefasst --
ein Tippfehler darin fällt dort nicht auf, auch nicht bei `cargo test` oder
`cargo clippy`. Genau so ist ein Vergleich `Result<u64, io::Error> == Ok(n)`
durchgerutscht, der nicht übersetzt (`io::Error` ist nicht vergleichbar).
Deshalb gehört zu jeder Prüfung:

```bash
cargo clippy --all-targets                     # schlank
cargo clippy --all-targets --features bundled  # mitgeliefert
```

Das Packen ist zwischengespeichert, der zweite Aufruf kostet also kaum Zeit.

---

## Tests

135 Tests, alle ohne Terminal und ohne Netz lauffähig (`cargo test`, ~0,4 s).
Testmaterial erzeugt ffmpeg lokal:

```powershell
ffmpeg -f lavfi -i testsrc2=size=1280x720:rate=30 -t 10 testdata\test.mp4
ffmpeg -f lavfi -i smptebars=size=1280x720:rate=30 -t 5 testdata\bars.mp4
```

Die Tests prüfen Verhalten, nicht Implementierung: dass ein Verlauf aufsteigende
Zeichendichte ergibt, dass ein unveränderter Frame **null** Zellen schreibt,
dass ein Farbwechsel ein Vollbild erzwingt, dass eine Pause aus der Uhr
herausgerechnet wird, dass jedes Sextanten-Muster auf ein eigenes Zeichen
zeigt, dass ein hängender Prozess am Zeitlimit stirbt.
