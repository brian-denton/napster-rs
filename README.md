# napster-rs

A Rust rewrite of the **Napster v2.0 BETA 10** Windows client, plus a server
that makes it work again. The protocol was recovered from the original
`napster.exe`; see [docs/PROTOCOL.md](docs/PROTOCOL.md).

The desktop client recreates the look of the 2001 client: Windows 2000
gray, bevelled buttons, the tab row, and the Search and Transfer windows.

![Search](docs/screenshots/search.png)
![Transfer](docs/screenshots/transfer.png)
![Chat](docs/screenshots/chat.png)

| Crate | Binary | What it is |
|---|---|---|
| `napster-proto` | | Wire protocol: frame codec, message numbers, field tokenizer, search grammar, and the peer `GET`/`SEND`/`GETLIST` handshakes |
| `napster-server` | `napd` | Server and metaserver: accounts, sharing and search, transfer brokering (including firewalled push), chat rooms, hot lists, whois, and moderation |
| `napster-core` | | Client engine: login, library scan (MP3 parsing and Napster MD5), downloads and uploads with resume, push transfers, direct browse |
| `napster-gui` | `napster` | Desktop client (egui) with the original's tabs: Home, Chat, Library, Search, Hot List, Transfer and Discover |
| `napster-cli` | `napster-cli` | Line-based client over the same engine |

## Build

```bash
cargo build --release
```

The binaries end up in `target/release/`: `napd`, `napster` and
`napster-cli`.

### Prebuilt packages (GitHub Actions)

`.github/workflows/build.yml` runs clippy and the tests on Windows, macOS
and Linux. It then packages:

| Package | Contents |
|---|---|
| `napster-rs-windows-x64.zip` | `napster.exe`, `napster-cli.exe` and `napd.exe` |
| `napster-rs-macos-universal.zip` | `Napster.app` and the command-line tools, built for Intel and Apple Silicon |
| `napster-rs-linux-x64.tar.gz` | `napster`, `napster-cli` and `napd` |

Download them from the run's **Artifacts** section. Pushing a tag such as
`v0.1.0` also attaches them to a GitHub release.

The macOS app is signed ad hoc but not notarized, so the first time you
open it you'll need to right-click it and choose **Open**. The Linux
client needs GTK 3 and X11 or Wayland at runtime.

## Run a server

```bash
napd --elite yourname
```

* The main server listens on `0.0.0.0:8888`.
* The metaserver (redirector) listens on `0.0.0.0:8875`. It hands out the
  address the client connected on; override that with `--public-host`.
* Accounts and bans are kept in `napd-data/db.json`. Unknown nicks are
  registered on first login, and passwords are stored salted with SHA-256.
* `--elite <nick>` gives full admin rights. `--channel <name>` (repeatable)
  sets the predefined rooms, and `--motd motd.txt` sets the message of the
  day.

People outside your LAN need to reach port 8888, plus 8875 if they use the
metaserver.

## Run the client

```bash
napster
```

On first run, Preferences opens. Set a user name and password, the server
(`host:8888`, or `host:8875` with the metaserver box ticked), your shared
folders and a download folder. The data port defaults to **6699**. Other
users must be able to reach it to download from you. If they can't, set it
to `0` (firewalled) and your uploads will be pushed out instead.

Settings are stored in `%APPDATA%\napster-rs\config.json`, which is shared
with the CLI.

Chat boxes accept IRC-style commands such as `/join`, `/msg`, `/me`,
`/whois`, `/browse`, `/ping` and `/raw <numeric> <data>`. Type `/help` for
the full list.

### CLI

```bash
napster-cli --server myhost:8888 --nick bob --password pw --share D:\Music
```

Then use `/search beatles`, `/get 1 2`, `/transfers`, `/join Lobby`, and so
on. `/help` lists everything.

## Authentic 1999 speeds

In the original client, **Line Speed** was only a label: you could claim
T1 while on a 28.8 modem. In napster-rs it really limits your transfers,
so downloading a song can take as long as it did then. This is on by
default. To turn it off, uncheck **Authentic speeds** in Preferences >
Transfer Settings, or pass `--modern-speeds` to the CLI.

| Line Speed | Downloads | Uploads | One 4 MB song |
|---|---|---|---|
| 14.4 Kbps | 1.6 KB/s | 1.6 KB/s | ~43 min |
| 28.8 Kbps | 3.2 KB/s | 3.2 KB/s | ~21 min |
| 33.6 Kbps | 3.8 KB/s | 3.6 KB/s | ~18 min |
| 56K | 5.0 KB/s | 3.5 KB/s | ~14 min |
| ISDN-64K | 7.5 KB/s | 7.5 KB/s | ~9 min |
| ISDN-128K | 15 KB/s | 15 KB/s | ~4.5 min |
| Cable | 150 KB/s | 16 KB/s | ~27 s |
| DSL | 90 KB/s | 15 KB/s | ~45 s |
| T1 | 180 KB/s | 180 KB/s | ~23 s |
| T3 | 5,000 KB/s | 5,000 KB/s | <1 s |
| Unknown | unlimited | unlimited | |

These are the speeds connections of the time actually sustained, not
their advertised ratings:

* Modems never reached their rated speed.
* 56K modems could only send at 33.6K.
* Home DSL and cable had fast downloads but uploads capped near 128 Kbps.

Things that work the way they did on a real connection:

* **One shared line.** The limit covers all your transfers together.
  Two downloads on 56K each get about 2.5 KB/s.
* **The slower side sets the pace.** If you're on DSL and download
  from someone on 56K, you get their 3.5 KB/s upload speed.
* **Changes apply right away.** A new Line Speed takes effect on
  transfers already in progress.

With authentic speeds off, you can set your own totals instead: **Max
Download Speed** and **Max Upload Speed** in KB/s, where 0 means
unlimited. On the CLI, use `--download-limit` and `--upload-limit`.

```bash
napster-cli --line-speed 4 --server myhost:8888 --nick modemguy --password pw
```

`--line-speed` takes 0–10: 0 Unknown, 1 14.4, 2 28.8, 3 33.6, 4 56K,
5 ISDN-64K, 6 ISDN-128K, 7 Cable, 8 DSL, 9 T1, 10 T3.

## Using the original 2001 client

The b10 client has `server.napster.com` (and `server2.napster.com`) built
in and contacts the metaserver on port 8875. To point it at `napd`, add a
hosts-file entry (`C:\Windows\System32\drivers\etc\hosts`):

```
192.168.1.10  server.napster.com
192.168.1.10  server2.napster.com
```

Then start the old client. `napd` speaks the same messages, including the
607/608 upload-request handshake that b10 uses. Not yet tested against
the original binary.

## Tests

```bash
cargo test --workspace
```

The end-to-end tests in `crates/napster-core/tests/e2e.rs` start a real
`napd` and real client engines on loopback. They cover:

* search and download, checked byte for byte
* a firewalled uploader pushing a file
* resuming a partial download
* direct browse, normal and firewalled
* chat, private messages and hot-list notifications
* login through the metaserver, including a wrong password

### Screenshots of every screen

Set `NAPSTER_SNAPSHOT_DIR` to make the GUI capture itself and exit. With
this set, it:

1. logs in using the given config
2. searches for `NAPSTER_SNAPSHOT_SEARCH` (default `mp3`)
3. starts a few downloads
4. saves a PNG of every tab and dialog to that directory

```bash
NAPSTER_SNAPSHOT_DIR=shots napster --config test-user.json
```

## Not implemented

* The built-in MP3 player.
* Proxy (SOCKS) support.
* Server-to-server linking.
* Auto-upgrade (message 5) and the connection tests (900/901), which are
  ignored on purpose. See the protocol notes.
