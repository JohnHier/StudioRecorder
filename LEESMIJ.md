# StudioRecorder 0.6.5

Eenvoudige opname-app voor de radiostudio: opnemen met live waveform en VU-meter, daarna begin en eind trimmen, foutjes eruit knippen, exporteren naar FLAC, WAV of MP3 en aanleveren bij AudioShare.

## Starten

`StudioRecorder.exe` is één los bestand zonder installatie. Kopieer het naar elke studio-pc en dubbelklik.
Windows SmartScreen kan de eerste keer waarschuwen ("onbekende uitgever"): kies *Meer informatie → Toch uitvoeren*.

## Opnemen

1. Kies bij **Invoer** de geluidskaart of mengtafel. De meter loopt meteen mee, zodat je de niveaus vóór de opname kunt controleren.
   *Computergeluid: …* neemt op wat de pc zelf afspeelt (loopback).
2. Rechts staat wie er is ingelogd en voor welk programma (zie *Inloggen en uitloggen* hieronder).
   Heeft de maker één programma, dan staat dat al klaar; bij meerdere kies je er een (de vorige keuze wordt onthouden).
   Uitzenddag (vandaag), titel en beschrijving vul je na de opname in bij **Bewerken**, onder *Na export uploaden naar AudioShare*.
3. Klik de grote **rode knop**. De opname gaat als 24-bit WAV naar de opnamemap, met naam `<programma>_<datum>_<tijd>.wav`.
   De programmagegevens worden ernaast bewaard in een `.json`-bestand met dezelfde naam.
4. **Pauze** (de ronde knop naast de rode): tijdens de pauze wordt er niets opgenomen, de meter loopt wel door.
   Het scherm en de klok tonen oranje *GEPAUZEERD*. Klik nogmaals om te hervatten; het wordt één doorlopende opname.
5. Klik de rode knop om te stoppen (met bevestiging). De opname opent direct in **Bewerken**, met de ingevulde gegevens.

**Klein venster**: tijdens het opnemen kun je de recorder klein zetten, zodat je iets anders op de pc kunt doen.
Je houdt een smalle balk die altijd bovenop blijft, met REC-lampje, tijd, een kleine meter, **Pauze**, **Stop** en **Groot**
(terug naar het volledige venster). Na stoppen gaat het venster vanzelf weer groot.

Veiligheid tijdens het opnemen:
- Het bestand wordt elke seconde bijgewerkt; bij een crash of stroomstoring ben je hooguit een paar seconden kwijt. Open het bestand daarna gewoon in Bewerken.
- Opnames langer dan ca. 4 uur worden automatisch RF64 (de broadcast-standaard voor grote WAV-bestanden).
- De pc gaat tijdens een opname niet in slaapstand.
- Afsluiten tijdens een opname vraagt eerst om bevestiging.
- Meldingen bij: geen signaal (10 s stilte), weggevallen apparaat, haperingen, weinig schijfruimte.
- De CLIP-lampjes blijven branden tot je op de meter klikt.

## Bewerken

**Begin en eind (IN / UIT)**
- Sleep de groene **IN**- en oranje **UIT**-markers (aan de lijn of aan het vlaggetje).
- **Cursor is IN-punt** / **Cursor is UIT-punt** zetten IN of UIT op de afspeelpositie (ook met de toetsen I en O).
- **Beluister vanaf IN** (links) en **Beluister laatste 3 s** (rechts, stopt precies op UIT) om de knippunten te controleren.

**Een stuk eruit knippen**
1. Houd **Shift** ingedrukt en sleep in de golfvorm over het stuk dat weg moet. Het wordt wit gemarkeerd; de randen (met de witte handgrepjes) kun je nog verslepen.
2. **Beluister de knip** speelt 3 seconden ervoor en meteen 3 seconden erna, dus zoals het na het knippen klinkt.
3. **Knip weg** (of Delete). Het stuk wordt rood en wordt bij afspelen en exporteren overgeslagen.
4. Begin of eind van een rood stuk aanpassen: sleep de rode rand (met de handgrepjes). Daarna opnieuw **Beluister de knip**.
5. Klik op een rood stuk om het opnieuw te beluisteren of **terug te zetten**. **Ongedaan maken** (Ctrl+Z) draait de laatste knip of randverschuiving terug.

Op elk knippunt komt bij het exporteren een crossfade van 10 ms, zodat er geen tik te horen is. De originele opname wordt nooit aangepast.

**Navigeren**
- **Klik** in de golfvorm: afspeelpositie. **Slepen**: de afspeelpositie verschuiven (ook tijdens afspelen). **Scrollwiel**: in-/uitzoomen (tot op sample-niveau).
- **Shift + scrollwiel**, rechtermuisknop slepen of klikken in de overzichtsbalk: verschuiven.

**Exporteren** maakt een nieuw bestand (FLAC, WAV of MP3) van IN tot UIT, zonder de weggeknipte stukken, in dezelfde map.
MP3 wordt gemaakt in de hoogste MP3-kwaliteit: 320 kbps.
Ben je ingelogd en staat **Na export uploaden naar AudioShare** aan, dan verschijnen daaronder programma, uitzenddag, titel en
beschrijving (programma, uitzenddag en titel zijn dan verplicht) en wordt de bestandsnaam `<Programma> <uitzenddag> - <titel>`.
Niet ingelogd of uploaden uit: dan zie je die velden niet en is het gewoon opslaan.
FLAC-tags: TITLE (titel), ALBUM en ARTIST (programma), DATE (uitzenddag), DESCRIPTION (beschrijving), ORGANIZATION (studionaam).
Met *Korte fade (10 ms)* krijgen begin en eind een minifade.
Een WAV-, MP3- of FLAC-bestand openen kan via **Bestand openen…** of door het op het venster te slepen.
MP3 en FLAC worden eerst omgezet naar een WAV-werkkopie in de tijdelijke map (FLAC blijft daarbij bit voor bit gelijk);
het origineel wordt nooit aangepast en de export komt in de map van het origineel.

| Toets | Functie |
|---|---|
| Spatie | Afspelen / pauze (slaat weggeknipte stukken over) |
| Delete | Selectie wegknippen |
| Ctrl+Z | Laatste knip ongedaan maken |
| Esc | Selectie opheffen |
| I / O | IN / UIT op afspeelpositie |
| Home / End | Naar IN / UIT |
| + / − | In- / uitzoomen |

## AudioShare

**Koppelen** (eenmalig per maker en per pc). De beheerder maakt in AudioShare onder *Recorders* een koppelcode of koppelbestand aan.
- Dubbelklik op het `.audioshare`-bestand, sleep het op het venster, of klik bovenin op **Koppelen met AudioShare** en vul adres en code in.
- De recorder toont daarna bijvoorbeeld: *Gekoppeld als Jan de Vries — 2 programma's: De Blik uur 1, De Blik uur 2*.
- De programmakeuze op het opnamescherm komt dan uit AudioShare; titel en omschrijving worden voorgesteld (paarse melding).

**Pincode**: bij het koppelen kies je een pincode (minimaal 4 cijfers) waarmee je voortaan inlogt.
- 5 foute pincodes: 5 minuten op slot. 10 foute: opnieuw koppelen met een nieuwe code. Pincode vergeten = nieuwe koppelcode.
- Iedere maker ziet alleen zijn eigen uploads en logboek; een beheerder ziet alles.

**Uploaden**
- In Bewerken staat **Na export uploaden naar AudioShare** aangevinkt. Ernaast staat vooraf of de export aan de normen van het programma voldoet (formaat, grootte, duur, samplerate).
- Een deel mag maximaal 200 MB zijn. Een uur FLAC is groter; kies daarom **MP3 320k** voor het aanleveren.
- Na de export gaat het bestand in de uploadwachtrij (tabblad **Uploads**): aflevering aanmaken, uploaden met controlegetal, vrijgeven. Waarschuwingen (bijv. te laat aangeleverd) verschijnen in oranje.
- Geen verbinding? De recorder probeert het opnieuw na 10 s, 30 s, 1 min en daarna elke 5 minuten, ook na een herstart.
- Staat er al een eigen aflevering op die datum, dan vraagt de recorder of je wilt vervangen, met een waarschuwing als een station de oude al heeft gedownload. Van een andere maker kun je niets vervangen.
- Uur 1 en uur 2 zijn aparte opnames met elk hun eigen programma.

## Inloggen en uitloggen

- Bij het starten is er **niemand ingelogd**. Dan kun je opnemen, een bestand openen, bewerken en opslaan (exporteren).
  Je kunt geen programma kiezen en niet uploaden.
- **Inloggen** (rechtsboven of op het opnamescherm): kies je naam en vul je pincode in. Daarna staat je programma klaar
  en kun je uploaden. Welke programma's je hebt, stelt de beheerder in AudioShare in.
- **Uitloggen** rechtsboven (of op het opnamescherm als er geen opname loopt). Na 15 minuten zonder activiteit
  log je ook vanzelf uit, en na het sluiten van de recorder ben je altijd uitgelogd.
- Een koppeling uit een eerdere versie (zonder pincode) vraagt bij de eerste keer inloggen om een nieuwe pincode.
- Iemand met de rol *admin* in AudioShare is op deze pc **beheerder** (rechtsboven staat *(beheerder)*).
  Zodra er een beheerder gekoppeld is, mogen alleen beheerders de **Instellingen** openen en de **invoer** wijzigen;
  een beheerder ziet ook alle uploads en het hele logboek.
- Is er nog geen beheerder gekoppeld (eerste installatie), dan mag iedereen alles. Koppel dus eerst de beheerder.

## Instellingen

Via **Instellingen** (alleen beheerder): studionaam (FLAC-tag), opnamemap, afspeelapparaat, de lijst met programma's en het exportformaat.
Ze worden per pc opgeslagen in `%APPDATA%\StudioRecorder\settings.json`, dus elke studio houdt zijn eigen configuratie.

## Techniek

- Rust, met egui (interface), cpal (WASAPI-audio), flacenc (FLAC), LAME (MP3) en ureq + Windows-TLS (AudioShare).
- `src/engine.rs`: invoer, meters en crashveilig wegschrijven · `src/wav.rs`: WAV/RF64 lezen en schrijven ·
  `src/export.rs`: trimmen naar FLAC/WAV · `src/player.rs`: afspelen · `src/app.rs`: interface.
- Bouwen op Windows: installeer Rust (rustup.rs) en voer `cargo build --release` uit in deze map.
- Tests: `cargo test` (o.a. bit-exacte FLAC-export, herstel na een crash en de volledige AudioShare-flow).
- `testserver/audioshare_mock.py`: testserver volgens koppelspecificatie v0.5, voor testen zonder het echte platform.

### Afwijkingen van de koppelspecificatie (alleen recorderkant, het contract met AudioShare is ongewijzigd)
- Geen installer: de recorder registreert `.audioshare` zelf bij de eerste start (per gebruiker, zonder adminrechten).
- Geen `audioshare.ini`: adres en profielen staan in `%APPDATA%\StudioRecorder\audioshare.json`.
  Het token staat versleuteld met de pincode van de maker (Argon2id + AES-256-GCM); zonder pincode is het niet te lezen.
- Wachtrij en logboek: `%APPDATA%\StudioRecorder\uploads.json`.
- Nog niet gebouwd (volgende stappen): bezette data in de kalender en vrije datum voorstellen, LUFS-meting vooraf,
  sjablonen, overzicht 8 weken, metadata wijzigen/verwijderen, deadline-bewaking.
