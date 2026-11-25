# ArchiefRecorder 1.1.0

Opname-app voor het digitaliseren van cassettes en banden: opnemen, knippen, het volume gelijktrekken en
exporteren naar MP3, FLAC of WAV. Gebouwd op dezelfde basis als de StudioRecorder, maar zonder AudioShare.

## Starten

`ArchiefRecorder.exe` is één los bestand zonder installatie. Zet het in een vaste map en dubbelklik.
Instellingen staan apart van de StudioRecorder in `%APPDATA%\ArchiefRecorder`; opnames standaard in
`Muziek\ArchiefRecorder`. Beide programma's kunnen naast elkaar op dezelfde pc staan.

## Werkwijze

1. **Opnemen**: kies de invoer, druk op de rode knop en neem een hele kant van de cassette in één keer op.
   De opname gaat als 24-bit WAV naar de opnamemap (crashveilig, zoals bij de StudioRecorder).
   Met **Automatisch stoppen** hoef je er niet bij te blijven (zie hieronder).
2. **Bewerken**: begin en eind trimmen en stukken eruit knippen werkt precies zoals in de StudioRecorder.
   Ook een bestaand WAV-, MP3- of FLAC-bestand openen kan.
3. **Volume**: zodra de opname open staat, meet de recorder het volume van het hele bestand (balk rechts in
   het paneel *Volume*). Daarna zie je de **oranje lijn** over de golfvorm: zoveel dB gaat het volume op dat
   moment omhoog of omlaag. Met de muis boven de golfvorm zie je de precieze waarde.
4. **Beluisteren** klinkt meteen zoals de export: met leveler en piekbegrenzer. Vink **Beluister origineel**
   aan om te vergelijken.
5. **Exporteren**: kies het formaat, vul eventueel titel, artiest/collectie, datum en omschrijving in
   (die komen in de tags en in de bestandsnaam) en klik *Exporteren*. De export komt in dezelfde map als het origineel.
   Het origineel wordt nooit aangepast.

## Automatisch stoppen (nieuw in 1.1)

Rechts op het opnamescherm. De opname stopt bij wat het eerst gebeurt:
- **Tijd**: *C60* (31 min), *C90* (46 min), *C120* (61 min): de lengte van één kant plus een minuut speling,
  of een eigen aantal minuten. *Uit* = niet op tijd stoppen.
- **Stilte**: *Of na 30 s stilte (einde bandje)*. Bandruis telt als stilte, een stem niet. Dit werkt pas nadat er
  geluid is geweest, dus je kunt rustig eerst op opnemen drukken en dan pas op play op het deck.
  Bij interviews met lange stiltes: zet het aantal seconden hoger.

Tijdens de opname staat er in oranje wanneer hij stopt. Na het stoppen opent de opname in Bewerken met het
**UIT-punt al vlak na het laatste geluid**: de lege staart van het bandje hoef je niet weg te knippen.

## Ruis verwijderen (AI-stemisolatie, nieuw in 1.1)

In het paneel boven het exporteren: **Ruis verwijderen (AI-stemisolatie)**. Dit gebruikt **DeepFilterNet3**, een
AI-model dat gemaakt is voor spraak: het houdt de stem over en haalt bandruis, brom en geknister weg, net als
*Voice Isolation* in DaVinci Resolve. Het model zit in het programma; er gaat niets naar internet.

| Sterkte | Wat |
|---|---|
| Licht | Ruis 12 dB zachter; klinkt het meest natuurlijk |
| Middel | Ruis 24 dB zachter (standaard) |
| Sterk | Ruis helemaal weg; kan wat "droog" klinken |

- Na het aanzetten maakt de recorder op de achtergrond een opgeschoonde werkkopie (balk rechts). Op een gewone
  pc duurt een uur interview enkele minuten. Golfvorm, leveler, beluisteren en exporteren gebruiken daarna de schone versie.
- **Beluister origineel** speelt de opname zoals hij van het bandje kwam, om te vergelijken.
- Eerst de ruis weg, dan de leveler: de leveler meet dus op de schone stem.
- Bedoeld voor **spraak**. Op muziek kan het vreemd klinken; zet het daar uit.
- Werkt met elke samplerate (intern 48 kHz); knippunten blijven precies op hun plek.

## Volume gelijktrekken (leveler)

De leveler werkt achteraf en kan dus vooruit én achteruit kijken. Hij kijkt naar het **hardste niveau in de
omgeving** (bij *Normaal* 8 seconden ervoor en erna) en regelt naar het doelniveau. Daardoor:
- worden verschillen tussen nummers, kanten en opnames gelijkgetrokken;
- blijven zachte stukken en fades binnen een nummer zoals ze zijn (de muziek wordt niet "platgeslagen");
- begint de regeling al vóór een hard of zacht stuk, zodat je hem niet hoort pompen;
- worden stiltes en bandruis tussen nummers **niet** opgeblazen (stiltedrempel).

| Instelling | Standaard | Betekenis |
|---|---|---|
| Doel | −16 LUFS | Hoe luid het resultaat wordt |
| Rustig / Normaal / Snel | Normaal | Kijkt 20 / 8 / 3 seconden vooruit en achteruit |
| Max. versterking | 12 dB | Nooit meer versterken dan dit (heel zachte opnames worden niet met ruis en al opgeblazen) |
| Stiltedrempel | −45 LUFS | Zachter dan dit telt als stilte of ruis. Hoor je ruis opkomen tussen nummers? Zet hem op bijv. −40 |
| Plafond (piekbegrenzer) | −1 dB | Hoogste piek in het bestand |

**Piekbegrenzer**: ziet pieken 5 ms van tevoren aankomen en zet het volume er vloeiend net vóór terug,
zodat niets boven het plafond komt en niets vervormt.

*Standaardwaarden* zet alles terug.

## Exporteren

- **MP3 192k** (standaard) is ruim voldoende voor cassettes; **MP3 320k**, **FLAC** en **WAV** kunnen ook.
- **Mono**: beide kanalen samen in één. Veel cassettes zijn (bijna) mono; het bestand wordt dan half zo groot.
- MP3 wordt gemaakt in LAME-stand *NearBest*: de door LAME aanbevolen hoge kwaliteit, ongeveer **2,7 keer
  sneller** dan de stand *Best* die de StudioRecorder gebruikt.
- Tags: TITLE, ARTIST en ALBUM (artiest/collectie), DATE, DESCRIPTION. Bestandsnaam: `Artiest - Titel`.

## Techniek

- Zelfde broncode als de StudioRecorder: `src/main.rs` bouwt `StudioRecorder.exe`,
  `src/bin/archiefrecorder.rs` bouwt `ArchiefRecorder.exe`. `src/edition.rs` bepaalt welke het is.
- `src/denoise.rs`: stemisolatie met DeepFilterNet3 (crate `deep-filter-rt`, model ingebouwd, pure Rust), met
  omzetting naar en van 48 kHz (`rubato`); tests op uitlijning en ruisvermindering bij 48 kHz stereo en 44,1 kHz mono.
- `src/level.rs`: luidheidsmeting (ITU-R BS.1770 / EBU R128), versterkingscurve en look-ahead peak limiter, met tests.
- De leveler en piekbegrenzer worden toegepast in `src/export.rs` (exporteren) en `src/player.rs` (beluisteren).
- Bouwen: `cargo build --release` (beide), of `--bin archiefrecorder`. Vanaf Linux: `build-win.sh` (Rust 1.97;
  `deep-filter-rt` vraagt minimaal 1.96).
- Tests: `cargo test` (o.a. leveler op een "cassette" met zacht stuk, ruis en hard stuk: nooit boven het plafond,
  ruis blijft laag, verschil van 25 dB teruggebracht tot 7 dB).
