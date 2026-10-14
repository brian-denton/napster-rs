# napster-rs

A Rust rewrite of the **Napster v2.0 BETA 10** Windows client, plus a server
that makes it work again. The protocol was recovered from the original
`napster.exe`; see [docs/PROTOCOL.md](docs/PROTOCOL.md).

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

## Not implemented

* The built-in MP3 player.
* Proxy (SOCKS) support.
* Server-to-server linking.
* Auto-upgrade (message 5) and the connection tests (900/901), which are
  ignored on purpose. See the protocol notes.
