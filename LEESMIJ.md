# StudioRecorder 0.9.1

Eenvoudige opname-app voor de radiostudio: opnemen met live waveform en VU-meter, daarna begin en eind trimmen, foutjes eruit knippen, exporteren naar FLAC, WAV of MP3 en aanleveren bij AudioShare.

## Starten

Download de recorder in AudioShare onder **Recorders**. Je krijgt een zip met `StudioRecorder.exe` en jouw persoonlijke koppelbestand.
Pak de zip uit in een vaste map (bijv. `C:\StudioRecorder`) en dubbelklik op de exe; het koppelscherm staat dan meteen klaar (zie *AudioShare* hieronder).
`StudioRecorder.exe` is één los bestand zonder installatie.
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

**Koppelen** (eenmalig per maker en per pc).
- Makkelijkst: download de recorder zelf in AudioShare → *Recorders*. Staat er bij de eerste start een `.audioshare`-bestand naast de exe,
  dan opent de recorder meteen *Koppelen met AudioShare* met jouw gegevens ingevuld; je kiest alleen nog een pincode.
  Na het koppelen krijgt het bestand de extensie `.gebruikt`, zodat het niet opnieuw wordt aangeboden. Een verlopen bestand wordt overgeslagen.
- Ook mogelijk: maak in AudioShare onder *Recorders* een koppelcode of koppelbestand aan (de admin kan dat ook voor je doen).
  Dubbelklik op het `.audioshare`-bestand, sleep het op het venster, of klik bovenin op **Koppelen met AudioShare** en vul adres en code in.
- De recorder toont daarna bijvoorbeeld: *Gekoppeld als Jan de Vries — 2 programma's: De Blik uur 1, De Blik uur 2*.
- De programmakeuze op het opnamescherm komt dan uit AudioShare; titel en omschrijving worden voorgesteld (paarse melding).

**Nieuwe versie**: na het inloggen kijkt de recorder in AudioShare of er een nieuwere versie is (vanaf AudioShare 1.4.0) en meldt dat één keer.
Downloaden gaat via AudioShare → *Recorders*; je koppelingen en instellingen in `%APPDATA%\StudioRecorder` blijven behouden.

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

## Studio (vaste studio-pc, vanaf 0.8.0)

Op een studio-pc waar verschillende technici en makers werken, koppel je de **pc als studio** in plaats van iedere persoon apart.

1. De admin maakt in AudioShare onder *Recorders → Studio's* een studio aan (bijv. "Studio 2") en vinkt aan welke programma's die studio mag aanleveren.
2. De admin klikt op **Recorder downloaden** en geeft de zip aan de studiobeheerder.
3. De studiobeheerder pakt de zip uit op de studio-pc en start de recorder. Het koppelscherm vraagt de **naam en beheerpincode van de studiobeheerder**.
4. Onder **Instellingen → AudioShare → Gebruikers** maakt de studiobeheerder de technici en makers aan: naam, pincode en welke programma's (uit de lijst van de studio) ieder mag uploaden.

Daarna:
- Bij het starten is niemand ingelogd. **Inloggen**: kies je naam en vul je pincode in. Je ziet alleen je eigen programma's.
- Bij elke upload gaat je naam mee als **bediend door**; die staat in AudioShare bij de aflevering.
- De koppeling met AudioShare hoort bij de studio, niet bij een persoon. Het token staat in Windows Credential Manager, dus uploads in de wachtrij gaan door, ook als iedereen is uitgelogd of de recorder opnieuw is gestart.
- Alleen een studiobeheerder mag de instellingen, de invoer en de gebruikers wijzigen, en ziet alle uploads. Een gewone gebruiker ziet alleen zijn eigen uploads.
- 5 foute pincodes: 5 minuten wachten. Pincode vergeten: de studiobeheerder stelt een nieuwe in (Gebruiker → Wijzigen).
- Haalt de admin in AudioShare een programma bij de studio weg, dan verdwijnt het ook bij de gebruikers.
- Studio opnieuw koppelen (nieuwe code van de admin): gebruikers en instellingen blijven behouden.

## Automatische opname (vanaf 0.9.0)

Een eigen script of planner start en stopt de opname via UDP. Er hoeft niemand bij de recorder te zitten of ingelogd te zijn.

**Aanzetten** (alleen een beheerder): *Instellingen → Automatische opname → Instellen…* en vink **Automatische modus aan**.
Op het opnamescherm staat dan een groene balk: *AUTOMATISCH — wacht op START (UDP 47820)*. Handmatig opnemen kan niet zolang de modus aan staat.

**Commando's** (één per UDP-bericht, hoofdletters maken niet uit). De recorder antwoordt altijd naar de afzender:

| Bericht | Antwoord |
|---|---|
| `START` | `OK RECORDING <programma>` of `ERR <CODE> …` |
| `STOP` | `OK STOPPED 00:58:12` of `ERR NOT_RECORDING` |
| `STATUS` | `OK RECORDING <programma> 00:12:03` of `OK IDLE` |

Foutcodes: `ALREADY_RECORDING`, `NOT_RECORDING`, `UNKNOWN_PROGRAM`, `NO_INPUT`, `BAD_KEY`, `BAD_COMMAND`.
Komt er geen STOP, dan stopt de opname zelf na de **maximale opnameduur** (standaard 60 minuten, instelbaar).

**Verbinding**: poort (standaard 47820), toegestane afzenders (standaard alleen `127.0.0.1` = deze pc; `*` = iedereen)
en een optionele sleutel. Met een sleutel moet elk bericht `key=<sleutel>` bevatten, bijv. `START key=geheim`.

**Welk programma?** De recorder kijkt op volgorde (elk aan/uit te zetten):
1. het bericht zelf: `START DeBlik1` (slug, naam of id uit AudioShare);
2. het **weekrooster**: dagen, van–tot en programma. START mag 5 minuten vóór het begin; een blok over middernacht telt voor de dag waarop het begint;
3. het **vaste programma**.
Vindt hij niets, dan neemt hij niet op (`ERR UNKNOWN_PROGRAM`).

**Na STOP, per programma**: *Alleen opslaan* (standaard), *Alleen uploaden* of *Opslaan + uploaden*.
- Opslaan: in de gekozen map (standaard `<opnamemap>\Automatisch\<programma>`), als FLAC, WAV of MP3 320k.
  **Bestandsnaam** (vanaf 0.9.1): leeg = de titel, of een vaste naam zoals `testprogramma` (ook met `{datum}` enz.).
  Met **Bestaand bestand overschrijven** is het steeds hetzelfde bestand: eerst volledig naar een tijdelijk bestand, dan in één keer omgewisseld.
  Is het oude bestand in gebruik (bijv. door de playout), dan blijft het staan en krijgt de nieuwe opname (2) erachter, met een melding in het logboek.
- Uploaden: als MP3 320k via de uploadwachtrij, met de koppeling van de studio. In AudioShare staat *bediend door: Automatisch*.
  Kan alleen als de pc als studio is gekoppeld en het een AudioShare-programma van de studio is.
- Uitzenddatum = de dag van de opname. Titel volgens een patroon (standaard `{programma} — {datum}`, ook `{tijd}` en `{week}`).
  Omschrijving: die van de vorige aflevering uit AudioShare, of een vaste tekst. De bestandsnaam is de titel.
- Staat er al een aflevering op die datum, dan wacht de upload op een bevestiging in het tabblad Uploads (er wordt niets automatisch vervangen).
- Het originele 24-bit WAV-bestand blijft altijd in de opnamemap staan.

Alles komt in het **logboek onderaan het venster Automatische opname** (vanaf 0.9.1, ook zonder AudioShare-koppeling), ook genegeerde berichten van niet-toegestane afzenders.
De automatische modus werkt ook als het venster geminimaliseerd is. Een automatische opname kun je met de rode knop stoppen; hij wordt dan verwerkt zoals bij STOP.

Testen vanuit PowerShell op dezelfde pc:

```powershell
$u = New-Object System.Net.Sockets.UdpClient
$b = [Text.Encoding]::UTF8.GetBytes("START"); $u.Send($b, $b.Length, "127.0.0.1", 47820)
$b = [Text.Encoding]::UTF8.GetBytes("STOP");  $u.Send($b, $b.Length, "127.0.0.1", 47820)
```

Of met Python (toont ook het antwoord):

```python
import socket
s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM); s.settimeout(2)
s.sendto(b"START", ("127.0.0.1", 47820)); print(s.recvfrom(1024)[0].decode())
```

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
- Dezelfde broncode bouwt ook de **ArchiefRecorder** (`src/bin/archiefrecorder.rs`, zie `LEESMIJ-ArchiefRecorder.md`).
  De StudioRecorder werkt ongewijzigd: export, MP3 320k (stand *Best*), AudioShare en instellingen zijn gelijk aan 0.8.0.
- `src/engine.rs`: invoer, meters en crashveilig wegschrijven · `src/auto.rs` + `src/auto_ui.rs`: automatische opname via UDP · `src/wav.rs`: WAV/RF64 lezen en schrijven ·
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
