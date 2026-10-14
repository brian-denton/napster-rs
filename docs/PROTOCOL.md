# Napster v2.0 BETA 10: reverse engineering notes

These are the findings behind `napster-proto`. The target was the
installed `napster.exe` from `napster2b10.exe`, analyzed statically: strings
and x86 disassembly with capstone. Nothing from the installer was executed.
The scripts are in [`re/`](../re).

## The binaries

| File | Notes |
|---|---|
| `napster2b10.exe` | MindVision **VISE** installer (1.8 MB). It installs `napster.exe`, `cleanap.dll`, `vmp3.nvm`, `vwma.nvm`, `lcd.ttf`, `license.txt` and `filetypes.reg`, plus `amp.dll` and `AMPL3DLite.ocx` in `system32`. It also registers the `napster:` URL protocol, the `.nap` file type and `Nap.Protocol.1`. |
| `napster.exe` | 32-bit MSVC/MFC/ATL GUI with an image base of `0x400000`. The build stamp is `v2.0 BETA 10`, `May  5 2001 13:52:58`. It imports `WSOCK32`, `ICMP` (for pinging peers), `WININET`, `WINMM`, `WMVCore` and `amp.dll` (MP3 playback). |
| `vmp3.nvm`, `vwma.nvm` | The "validators". They are DLLs exporting `Validate`, `Is_Media` and `ExtractMediaInfo`, which read the bitrate, frequency and length of MP3 and WMA files. `filetypes.reg` maps each extension to its validator. |

Settings live in `HKLM\Software\Napster` (`CurrentUser`, `LineSpeed`,
`MaxUploads`, `MaxDownloads`, `WaitPort` (the data port), `ProxyServer`,
`SavePath`, …). User data files are `napster.dat`, `shared.dat`,
`resume.dat`, `channels.dat` and `notify.dat`. `notify.dat` holds the hot list.

## Key functions (VA)

| VA | Role |
|---|---|
| `0x42f120` | `SendMessage(type, fmt, ...)`. It formats with `sprintf` and frames the result. Every outgoing message goes through it, so walking its callers produced the full list of messages the client sends. |
| `0x42c040` | `RecvMessage(buf, max)`. Returns the message type. |
| `0x420c50` | The main **server-message dispatcher**, a large `switch` on the type: a binary search tree plus four jump tables. |
| `0x42df60` | Field tokenizer: space separated, `"…"` quoting, no escapes. |
| `0x435db0` | Metaserver lookup on `server.napster.com:8875` (`0x22ab`). |
| `0x436040` | Connect and login sequence. |
| `0x410fb0` | `DownloadFile()`, the peer `GET` handshake. |
| `0x426ad0` / `0x426cf0` | Handlers for server messages 900 and 901 (connection and listen tests). |

## Connecting

1. Resolve `server.napster.com`, falling back to `server2.napster.com`.
   Connect to port **8875**, the metaserver.
2. Read up to 99 bytes. The reply is either `ip:port` or `WAIT <seconds>`.
   On `WAIT` the client shows *"Waiting %d seconds (%d) Server Busy..."* and
   tries again.
3. Connect to `ip:port`. Send **920** with the payload `4`, then **2**
   (login):

   ```
   %s %s %d "%s" %d %lu %d
   nick pass dataport "v2.0 BETA 10" linespeed 5151 flag
   ```

   `5151` (`0x141f`) is the build number. The meaning of the final flag is
   unknown.
4. The login loop accepts **3** (ack, carrying the email), **0** (error),
   **5** (auto-upgrade), **900**, **910** and **911**.

New accounts go through **7** (nick check). The server answers 8 (free),
9 (taken) or 10 (invalid). Registration then uses **6**:
`%s %s %d "%s" %d %s %d`, which adds the email. Existing accounts may be
checked with **11** (`nick pass`), answered by 12. The COPPA age prompt
and the demographic fields (14 and 15) are client side only.

## Frame format

```
u16 length (LE) | u16 type (LE) | payload: `length` bytes of text
```

## Messages the b10 client sends

All of these were found by walking the callers of `0x42f120`:

```
2 login            4 version          6 new user         7 nick check
11 pass check      15 login options   100 share          102 unshare
200 search         203 download req   205 privmsg        207/208 hotlist
211 browse         215 resume search  218/219 dl start/end  220/221 ul start/end
300 alt ports      303 hotlist rm     320 ignore list    322/323 ignore add/rm
326 ignore clear   400 join           401 part           402 say
410 topic          420-424 chan bans  430/431/432 invite 500 push request
600 linkspeed      603 whois          606 setuserlevel   608 accept upload
610 kill           612 ban            613 set dataport   614 unban
615 banlist        617 list channels  619 queue limit    621 motd
622/623 muzzle     624 unnuke         625 set linespeed  626 dataport error
627 opsay          628 announce       641 direct browse accept
651/652 cloak      700-704 change speed/pass/email/port
750 server ping    751 ping           752 pong           753 set password
800 reloadconfig   801 version        810 setconfig      820 clear channel
824 emote          827 list all chans 829 kick           870 share dir
920 pre-login
```

## Server messages the dispatcher handles

These come from the decoded switch at `0x420c50`:

```
0 1 3 5 201 202 204 205 206 209 210 212 213 214 216 217
301 302 320 321 324 325 401 403 404 405 406 407 408 409 410 420 421 430
501 601 604 605 607 609 613 616 618 620 621 625 627 628
640 641 642 748 750 751 752 824 828 900 901 910 931 1100 1200
```

The download flow uses the upload-request handshake. The server does not
answer 203 directly:

```
downloader --203 nick "file"--> server --607 downloader "file" speed--> uploader
uploader   --608 downloader "file"--> server --204 nick ip port "file" md5 speed--> downloader
           (or 619 queue limit --> server --620 --> downloader)
```

If the uploader's port is 0, the downloader sends 500 and the uploader
receives 501 with the downloader's address.

IP addresses are the decimal value of the raw `in_addr` read as a
little-endian integer, so `1.2.3.4` is sent as `0x04030201`.

## Search

The search request is built from these fragments (at `0x49b374`):

```
FILENAME CONTAINS "%s" MAX_RESULTS %s FILENAME CONTAINS "%s" LINESPEED "%s" %d BITRATE "%s" "%s"
```

The comparison operators are `AT LEAST`, `AT BEST` and `EQUAL TO`. Each
result (201) has the form
`"file" md5 size bitrate freq secs nick ip linespeed`.

## Peer protocol (from `DownloadFile()`)

```
connect to uploader:port
  <- "1"                                   (rejected unless the byte is 0x31)
  -> "GET"                                 (sent on its own)
  -> nick "filename" offset                ("%s \"%s\" %lu")
  <- size as ascii | FILE NOT SHARED | FILE NOT FOUND | INVALID REQUEST
  <- data ...
```

After the transfer the client sends 218 and 219 (or 220 and 221 for
uploads). A partial download goes to `Incomplete\` and is recorded in
`resume.dat`.

In firewalled push mode the roles swap: the uploader connects, sends
`SEND` and then `nick "file" size`, and reads back an ascii offset.

Direct browse (640/641) uses `GETLIST`. The browsee replies with its nick
and a newline, then share lines, then an empty line. The pushed variant
starts with `SENDLIST nick\n`.

## MD5

Hash the first **299,008** bytes of audio, starting at the first frame
sync after any ID3v2 tag. WMA files send the literal `WMA-FILE`.

## Deliberate differences in napster-rs

* **5 (auto-upgrade)** is ignored. In b10 it downloads `upgrade.exe` over
  HTTP and runs it without asking, which is a well-known remote code
  execution hole.
* **900 and 901** are ignored. They make the client open connections, or
  listen on ports, chosen by the server.
* Feedback by SMTP to `mail.napster.com`, the CDNOW shop links and the
  swear-word filter are not reproduced.
* napd adds a TLS port (8889 by default) that carries the same framed
  protocol inside TLS. The plain port is unchanged for the 2001 client.
* The client string is `napster-rs <version>`. The login layout is the same
  as b10's.

## Server links (napster-rs extension)

`napd` servers can join into one network. Links are napster-rs's own
design, not compatible with OpenNap's linking. They run over TLS, and the
connecting server pins the peer's certificate fingerprint. Messages use
the normal frame format, with types from 10000 up:

| Type | Name | Payload |
|---|---|---|
| 10000 | HELLO | `<server-name> <nonce>` |
| 10001 | AUTH | hex HMAC-SHA256(link password, `napster-link\|<peer's nonce>\|<own name>`) |
| 10010 | USER_ON | `<nick> <ip> <data-port> <speed> <shared> <level> <login-ms> "<client>"` |
| 10011 | USER_OFF | `<nick>` |
| 10012 | USER_SHARES | `<nick> <count>` |
| 10020 | DELIVER | `<to-nick> <type> <data>`: hand a client message to a local user |
| 10021 | FORWARD | `<from-nick> <type> <data>`: a remote user's 203/500/211/640 about a local user |
| 10030 | SEARCH | `<id> <max> <200 payload>` |
| 10031 | SEARCH_RESULT | `<id> <201 payload>` |
| 10032 | SEARCH_END | `<id>` |
| 10040–10044 | CHAN_JOIN / PART / SAY / EMOTE / TOPIC | `<channel> <nick> [text]`, or `<channel> <topic>` for TOPIC |

The handshake:

1. The accepting server sends HELLO.
2. The connecting server answers with its own HELLO, then AUTH.
3. The accepting server checks that AUTH and replies with its own AUTH,
   which the connecting server checks.

After that, each server sends USER_ON for its users and CHAN_JOIN for its
channel members.

The network is a full mesh: nothing is relayed between servers.

If two servers accept the same nick at the same moment, the earlier login
wins. Ties go to the alphabetically lower server name. Both servers apply
the same rule, so they agree on who keeps the nick.

## Discover (napster-rs extension)

The Discover tab uses messages from 11000 to 11099. `napd` only sends them
to clients whose client string starts with `napster-rs`, so the original
client never sees them. Right before LOGIN_ACK (3), `napd` sends
CAPABILITIES. A client that sees `discover` in it sends a SHARE_META for
each tagged file after its shares.

| Type | Name | Direction | Payload |
|---|---|---|---|
| 11099 | CAPABILITIES | server → client | space-separated features, e.g. `discover` |
| 11010 | SHARE_META | client → server | `"<shared filename>" "<artist>" "<title>" "<genre>"` |
| 11000 | DISCOVER_GENRES | both | client: empty request. Server: ends the list |
| 11001 | GENRE_ENTRY | server → client | `"<genre>" <files> <users>` |
| 11002 | DISCOVER_GENRE | both | client: `"<genre>"`. Server: `"<genre>"`, ends the list |
| 11003 | GENRE_FILE | server → client | a 201 search-result payload |
| 11004 | DISCOVER_CHARTS | both | client: empty request. Server: ends the charts |
| 11005 | CHART_ENTRY | server → client | `<songs\|searches\|artists> <count> "<text>"` |
| 11006 | DISCOVER_MEMBERS | both | client: empty request. Server: ends the list |
| 11007 | MEMBER_ENTRY | server → client | `<nick> <match%> <files> <speed> "<common; ...>" "<favorites; ...>"` |

How the server builds the data:

* **Tags:** kept per session only, and dropped when a file is unshared or
  the user logs off. A SHARE_META for a file that isn't shared is
  ignored. Fields are clipped to 100 characters.
* **Songs chart:** download requests (203) seen in the last 24 hours.
  Each is labelled `Artist - Title` from the uploader's tags, or with the
  file name when there are none.
* **Searches chart:** the search terms (200) seen in the last 24 hours.
* **Artists chart:** how many users currently share each artist.
* **Members:** the Jaccard similarity of the two users' artist sets,
  ignoring "Unknown" and "Various Artists". Cloaked users and users
  sharing nothing are left out.

## References

* *napster messages* by drscholl (OpenNap):
  <https://opennap.sourceforge.net/napster.txt>. This community spec was
  checked against the disassembly above. The b10 binary agrees with it,
  apart from additions noted here: the 920 payload, the login build number,
  and messages 910, 911, 931, 1100 and 1200.
* <https://en.wikipedia.org/wiki/Napster>
