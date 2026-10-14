//! Message type numbers.
//!
//! The direction comment notes who sends each message: `C` = client,
//! `S` = server. Numbers marked "(b10)" were confirmed by disassembling the
//! v2.0 BETA 10 client: either it sends them (calls to its `SendMessage`
//! helper) or its server-message dispatcher has a case for them.

/// S: error during login. `<message>` (b10)
pub const ERROR: u16 = 0;
/// C: log in. `<nick> <password> <port> "<client>" <speed> [build] [flag]` (b10)
pub const LOGIN: u16 = 2;
/// S: login accepted. `<email>` (b10)
pub const LOGIN_ACK: u16 = 3;
/// C: version check. `"<version>"` (b10)
pub const VERSION_CHECK: u16 = 4;
/// S: auto-upgrade. `<version> <host:file>` (b10). Never sent by our server.
pub const AUTO_UPGRADE: u16 = 5;
/// C: register and log in. `<nick> <pass> <port> "<client>" <speed> <email> [build]` (b10)
pub const NEW_USER_LOGIN: u16 = 6;
/// C: is this nick available? `<nick>` (b10)
pub const NICK_CHECK: u16 = 7;
/// S: nick is free (b10)
pub const NICK_NOT_REGISTERED: u16 = 8;
/// S: nick is taken (b10)
pub const NICK_REGISTERED: u16 = 9;
/// S: nick has invalid characters (b10)
pub const NICK_INVALID: u16 = 10;
/// C: verify password before login. `<nick> <pass>` (b10)
pub const PASSWORD_CHECK: u16 = 11;
/// S: password ok (b10)
pub const PASSWORD_OK: u16 = 12;
/// C: demographic info sent after registration (b10 sends 14 & 15)
pub const LOGIN_OPTIONS: u16 = 14;
pub const LOGIN_OPTIONS2: u16 = 15;

/// C: share a file. `"<file>" <md5> <size> <bitrate> <freq> <secs>` (b10)
pub const SHARE_FILE: u16 = 100;
/// C: stop sharing a file. `"<file>"` (b10)
pub const UNSHARE_FILE: u16 = 102;
/// C: stop sharing everything.
pub const UNSHARE_ALL: u16 = 110;

/// C: search. see [`crate::search`] (b10)
pub const SEARCH: u16 = 200;
/// S: search result (b10)
pub const SEARCH_RESULT: u16 = 201;
/// S: end of search results (b10)
pub const SEARCH_END: u16 = 202;
/// C: download request. `<nick> "<file>"` (b10)
pub const DOWNLOAD_REQUEST: u16 = 203;
/// S: download ack. `<nick> <ip> <port> "<file>" <md5> <speed>` (b10)
pub const DOWNLOAD_ACK: u16 = 204;
/// C/S: private message. `<nick> <text>` (b10)
pub const PRIVATE_MESSAGE: u16 = 205;
/// S: download error. `<nick> "<file>"` (b10)
pub const DOWNLOAD_ERROR: u16 = 206;
/// C: add to hotlist. `<nick>` (b10)
pub const HOTLIST_ADD: u16 = 207;
/// C: initial hotlist entry. `<nick>` (b10)
pub const HOTLIST_INIT: u16 = 208;
/// S: hotlist user signed on. `<nick> <speed>` (b10)
pub const USER_SIGNON: u16 = 209;
/// S: hotlist user signed off. `<nick>` (b10)
pub const USER_SIGNOFF: u16 = 210;
/// C: browse a user's files via the server. `<nick>` (b10)
pub const BROWSE: u16 = 211;
/// S: browse entry. `<nick> "<file>" <md5> <size> <bitrate> <freq> <secs>` (b10)
pub const BROWSE_RESULT: u16 = 212;
/// S: end of browse. `<nick> [ip]` (b10)
pub const BROWSE_END: u16 = 213;
/// C/S: server stats. S: `<users> <files> <gigabytes>` (b10)
pub const SERVER_STATS: u16 = 214;
/// C: find other sources for a file. `<md5> <size>` (b10)
pub const RESUME_REQUEST: u16 = 215;
/// S: resume match. `<nick> <ip> <port> "<file>" <md5> <size> <speed>` (b10)
pub const RESUME_RESULT: u16 = 216;
/// S: end of resume matches (b10)
pub const RESUME_END: u16 = 217;
/// C: download started / finished (b10)
pub const DOWNLOAD_START: u16 = 218;
pub const DOWNLOAD_COMPLETE: u16 = 219;
/// C: upload started / finished (b10)
pub const UPLOAD_START: u16 = 220;
pub const UPLOAD_COMPLETE: u16 = 221;

/// C: alternate data ports (b10)
pub const OPTIONAL_PORTS: u16 = 300;
/// S: hotlist add ok. `<nick>` (b10)
pub const HOTLIST_ACK: u16 = 301;
/// S: hotlist add failed. `<nick>` (b10)
pub const HOTLIST_ERROR: u16 = 302;
/// C: remove from hotlist. `<nick>` (b10)
pub const HOTLIST_REMOVE: u16 = 303;
/// C/S: ignore list request / S: end of list `<count>` (b10)
pub const IGNORE_LIST: u16 = 320;
/// S: ignore list entry `<nick>` (b10)
pub const IGNORE_ENTRY: u16 = 321;
/// C/S: ignore a user `<nick>` (b10)
pub const IGNORE_ADD: u16 = 322;
/// C/S: unignore a user `<nick>` (b10)
pub const IGNORE_REMOVE: u16 = 323;
/// S: user wasn't ignored `<nick>` (b10)
pub const IGNORE_NOT_IGNORED: u16 = 324;
/// S: user already ignored `<nick>` (b10)
pub const IGNORE_ALREADY: u16 = 325;
/// C/S: clear ignore list / S: `<count>` (b10)
pub const IGNORE_CLEAR: u16 = 326;

/// C: join channel `<channel>` (b10)
pub const JOIN_CHANNEL: u16 = 400;
/// C/S: part channel `<channel>` (b10)
pub const PART_CHANNEL: u16 = 401;
/// C: say in channel `<channel> <text>` (b10)
pub const CHANNEL_SAY: u16 = 402;
/// S: channel message `<channel> <nick> <text>` (b10)
pub const CHANNEL_MESSAGE: u16 = 403;
/// S: general error `<text>` (b10)
pub const SERVER_ERROR: u16 = 404;
/// S: join ok `<channel>` (b10)
pub const JOIN_ACK: u16 = 405;
/// S: user joined `<channel> <nick> <shared> <speed>` (b10)
pub const CHANNEL_JOIN: u16 = 406;
/// S: user parted `<channel> <nick> <shared> <speed>` (b10)
pub const CHANNEL_PART: u16 = 407;
/// S: channel member `<channel> <nick> <shared> <speed>` (b10)
pub const CHANNEL_USER: u16 = 408;
/// S: end of channel members `<channel>` (b10)
pub const CHANNEL_USER_END: u16 = 409;
/// C/S: channel topic `<channel> <topic>` (b10)
pub const CHANNEL_TOPIC: u16 = 410;
/// C/S: channel ban list / end `<channel>` (b10)
pub const CHANNEL_BAN_LIST: u16 = 420;
/// S: channel ban entry (b10)
pub const CHANNEL_BAN_ENTRY: u16 = 421;
/// C: channel ban `<channel> <nick> ["reason"]` (b10)
pub const CHANNEL_BAN: u16 = 422;
/// C: channel unban `<channel> <nick> ["reason"]` (b10)
pub const CHANNEL_UNBAN: u16 = 423;
/// C: clear channel bans `<channel>` (b10)
pub const CHANNEL_BAN_CLEAR: u16 = 424;
/// C/S: invite. C: `<nick> <channel>` S: `<inviter> <channel> "<topic>" 0 <text>` (b10)
pub const CHANNEL_INVITE: u16 = 430;
pub const CHANNEL_INVITE_ACCEPT: u16 = 431;
pub const CHANNEL_INVITE_DECLINE: u16 = 432;

/// C: firewalled download request `<nick> "<file>"` (b10)
pub const PUSH_REQUEST: u16 = 500;
/// S: push to downloader `<nick> <ip> <port> "<file>" <md5> <speed>` (b10)
pub const PUSH_ACK: u16 = 501;

/// C: link speed request `<nick>` (b10)
pub const LINK_SPEED_REQUEST: u16 = 600;
/// S: link speed `<nick> <speed>` (b10)
pub const LINK_SPEED: u16 = 601;
/// C: whois `<nick>` (b10)
pub const WHOIS: u16 = 603;
/// S: whois response (b10)
pub const WHOIS_RESPONSE: u16 = 604;
/// S: whowas `<nick> <level> <last-seen>` (b10)
pub const WHOWAS: u16 = 605;
/// C: set user level `<nick> <level>` (b10)
pub const SET_USER_LEVEL: u16 = 606;
/// S: someone wants a file from you `<nick> "<file>" <speed>` (b10)
pub const UPLOAD_REQUEST: u16 = 607;
/// C: accept upload `<nick> "<file>"` (b10)
pub const UPLOAD_ACCEPT: u16 = 608;
/// S: upload not accepted `<nick> "<file>"` (b10)
pub const UPLOAD_ACCEPT_FAILED: u16 = 609;
/// C: kill (disconnect) user `<nick> ["reason"]` (b10)
pub const KILL: u16 = 610;
/// C: nuke account `<nick>`
pub const NUKE: u16 = 611;
/// C: ban `<nick|ip> ["reason"]` (b10)
pub const BAN: u16 = 612;
/// C/S: set data port (b10)
pub const SET_DATA_PORT: u16 = 613;
/// C: unban (b10)
pub const UNBAN: u16 = 614;
/// C/S: ban list request / end (b10)
pub const BAN_LIST: u16 = 615;
/// S: ban list entry `<ip> <nick> "<reason>" <time> <n>` (b10)
pub const BAN_ENTRY: u16 = 616;
/// C/S: list channels / end of list (b10)
pub const LIST_CHANNELS: u16 = 617;
/// S: channel list entry `<channel> <users> <topic>` (b10)
pub const CHANNEL_LIST_ENTRY: u16 = 618;
/// C: queue limit reached `<nick> "<file>" <limit>` (b10)
pub const QUEUE_LIMIT: u16 = 619;
/// S: queue limit `<nick> "<file>" <size> <limit>` (b10)
pub const QUEUE_LIMIT_NOTIFY: u16 = 620;
/// C/S: message of the day (b10)
pub const MOTD: u16 = 621;
/// C: muzzle `<nick> ["reason"]` (b10)
pub const MUZZLE: u16 = 622;
/// C: unmuzzle (b10)
pub const UNMUZZLE: u16 = 623;
/// C: unnuke (b10)
pub const UNNUKE: u16 = 624;
/// C: set link speed for user `<nick> <speed>` (b10)
pub const SET_LINK_SPEED: u16 = 625;
/// C/S: data port unreachable `<nick> [port]` (b10)
pub const DATA_PORT_ERROR: u16 = 626;
/// C/S: message to operators (b10)
pub const OP_SAY: u16 = 627;
/// C/S: announcement to all (b10)
pub const ANNOUNCE: u16 = 628;
/// S: banned nick entry
pub const BANNED_USER: u16 = 629;
/// C/S: direct browse request. C: `<nick>` S: `<requester> [ip port]` (b10)
pub const DIRECT_BROWSE: u16 = 640;
/// C/S: direct browse accept. C: `<requester>` S: `<nick> <ip> <port>` (b10)
pub const DIRECT_BROWSE_ACCEPT: u16 = 641;
/// S: direct browse error `<nick> "<message>"` (b10)
pub const DIRECT_BROWSE_ERROR: u16 = 642;
/// C: cloak (b10 sends 651/652)
pub const CLOAK: u16 = 652;

/// C: change own link speed `<speed>` (b10)
pub const CHANGE_LINK_SPEED: u16 = 700;
/// C: change password `<pass>` (b10)
pub const CHANGE_PASSWORD: u16 = 701;
/// C: change email `<email>` (b10)
pub const CHANGE_EMAIL: u16 = 702;
/// C: change data port `<port>` (b10)
pub const CHANGE_DATA_PORT: u16 = 703;
/// C: unknown, sent by b10 with the user name
pub const UNKNOWN_704: u16 = 704;
/// S: someone tried to log in with your nick (b10)
pub const LOGIN_ATTEMPT: u16 = 748;
/// C/S: server ping (b10)
pub const SERVER_PING: u16 = 750;
/// C/S: ping user `<nick>` (b10)
pub const PING: u16 = 751;
/// C/S: pong `<nick>` (b10)
pub const PONG: u16 = 752;
/// C: change another user's password `<nick> <pass> "<reason>"` (b10)
pub const SET_PASSWORD: u16 = 753;

/// C: reload config (b10)
pub const RELOAD_CONFIG: u16 = 800;
/// C: server version (b10)
pub const SERVER_VERSION: u16 = 801;
/// C: set config (b10)
pub const SET_CONFIG: u16 = 810;
/// C: clear channel (b10)
pub const CLEAR_CHANNEL: u16 = 820;
/// C/S: redirect `<server> <port>`
pub const REDIRECT: u16 = 821;
/// C/S: cycle `<metaserver>`
pub const CYCLE: u16 = 822;
/// C/S: emote. C: `<channel> "<text>"` S: `<channel> <nick> "<text>"` (b10)
pub const EMOTE: u16 = 824;
/// S: channel user entry (for 830) `<channel> <nick> <shared> <speed>`
pub const CHANNEL_USER_ENTRY: u16 = 825;
/// C/S: list all channels / end (b10)
pub const LIST_ALL_CHANNELS: u16 = 827;
/// S: full channel list entry `<channel> <users> <n> <level> <limit> "<topic>"` (b10)
pub const ALL_CHANNELS_ENTRY: u16 = 828;
/// C: kick from channel `<channel> <nick> ["reason"]` (b10)
pub const KICK: u16 = 829;
/// C/S: list users in a channel / end `<channel>`
pub const LIST_CHANNEL_USERS: u16 = 830;
/// C: share files by directory `"<dir>" "<file>" <md5> <size> <bitrate> <freq> <secs> ...` (b10)
pub const SHARE_DIRECTORY: u16 = 870;

/// S: connection test `<ip> <port> <data>` (b10 handles it)
pub const CONNECTION_TEST: u16 = 900;
/// S: listen test `<port> <timeout> <data>` (b10 handles it)
pub const LISTEN_TEST: u16 = 901;
/// S: b10 login dispatcher accepts 910/911 during login
pub const LOGIN_910: u16 = 910;
pub const LOGIN_911: u16 = 911;
/// C: sent by b10 right before login with the payload `4`
pub const PRE_LOGIN: u16 = 920;

// ---- napster-rs extensions (Discover). Servers announce support with
// CAPABILITIES, and clients only send these to servers that did, so the
// 2001 client and other servers never see them.

/// S: `discover ...` features this server supports
pub const CAPABILITIES: u16 = 11099;
/// C: `"<file>" "<artist>" "<title>" "<genre>"` tags for a shared file
pub const SHARE_META: u16 = 11010;
/// C: list genres / S: end of list
pub const DISCOVER_GENRES: u16 = 11000;
/// S: `"<genre>" <files> <users>`
pub const GENRE_ENTRY: u16 = 11001;
/// C: `"<genre>"` files in a genre / S: end `"<genre>"`
pub const DISCOVER_GENRE: u16 = 11002;
/// S: a file in the genre, same payload as 201
pub const GENRE_FILE: u16 = 11003;
/// C: charts / S: end of charts
pub const DISCOVER_CHARTS: u16 = 11004;
/// S: `<chart> <count> "<text>"`, chart = songs | searches | artists
pub const CHART_ENTRY: u16 = 11005;
/// C: members with similar taste / S: end of list
pub const DISCOVER_MEMBERS: u16 = 11006;
/// S: `<nick> <match%> <files> <speed> "<common artists>" "<favorite artists>"` (lists `; `-separated)
pub const MEMBER_ENTRY: u16 = 11007;

/// Human readable name for logging.
pub fn name(kind: u16) -> &'static str {
    match kind {
        ERROR => "error",
        LOGIN => "login",
        LOGIN_ACK => "login-ack",
        VERSION_CHECK => "version-check",
        AUTO_UPGRADE => "auto-upgrade",
        NEW_USER_LOGIN => "new-user-login",
        NICK_CHECK => "nick-check",
        NICK_NOT_REGISTERED => "nick-not-registered",
        NICK_REGISTERED => "nick-registered",
        NICK_INVALID => "nick-invalid",
        PASSWORD_CHECK => "password-check",
        PASSWORD_OK => "password-ok",
        SHARE_FILE => "share",
        UNSHARE_FILE => "unshare",
        UNSHARE_ALL => "unshare-all",
        SEARCH => "search",
        SEARCH_RESULT => "search-result",
        SEARCH_END => "search-end",
        DOWNLOAD_REQUEST => "download-request",
        DOWNLOAD_ACK => "download-ack",
        PRIVATE_MESSAGE => "privmsg",
        DOWNLOAD_ERROR => "download-error",
        HOTLIST_ADD => "hotlist-add",
        HOTLIST_INIT => "hotlist-init",
        USER_SIGNON => "user-signon",
        USER_SIGNOFF => "user-signoff",
        BROWSE => "browse",
        BROWSE_RESULT => "browse-result",
        BROWSE_END => "browse-end",
        SERVER_STATS => "stats",
        JOIN_CHANNEL => "join",
        PART_CHANNEL => "part",
        CHANNEL_SAY => "say",
        CHANNEL_MESSAGE => "channel-message",
        SERVER_ERROR => "server-error",
        PUSH_REQUEST => "push-request",
        PUSH_ACK => "push-ack",
        UPLOAD_REQUEST => "upload-request",
        UPLOAD_ACCEPT => "upload-accept",
        WHOIS => "whois",
        WHOIS_RESPONSE => "whois-response",
        DIRECT_BROWSE => "direct-browse",
        DIRECT_BROWSE_ACCEPT => "direct-browse-accept",
        DIRECT_BROWSE_ERROR => "direct-browse-error",
        PING => "ping",
        PONG => "pong",
        SERVER_PING => "server-ping",
        PRE_LOGIN => "pre-login",
        _ => "?",
    }
}
