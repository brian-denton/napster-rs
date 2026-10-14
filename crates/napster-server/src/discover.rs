//! Discover: genres, live charts and members with similar taste, built from
//! the tags napster-rs clients send (SHARE_META) and from what users search
//! for and download. Covers users on this server (not linked ones).

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

use napster_proto::SearchResult;

use crate::state::{Session, State};

/// Charts cover this much recent activity.
pub const WINDOW: Duration = Duration::from_secs(24 * 60 * 60);
const MAX_EVENTS: usize = 50_000;
const CHART_LEN: usize = 20;
const MAX_FIELD: usize = 100;

/// Tags for one shared file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Meta {
    pub artist: String,
    pub title: String,
    pub genre: String,
}

impl Meta {
    pub fn new(artist: &str, title: &str, genre: &str) -> Self {
        let clip = |s: &str| s.trim().chars().take(MAX_FIELD).collect::<String>();
        Self { artist: clip(artist), title: clip(title), genre: clip(genre) }
    }
}

/// Recent downloads and searches, oldest first.
#[derive(Default)]
pub struct Trends {
    downloads: VecDeque<(Instant, String)>,
    searches: VecDeque<(Instant, String)>,
}

impl Trends {
    fn push(q: &mut VecDeque<(Instant, String)>, item: String) {
        let now = Instant::now();
        while q.front().is_some_and(|(t, _)| now.duration_since(*t) > WINDOW) || q.len() >= MAX_EVENTS {
            q.pop_front();
        }
        q.push_back((now, item));
    }

    pub fn download(&mut self, song: String) {
        Self::push(&mut self.downloads, song);
    }

    pub fn search(&mut self, text: String) {
        Self::push(&mut self.searches, text);
    }

    fn top(q: &VecDeque<(Instant, String)>) -> Vec<(String, u32)> {
        let now = Instant::now();
        let mut counts: HashMap<String, (String, u32)> = HashMap::new();
        for (t, item) in q.iter().filter(|(t, _)| now.duration_since(*t) <= WINDOW) {
            let _ = t;
            counts.entry(item.to_lowercase()).or_insert_with(|| (item.clone(), 0)).1 += 1;
        }
        let mut v: Vec<(String, u32)> = counts.into_values().collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.to_lowercase().cmp(&b.0.to_lowercase())));
        v.truncate(CHART_LEN);
        v
    }
}

/// How a file shows up in the charts: "Artist - Title", from its tags if
/// we have them, else its file name.
pub fn song_label(s: &Session, filename: &str) -> String {
    match s.meta.get(filename) {
        Some(m) if !m.artist.is_empty() && !m.title.is_empty() => format!("{} - {}", m.artist, m.title),
        _ => file_label(filename),
    }
}

/// "Artist - Title" from a file name alone.
pub fn file_label(filename: &str) -> String {
    let base = napster_proto::share::basename(filename);
    base.rsplit_once('.').map(|(stem, _)| stem).unwrap_or(base).to_owned()
}

/// Lower-cased artists in a collection, with how many files each.
fn artists(s: &Session) -> HashMap<String, (String, u32)> {
    let mut out: HashMap<String, (String, u32)> = HashMap::new();
    for m in s.meta.values() {
        let a = m.artist.trim();
        if a.is_empty() || a.eq_ignore_ascii_case("unknown") || a.eq_ignore_ascii_case("various artists") {
            continue;
        }
        out.entry(a.to_lowercase()).or_insert_with(|| (a.to_owned(), 0)).1 += 1;
    }
    out
}

/// (genre, files, users), most files first.
pub fn genres(st: &State) -> Vec<(String, u32, u32)> {
    let mut by: BTreeMap<String, (String, u32, HashSet<&str>)> = BTreeMap::new();
    for s in st.sessions.values() {
        for m in s.meta.values().filter(|m| !m.genre.is_empty()) {
            let e = by.entry(m.genre.to_lowercase()).or_insert_with(|| (m.genre.clone(), 0, HashSet::new()));
            e.1 += 1;
            e.2.insert(&s.nick);
        }
    }
    let mut v: Vec<(String, u32, u32)> = by.into_values().map(|(g, n, users)| (g, n, users.len() as u32)).collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    v
}

/// Files tagged with `genre` shared by anyone but `me`.
pub fn genre_files(st: &State, genre: &str, me: &str, max: usize) -> Vec<SearchResult> {
    let mut out = Vec::new();
    for s in st.sessions.values().filter(|s| !s.nick.eq_ignore_ascii_case(me)) {
        for f in &s.files {
            if out.len() >= max {
                return out;
            }
            if s.meta.get(&f.filename).is_some_and(|m| m.genre.eq_ignore_ascii_case(genre)) {
                out.push(SearchResult { file: f.clone(), nick: s.nick.clone(), ip: s.ip, link_speed: s.link_speed });
            }
        }
    }
    out
}

/// (chart, entries): most requested songs, hottest searches, and the
/// artists shared by the most people right now.
pub fn charts(st: &State) -> Vec<(&'static str, Vec<(String, u32)>)> {
    let mut by_artist: HashMap<String, (String, u32)> = HashMap::new();
    for s in st.sessions.values() {
        for (key, (name, _)) in artists(s) {
            by_artist.entry(key).or_insert_with(|| (name, 0)).1 += 1;
        }
    }
    let mut artists: Vec<(String, u32)> = by_artist.into_values().collect();
    artists.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.to_lowercase().cmp(&b.0.to_lowercase())));
    artists.truncate(CHART_LEN);
    vec![
        ("songs", Trends::top(&st.trends.downloads)),
        ("searches", Trends::top(&st.trends.searches)),
        ("artists", artists),
    ]
}

pub struct MemberRow {
    pub nick: String,
    pub matching: u32,
    pub files: u32,
    pub speed: u32,
    pub common: Vec<String>,
    pub favorites: Vec<String>,
}

/// Other users ranked by how much their artists overlap `me`'s (Jaccard
/// similarity). If `me` has no tagged music yet, the biggest collections.
pub fn members(st: &State, me: &str, max: usize) -> Vec<MemberRow> {
    let mine = st.session(me).map(artists).unwrap_or_default();
    let mut rows: Vec<MemberRow> = st
        .sessions
        .values()
        .filter(|s| !s.nick.eq_ignore_ascii_case(me) && !s.cloaked && !s.files.is_empty())
        .map(|s| {
            let theirs = artists(s);
            let mut common: Vec<String> =
                theirs.iter().filter(|(k, _)| mine.contains_key(*k)).map(|(_, (name, _))| name.clone()).collect();
            common.sort_by_key(|a| a.to_lowercase());
            let union = mine.len() + theirs.len() - common.len();
            let matching = (common.len() * 100).checked_div(union).unwrap_or(0) as u32;
            let mut fav: Vec<(String, u32)> = theirs.into_values().collect();
            fav.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.to_lowercase().cmp(&b.0.to_lowercase())));
            MemberRow {
                nick: s.nick.clone(),
                matching,
                files: s.files.len() as u32,
                speed: s.link_speed,
                common: common.into_iter().take(5).collect(),
                favorites: fav.into_iter().take(3).map(|(a, _)| a).collect(),
            }
        })
        .filter(|r| mine.is_empty() || r.matching > 0)
        .collect();
    rows.sort_by(|a, b| b.matching.cmp(&a.matching).then_with(|| b.files.cmp(&a.files)));
    rows.truncate(max);
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use napster_proto::FileInfo;

    fn session(nick: &str, songs: &[(&str, &str, &str)]) -> Session {
        let mut s = Session {
            id: 0,
            nick: nick.into(),
            tx: tokio::sync::mpsc::unbounded_channel().0,
            kill: Default::default(),
            ip: std::net::Ipv4Addr::LOCALHOST,
            remote_port: 0,
            data_port: 6699,
            link_speed: 4,
            client: "napster-rs".into(),
            level: crate::state::Level::User,
            email: String::new(),
            files: vec![],
            hotlist: Default::default(),
            ignored: Default::default(),
            channels: Default::default(),
            downloads: 0,
            uploads: 0,
            total_downloads: 0,
            total_uploads: 0,
            connected: Instant::now(),
            muzzled: false,
            cloaked: false,
            login_ms: 0,
            announced_shares: 0,
            meta: HashMap::new(),
        };
        for (i, (artist, title, genre)) in songs.iter().enumerate() {
            let filename = format!(r"music\{i}.mp3");
            s.files.push(FileInfo {
                filename: filename.clone(),
                md5: String::new(),
                size: 1,
                bitrate: 128,
                frequency: 44100,
                seconds: 1,
            });
            s.meta.insert(filename, Meta::new(artist, title, genre));
        }
        s
    }

    fn state(sessions: Vec<Session>) -> State {
        let mut st = State::default();
        for s in sessions {
            st.sessions.insert(s.nick.to_lowercase(), s);
        }
        st
    }

    #[test]
    fn meta_is_trimmed_and_clipped() {
        let m = Meta::new("  Beck ", &"x".repeat(500), "Alternative");
        assert_eq!(m.artist, "Beck");
        assert_eq!(m.title.len(), MAX_FIELD);
    }

    #[test]
    fn genres_merge_case_and_count_users() {
        let st = state(vec![
            session("a", &[("X", "1", "Rock"), ("Y", "2", "rock"), ("Z", "3", "Jazz")]),
            session("b", &[("X", "4", "Rock"), ("Q", "5", "")]),
        ]);
        let g = genres(&st);
        assert_eq!(g.len(), 2, "untagged genres are left out: {g:?}");
        assert_eq!((g[0].1, g[0].2), (3, 2));
        assert!(g[0].0.eq_ignore_ascii_case("rock"));
        assert_eq!(g[1], ("Jazz".into(), 1, 1));
    }

    #[test]
    fn genre_files_skip_my_own_and_respect_max() {
        let st = state(vec![
            session("me", &[("X", "1", "Rock")]),
            session("them", &[("X", "2", "Rock"), ("X", "3", "ROCK"), ("Y", "4", "Pop")]),
        ]);
        let f = genre_files(&st, "rock", "ME", 10);
        assert_eq!(f.len(), 2);
        assert!(f.iter().all(|r| r.nick == "them"));
        assert_eq!(genre_files(&st, "rock", "me", 1).len(), 1);
    }

    #[test]
    fn members_rank_by_shared_artists() {
        let st = state(vec![
            session("me", &[("Beck", "1", ""), ("Radiohead", "2", ""), ("Bjork", "3", "")]),
            session("close", &[("beck", "1", ""), ("Radiohead", "2", ""), ("Pixies", "3", "")]),
            session("far", &[("Bjork", "1", ""), ("Metallica", "2", ""), ("Slayer", "3", ""), ("Tool", "4", "")]),
            session("none", &[("Celine Dion", "1", "")]),
            session("empty", &[]),
        ]);
        let m = members(&st, "me", 10);
        let nicks: Vec<&str> = m.iter().map(|r| r.nick.as_str()).collect();
        assert_eq!(nicks, ["close", "far"], "no overlap, no suggestion");
        assert_eq!(m[0].matching, 50); // 2 common of 4 artists
        assert_eq!(m[0].common, ["beck", "Radiohead"]);
        assert_eq!(m[1].matching, 16); // 1 of 6
    }

    #[test]
    fn members_without_tags_get_the_biggest_collections() {
        let st = state(vec![
            session("me", &[]),
            session("small", &[("A", "1", "")]),
            session("big", &[("A", "1", ""), ("B", "2", ""), ("B", "3", "")]),
        ]);
        let m = members(&st, "me", 10);
        assert_eq!(m[0].nick, "big");
        assert_eq!(m[0].favorites[0], "B", "favorites: most files first");
        assert_eq!(m.len(), 2);
    }

    #[test]
    fn hidden_users_are_not_suggested() {
        let mut cloaked = session("ghost", &[("Beck", "1", "")]);
        cloaked.cloaked = true;
        let st = state(vec![session("me", &[("Beck", "1", "")]), cloaked]);
        assert!(members(&st, "me", 10).is_empty());
    }

    #[test]
    fn charts_count_case_insensitively() {
        let mut st = state(vec![
            session("a", &[("Beck", "Loser", "")]),
            session("b", &[("beck", "Devils Haircut", ""), ("Pixies", "Debaser", "")]),
        ]);
        for s in ["Beck", "beck", "pixies"] {
            st.trends.search(s.into());
        }
        st.trends.download("Beck - Loser".into());
        let c = charts(&st);
        assert_eq!(c[0].1, vec![("Beck - Loser".to_owned(), 1)]);
        assert_eq!(c[1].1[0], ("Beck".to_owned(), 2));
        assert_eq!(c[2].1[0].1, 2, "Beck is shared by both users");
        assert_eq!(c[2].1[1], ("Pixies".to_owned(), 1));
    }

    #[test]
    fn labels_prefer_tags() {
        let s = session("a", &[("Beck", "Loser", "")]);
        assert_eq!(song_label(&s, r"music\0.mp3"), "Beck - Loser");
        assert_eq!(song_label(&s, r"C:\mp3\Pixies - Debaser.mp3"), "Pixies - Debaser");
    }

    #[test]
    fn trends_are_bounded() {
        let mut t = Trends::default();
        for i in 0..MAX_EVENTS + 10 {
            t.search(i.to_string());
        }
        assert_eq!(t.searches.len(), MAX_EVENTS);
    }
}
