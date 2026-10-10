# bridge-bot

Ein in Rust geschriebener Bridge-Bot, der Textnachrichten zwischen **Telegram**, **Reticulum LXMF** und **Reticulum Relay Chat (RRC)** weiterleitet. Verbinde Gruppen, Forum-Themen, LXMF-Peers und RRC-Räume über ausdrücklich konfigurierte Routen.

```text
Telegram ↔ bridge-bot ↔ LXMF / RRC
```

### Funktionen

- Einseitige und bidirektionale Routen, auch mit mehreren Zielen.
- Explizite Zuordnung von Telegram-Themen zu RRC-Räumen.
- Eingebauter Reticulum-Stack mit eigenen Interfaces und Identitäten pro Dienst.
- Mehrere Instanzen desselben Dienstes, etwa verschiedene Telegram-Bots oder RRC-Hubs.
- Begrenzte Versandwiederholungen im Arbeitsspeicher und Diagnosewerkzeuge.

### Ausblick

**More services might be added in the future.** Die Adapter-Struktur ermöglicht die Erweiterung um weitere Dienste; aktuell werden Telegram, LXMF und RRC unterstützt.

Weitergeleitete Nachrichten beginnen mit:

```text
Alice wrote via Telegram:
Hallo!
```

## Einrichtung

Voraussetzung ist eine aktuelle stabile Rust-Toolchain mit Unterstützung für die Edition 2024 sowie Zugriff auf die gewünschten Netzwerke.

1. Telegram-Bot bei @BotFather erstellen und sein Token in `bridge-bot.toml` unter `[services.telegram]` als `token` eintragen. Den Bot der gewünschten Gruppe oder dem Kanal hinzufügen. Für Gruppen den Privacy Mode bei BotFather deaktivieren (danach gegebenenfalls neu hinzufügen). Für Kanäle benötigt er Administratorrechte und die Berechtigung, Nachrichten zu veröffentlichen. Private Chats müssen vom Nutzer begonnen werden.
2. Für LXMF und RRC die gewünschten Reticulum-Interfaces direkt im jeweiligen Dienst konfigurieren. Beide Dienste betreiben ihren eigenen Rust-Reticulum-Stack im Bot-Prozess; eine separate Installation oder ein LXMF-Daemon ist nicht erforderlich.
3. Die Beispielkonfiguration kopieren und echte Telegram-Chat-IDs, LXMF-Peer-Destination-Hashes und gegebenenfalls RRC-Hub-Adressen einsetzen. Die eigene LXMF-Delivery-Destination wird aus `identity_path` abgeleitet und beim Start geloggt. Die Beispieladressen sind Platzhalter.

```sh
cp bridge-bot.toml.example bridge-bot.toml
cargo run -- --check
cargo run --release
```

Die Konfiguration heißt standardmäßig `bridge-bot.toml` und enthält TOML. Ein anderer Pfad ist mit `--config datei.toml` möglich. `--check` validiert ausschließlich die Konfiguration, ohne Dienstverbindungen aufzubauen oder das Token bei Telegram zu prüfen. Die lokale Konfiguration ist in `.gitignore` ausgeschlossen. Das Telegram-Token wird direkt aus dem Feld `token` gelesen. Die optionale `[retry]`-Tabelle steht im Beispiel am Dateiende; ohne sie gelten die dort gezeigten Standardwerte.

Mit `cargo run --bin bridge-check` werden Telegram-Anmeldung, Zugriff auf die konfigurierten Gruppen, Forum-Status und Bot-Mitgliedschaft geprüft. Dabei werden keine Nachrichten gesendet oder Telegram-Updates abgeholt; der Token wird nicht ausgegeben. `Receives all group messages: false` bedeutet, dass der Privacy Mode aktiv ist. Als gewöhnliches Gruppenmitglied erhält der Bot dann nicht alle normalen Nachrichten. Bei BotFather `/setprivacy` auf `Disable` setzen und den Bot erneut hinzufügen, oder dem Bot Administratorrechte geben. Ein aktueller Themenname ist für das Routing nicht erforderlich; die konfigurierte `message_thread_id` muss zu einem existierenden Thema gehören.

## Schutz lokaler Daten

Veröffentliche ausschließlich `bridge-bot.toml.example` mit Platzhaltern. Die echte `bridge-bot.toml`, Konfigurationskopien, `.env`-Dateien und `.bridge-bot/` mit privaten Reticulum-Identitäten und Netzwerkzustand sind in `.gitignore` ausgeschlossen. Speichere eigene Identitäten ebenfalls unter `.bridge-bot/` und behandle sie wie private Schlüssel. Logs können Chat-Inhalte und Adressen enthalten und gehören nicht ins Repository.

## Routen

Mit `cargo run --bin bridge-check -- --rrc` lässt sich zusätzlich der Verbindungsaufbau zu konfigurierten RRC-Hubs samt Raumbeitritt prüfen. Es werden dabei keine Chatnachrichten gesendet. Der optionale Parameter `--rrc-test-message "Testnachricht"` sendet ausdrücklich eine Nachricht in den einzigen konfigurierten RRC-Raum und wartet auf dessen Hub-Echo. Für einen Weiterleitungstest muss gleichzeitig der Bridge-Bot laufen. Der Checker holt keine Telegram-Updates ab und konkurriert daher nicht mit dem Bot-Polling.

Für Telegram → RRC mit `--rrc --expect-rrc-text TESTMARKER --wait-secs 300` auf einen Marker warten und diesen in das konfigurierte Telegram-Thema schreiben. Bei aktivem Privacy Mode einen direkt adressierten Befehl wie `/bridge_test@BOT_USERNAME TESTMARKER` verwenden. Erst wenn der zweite RRC-Teilnehmer die weitergeleitete Nachricht tatsächlich empfängt, meldet der Checker den Test als bestanden.

Alle Logmeldungen des Bots erscheinen direkt im Terminal auf Standardausgabe, einschließlich Debug- und Trace-Meldungen. `RUST_LOG` kann zusätzlich Logs der verwendeten Bibliotheken aktivieren; die eigenen Bot-Meldungen bleiben immer sichtbar.

Dienste und Routen sind optional. Ohne konfigurierte Dienste loggt der Bot `no services registered` und beendet sich erfolgreich. Routen zu nicht konfigurierten Diensten bleiben inaktiv und lösen keine Fehlermeldungen aus. RRC ohne aktive Raumroute wird nicht gestartet. Fehler in ausdrücklich konfigurierten Diensten werden weiterhin gemeldet.

```toml
[[bridges]]
from = { service = "telegram", chat = "-1001234567890" }
to = { service = "lxmf", chat = "0123456789abcdef0123456789abcdef" }
bidirectional = true
```

`bidirectional = true` aktiviert beide Richtungen; `false` oder ein fehlender Eintrag aktiviert nur `from` → `to`. Mehrere Einträge erlauben mehrere Chats und mehrere Ziele. Gleiche Ziele werden pro Eingang nur einmal bedient. Nur konfigurierte Quell-Chats bzw. LXMF-Absender werden weitergeleitet.

Dienstnamen sind frei wählbar: Mehrere Telegram-Bots, LXMF-Identitäten oder RRC-Hubs können eigene Einträge unter `[services.NAME]` haben. Bei den Standardnamen `telegram`, `lxmf` und `rrc` wird der Adapter automatisch erkannt; `type` ist dort optional. Eigene Namen wie `[services.telegram_work]` benötigen beispielsweise `type = "telegram"`. Eine explizite Typangabe hat Vorrang vor dem Namen. LXMF-Namen werden in `[services.NAME.usernames]` nach Delivery-Destination zugeordnet; sonst verwendet der Bot den Namen aus der verifizierten Ankündigung oder die Adresse. Telegram nutzt Username, Anzeigename oder Kanalname.

## Themen und Räume

Räume werden explizit in `[[bridges]]` zugeordnet. Bei Telegram ist `chat` immer die erforderliche Chat-ID der Gruppe; `message_thread_id` ist zusätzlich die numerische ID des Themas innerhalb dieser Gruppe. Bei RRC wird der Raum ausschließlich im Feld `room` angegeben. Die Namen müssen nicht übereinstimmen:

```toml
[[bridges]]
from = { service = "telegram", chat = "-1001234567890", message_thread_id = 42 }
to = { service = "rrc", room = "#lobby" }
bidirectional = true

[[bridges]]
from = { service = "telegram", chat = "-1001234567890", message_thread_id = 57 }
to = { service = "rrc", room = "#news" }
bidirectional = true
```

Hier gelangen Nachrichten aus Thema 42 ausschließlich nach `#lobby`, aus Thema 57 nach `#news`; die Rückrichtung sendet jeweils in das konfigurierte Telegram-Thema. Für mehrere ausdrückliche Ziele können mehrere Routen mit derselben Quelle angelegt werden. Identische Ziele werden pro Nachricht dedupliziert. Andere Räume werden nicht automatisch ausgewählt.

Ohne `message_thread_id` wird in eine normale Telegram-Gruppe ohne Forum-Themen gepostet. Hat die Zielgruppe Themen, ist `message_thread_id` zwingend erforderlich, auch für `General` (`message_thread_id = 1`). Fehlt der Eintrag, protokolliert der Bot einen Fehler und sendet nichts. Umgekehrt ist eine konfigurierte Themen-ID in einer Gruppe ohne Themen ein Fehler. Nachrichten aus einem Telegram-Thema oder RRC-Raum ohne passende Quellroute werden mit einem Fehler protokolliert und nicht weitergeleitet. Ein Telegram-Eintrag ohne `message_thread_id` ist kein Platzhalter für alle Themen.

Themen-IDs bleiben auch bei Umbenennungen unverändert. Der Bot verwendet eingehende [Telegram-`message_thread_id`s](https://core.telegram.org/bots/api#message) direkt; er muss keine Namen lernen. Ist das konfigurierte Ziel geschlossen oder nicht verfügbar, wird die Weiterleitung mit einem Fehler beendet, ohne auf einen anderen Raum auszuweichen. Alle Nachrichtenteile und Versandwiederholungen bleiben im konfigurierten Ziel.

## Eigene Reticulum-Interfaces pro Dienst

`[[services.rrc.interfaces]]` und `[[services.lxmf.interfaces]]` sind unabhängige Listen. Auch mehrere Interfaces pro Dienst sind möglich:

```toml
[services.lxmf]
identity_path = ".bridge-bot/lxmf-identity"
display_name = "bridge-bot"

[[services.lxmf.interfaces]]
type = "tcp_client"
host = "lxmf-peer.example.org"
port = 4242

[services.rrc]
hub = "00000000000000000000000000000003"
identity_path = ".bridge-bot/rrc-identity"
nickname = "bridge-bot"

[[services.rrc.interfaces]]
type = "tcp_client"
host = "rrc-peer.example.org"
port = 4242
```

Unterstützt sind `tcp_client` mit `host`/`port`, `tcp_server` mit `listen`/`port` und `auto` für IPv6-Peer-Erkennung im lokalen Netzwerk. Für `auto` sind `group_id` (Standard `reticulum`), `discovery_port` (29716), `data_port` (42671) und `devices` (Liste von Netzwerkkarten, standardmäßig keine Einschränkung) optional. Ein AutoInterface allein schafft keine Verbindung zu einem entfernten Netz; dafür wird ein erreichbarer Peer benötigt. TCP-Ports müssen zwischen 1 und 65535 liegen.

Ohne `interfaces` verwendet jeder Dienst ein AutoInterface. `interfaces = []` deaktiviert Netzwerkinterfaces ausdrücklich. Die Instanzen verwenden keine Shared Instance und teilen weder Interfaces noch Routingtabellen. Eigene Konfigurationen für denselben lokalen Listen-Port können kollidieren; insbesondere AutoInterfaces müssen bei Bedarf mit passenden eigenen Ports und Peer-Einstellungen konfiguriert werden.

Netzwerkzustand wird pro Dienst unter `.bridge-bot/reticulum/managed-<hexkodierter Dienstname>` gespeichert. Das optionale `reticulum_config` bestimmt das übergeordnete Zustandsverzeichnis; Interfaces stammen immer aus der TOML-Datei. Vorhandene externe Reticulum-Konfigurationsdateien werden nicht überschrieben oder automatisch übernommen. Wer vorher Interfaces in einer solchen Datei konfiguriert hat, muss sie in die jeweilige TOML-Liste übertragen.

Bei LXMF ersetzt `identity_path` die bisherigen Felder `rpc_endpoint` und `source`. Eine vorhandene rohe 64-Byte-RNS-Identität kann weiterverwendet werden, um dieselbe Adresse zu behalten. Ohne vorhandene Identität erstellt der Bot eine neue. Die Identitätsdatei muss erhalten bleiben. Eingehende Nachrichten werden vor der Weiterleitung auf Zieladresse, Senderidentität und Signatur geprüft; unbekannte Sender werden über eine Pfadanfrage aufgelöst. Kann die Signatur nicht verifiziert werden, erscheint eine Warnung und die Nachricht wird verworfen.

`identity_path` ist optional: Für `services.rrc` gilt `.bridge-bot/rrc-identity`, für `services.lxmf` gilt `.bridge-bot/lxmf-identity`. Existiert die Datei, wird sie geladen; andernfalls wird dort eine neue Identität gespeichert. Ungültige oder nicht lesbare vorhandene Dateien führen zu einem Fehler und werden nicht überschrieben. Eigene Dienstnamen erhalten getrennte Standarddateien unter `.bridge-bot/identity-<hexkodierter Dienstname>`; ein expliziter Pfad hat immer Vorrang.

`reticulum-node` bleibt ein optionales eigenständiges Diagnosewerkzeug. Der normale Bot-Betrieb benötigt diesen Prozess nicht. `bridge-check --rrc` betreibt für den zweiten Testteilnehmer ebenfalls einen eigenen eingebauten Stack mit den konfigurierten RRC-Interfaces; ein zusätzliches TCP-Server-Interface sollte dabei nicht denselben Listen-Port wie der Bot verwenden.

## RRC-Räume

RRC ist ein eigener Live-Chat über identifizierte Reticulum-Links, mit CBOR-Nachrichten an einen `rrc.hub`. Es verwendet keine LXMF-Delivery-Destination. Pro Hub wird ein Dienst konfiguriert; das Feld `room` bezeichnet einen Raum. Pro Dienst wird eine dauerhafte RNS-Identität gespeichert.

Führende `#` im konfigurierten Raumnamen werden entfernt: `room = "#general"` und `room = "general"` verwenden beide den Raum `general`.

```toml
[services.rrc]
hub = "00000000000000000000000000000003" # echte RRC-Hub-Destination einsetzen
identity_path = ".bridge-bot/rrc-identity"
nickname = "bridge-bot"
reticulum_config = ".bridge-bot/reticulum"

[[bridges]]
from = { service = "telegram", chat = "-1001234567890" }
to = { service = "rrc", room = "#lobby" }
bidirectional = true
```

Raumnamen müssen kleingeschrieben sein. Alle in Routen genannten Räume werden nach `WELCOME` betreten; erst nach `JOINED` gilt der Adapter als bereit. Nachrichten (`MSG`) und Aktionen (`ACTION`) werden weitergeleitet, eigene Echos sowie Hub-Notices nicht. Nicknames sind Anzeigehinweise des Hubs, keine verifizierten Identitäten. Lange Texte werden passend zur Link-Paketgröße in mehrere Nachrichten geteilt. Als Versandbestätigung gilt das vom Hub zurückgesendete eigene Nachrichten-Echo mit passender ID, passendem Raum und Text. Das bestätigt die Verarbeitung am Hub, nicht den Empfang bei jedem Raumteilnehmer. Ein Hub muss dazu Nachrichten auch an ihren Sender weiterleiten, wie der Referenz-Hub `rrcd`.

RRC und LXMF laufen vollständig im Bot über die Rust-Bibliothek [rsReticulum](https://github.com/ratspeak/rsReticulum), deren Git-Revision festgelegt ist. Jeder Dienst besitzt seinen eigenen Stack mit den im TOML konfigurierten Interfaces.

Die Identitätsdatei muss über Neustarts hinweg erhalten bleiben; bestehende rohe 64-Byte-RNS-Identitäten können weiterverwendet werden. Für gleichzeitig laufende Instanzen sind getrennte Identitätsdateien zu verwenden. Bei regulärem Herunterfahren verlässt der Bot seine Räume und beendet die Netzwerkinstanz geordnet.

Bei Link-Abbruch versucht der Adapter mit wachsenden Wartezeiten erneut zu verbinden und die Räume zu betreten. Verweigert der Hub den Beitritt oder entfernt er den Bot aus einem Raum, beendet sich der Bot mit Fehler. Hub-Ratenlimits lösen Versandwiederholungen aus; andere ausdrücklich gemeldete Versandablehnungen beenden die betreffende Weiterleitung. RRC-Nachrichten, die während einer Trennung im Raum entstehen, können nicht nachträglich abgeholt werden. Raum-Passwörter, historische Nachrichten und Resource-Erweiterungen sind noch nicht unterstützt. Protokoll: [RRC-Spezifikation](https://rrc.kc1awv.net/3-RRC-wire-encoding.html).

## Versandwiederholungen

Der Bot führt ausschließlich begrenzte Warteschlangen im Arbeitsspeicher. Nach erfolgreicher Weiterleitung wird der Auftrag verworfen; es gibt keine Nachrichtendatenbank und keine dauerhafte Outbox. Jeder Zielchat hat einen eigenen FIFO-Worker: Ein gestörtes Ziel hält andere Zielchats nicht auf, und später eingegangene Nachrichten überholen keine laufenden Wiederholungen desselben Ziels.

```toml
[retry]
max_attempts = 6          # erster Versuch plus höchstens fünf Wiederholungen
initial_delay_secs = 2
max_delay_secs = 60
max_age_secs = 600       # gilt ab Aufnahme in die Warteschlange
attempt_timeout_secs = 90
queue_capacity = 256     # wartende Aufträge pro Zielchat
```

Alle Angaben sind optional und verwenden diese Standardwerte. Wartezeiten wachsen exponentiell (ungefähr 2, 4, 8, 16, 32 Sekunden) mit bis zu 20 % zusätzlicher Zufallsabweichung, begrenzt durch `max_delay_secs`. Telegram-`retry_after` wird auch dann eingehalten, wenn es über dieser Obergrenze liegt. Nach einem erkennbar dauerhaften Fehler, etwa fehlenden Telegram-Berechtigungen, wird sofort abgebrochen. Wiederholungen und endgültige Fehlschläge werden protokolliert.

Telegram gilt nach Bestätigung der Bot-API als angenommen. LXMF sendet signierte Nachrichten als Reticulum-Resource über einen direkten verschlüsselten Link und wartet auf dessen Resource-Empfangsbestätigung. Das bestätigt die Übertragung zum Empfänger, nicht eine Anzeige oder das Lesen in dessen Anwendung. Falls der Empfänger einen Delivery-Stamp verlangt, wird dieser erzeugt; beim Zeitablauf wird die Arbeit abgebrochen. Nachrichtentimestamp, Signatur und ID bleiben bei Wiederholungen erhalten. Ein abgebrochener Versuch veranlasst das Schließen seines Links. RRC wartet bis zu 30 Sekunden auf das Hub-Echo und behält die IDs unbestätigter Teile bei Wiederholungen bei. Bereits bestätigte Telegram- und RRC-Teile werden übersprungen.

Insgesamt sind maximal 4096 Weiterleitungsaufträge mit zusammen 16 MiB ausgehendem Text zugelassen; vorbereitete Paketdaten kommen hinzu. Volle Warteschlangen, erschöpfte Grenzen, Zeitablauf und Herunterfahren führen zum Verwerfen der betreffenden Aufträge und werden protokolliert. Bei verlorener Bestätigung kann ein erneutes Senden trotz erfolgreicher erster Zustellung Duplikate erzeugen; eine Exactly-once-Garantie gibt es nicht. Nach einem Neustart existieren keine gespeicherten Retry-Aufträge.

## Aktueller Umfang und Grenzen

- Telegram: private Chats, Gruppen, Supergruppen und Kanäle; Text und Medien-Bildunterschriften. Forum-Themen in Supergruppen werden explizit nach Thread-ID geroutet. Lange Texte werden unter Beachtung der UTF-16-Grenze aufgeteilt. Medien selbst, Änderungen, Löschungen, Antwortbeziehungen und Kanal-Kommentarthreads werden noch nicht übertragen.
- LXMF: Direkter Versand sowie Empfang von direkten Link-Paketen, Resources und opportunistischen Destination-Paketen. Nachrichten sind auf 1 MiB begrenzt. Versand über Propagation Nodes, Tickets, Medien und LXMF-Gruppen-Relay-Protokolle sind noch nicht implementiert. RRC-Räume werden über einen eigenen Adapter unterstützt.
- Eigene Telegram-Nachrichten, eigene LXMF-Absender und doppelte Eingangs-IDs werden während der Laufzeit ausgefiltert. Die Caches sind begrenzt und nicht dauerhaft. Andere Relay-Bots können Schleifen verursachen; mehrere Bridges sollten keine zyklische Relay-Kette bilden.
- Telegram-Polling versucht nach Verbindungsfehlern erneut zu verbinden; TCP-Client-Interfaces werden durch den eingebauten Stack erneut verbunden. Versandfehler werden gemäß `[retry]` im Arbeitsspeicher erneut versucht; eine dauerhafte Outbox ist nicht vorgesehen.
- LXMF empfängt nur während der Laufzeit; es gibt keinen Nachrichtenabruf nach einem Neustart. Telegram nutzt Long Polling: Für denselben Bot darf kein anderer Poller oder Webhook aktiv sein.

## Entwicklung

```sh
cargo fmt --check
cargo test
cargo clippy --all-targets -- -D warnings

# Rust-RRC-Integrationstest über lokale TCP-Interfaces:
cargo test native_rrc
```

`src/config.rs` validiert Dienste und Routen. `src/services/mod.rs` definiert das gemeinsame Eingangsformat und den `Adapter`-Trait. Neue Dienste implementieren `receive` und `send` und ergänzen die Dienstkonfiguration und Adapter-Erstellung. Der Routing-Code benötigt dafür keine dienstspezifischen Änderungen.

Bibliotheken: [teloxide 0.17](https://docs.rs/teloxide/0.17.0/teloxide/), [lxmf-wire 0.13](https://crates.io/crates/lxmf-wire) für das LXMF-Nachrichtenformat und [rsReticulum](https://github.com/ratspeak/rsReticulum) für den Netzwerkstack. rsReticulum verwendet AGPL-3.0-or-later, LXMF-rs EPL-2.0 oder GPL-2.0-or-later; ihre Lizenzbedingungen sind bei einer Veröffentlichung zu berücksichtigen.
