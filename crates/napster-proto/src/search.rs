//! Search request (200) grammar.
//!
//! The b10 client builds requests from these fragments (strings at
//! 0x49b374..0x49b3d8 in napster.exe):
//!
//! ```text
//! FILENAME CONTAINS "<artist>" MAX_RESULTS <n> FILENAME CONTAINS "<title>"
//!     LINESPEED "<cmp>" <speed> BITRATE "<cmp>" "<kbps>"
//! ```
//!
//! with `<cmp>` one of `AT LEAST`, `AT BEST`, `EQUAL TO`.

use crate::args::{quote, Args};
use crate::share::FileInfo;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compare {
    AtLeast,
    AtBest,
    EqualTo,
}

impl Compare {
    pub const ALL: [Compare; 3] = [Compare::AtLeast, Compare::EqualTo, Compare::AtBest];

    pub fn as_str(self) -> &'static str {
        match self {
            Compare::AtLeast => "AT LEAST",
            Compare::AtBest => "AT BEST",
            Compare::EqualTo => "EQUAL TO",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_uppercase().as_str() {
            "AT LEAST" => Some(Compare::AtLeast),
            "AT BEST" => Some(Compare::AtBest),
            "EQUAL TO" => Some(Compare::EqualTo),
            _ => None,
        }
    }

    pub fn test(self, actual: u32, wanted: u32) -> bool {
        match self {
            Compare::AtLeast => actual >= wanted,
            Compare::AtBest => actual <= wanted,
            Compare::EqualTo => actual == wanted,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchQuery {
    /// Each `FILENAME CONTAINS` phrase.
    pub terms: Vec<String>,
    pub max_results: u32,
    pub link_speed: Option<(Compare, u32)>,
    pub bitrate: Option<(Compare, u32)>,
    pub frequency: Option<(Compare, u32)>,
    pub wma_only: bool,
    pub local_only: bool,
}

impl Default for SearchQuery {
    fn default() -> Self {
        Self {
            terms: vec![],
            max_results: 100,
            link_speed: None,
            bitrate: None,
            frequency: None,
            wma_only: false,
            local_only: false,
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SearchParseError {
    #[error("invalid search request")]
    Invalid,
    #[error("search request has no terms")]
    NoTerms,
}

impl SearchQuery {
    pub fn new(text: &str) -> Self {
        Self { terms: vec![text.to_owned()], ..Default::default() }
    }

    /// Serialize in the same order the original client uses.
    pub fn to_payload(&self) -> String {
        let mut parts = Vec::new();
        let mut terms = self.terms.iter().filter(|t| !t.trim().is_empty());
        if let Some(first) = terms.next() {
            parts.push(format!("FILENAME CONTAINS {}", quote(first)));
        }
        parts.push(format!("MAX_RESULTS {}", self.max_results));
        for t in terms {
            parts.push(format!("FILENAME CONTAINS {}", quote(t)));
        }
        if let Some((c, v)) = self.link_speed {
            parts.push(format!("LINESPEED {} {}", quote(c.as_str()), v));
        }
        if let Some((c, v)) = self.bitrate {
            parts.push(format!("BITRATE {} \"{}\"", quote(c.as_str()), v));
        }
        if let Some((c, v)) = self.frequency {
            parts.push(format!("FREQ {} \"{}\"", quote(c.as_str()), v));
        }
        if self.wma_only {
            parts.push("WMA-FILE".into());
        }
        if self.local_only {
            parts.push("LOCAL_ONLY".into());
        }
        parts.join(" ")
    }

    pub fn parse(payload: &str) -> Result<Self, SearchParseError> {
        let mut q = SearchQuery { max_results: 100, ..Default::default() };
        let mut a = Args::new(payload);
        while let Some(tok) = a.next_field() {
            match tok.to_ascii_uppercase().as_str() {
                "FILENAME" => {
                    if !a.str().is_some_and(|t| t.eq_ignore_ascii_case("CONTAINS")) {
                        return Err(SearchParseError::Invalid);
                    }
                    q.terms.push(a.str().ok_or(SearchParseError::Invalid)?.to_owned());
                }
                "MAX_RESULTS" => q.max_results = a.num().ok_or(SearchParseError::Invalid)?,
                "LINESPEED" => q.link_speed = Some(parse_cmp(&mut a)?),
                "BITRATE" => q.bitrate = Some(parse_cmp(&mut a)?),
                "FREQ" => q.frequency = Some(parse_cmp(&mut a)?),
                "WMA-FILE" => q.wma_only = true,
                "LOCAL_ONLY" => q.local_only = true,
                "" => {}
                _ => return Err(SearchParseError::Invalid),
            }
        }
        if q.words().is_empty() {
            return Err(SearchParseError::NoTerms);
        }
        Ok(q)
    }

    /// Lower-cased words that must all appear in a matching file name.
    pub fn words(&self) -> Vec<String> {
        self.terms
            .iter()
            .flat_map(|t| t.split(|c: char| c.is_whitespace() || c == '-' || c == '_'))
            .filter(|w| !w.is_empty())
            .map(|w| w.to_lowercase())
            .collect()
    }

    /// Does `file`, shared by someone on `link_speed`, satisfy the query?
    /// `words` should come from [`SearchQuery::words`] (computed once).
    pub fn matches(&self, words: &[String], file: &FileInfo, link_speed: u32) -> bool {
        let name = file.filename.to_lowercase();
        if !words.iter().all(|w| name.contains(w.as_str())) {
            return false;
        }
        if let Some((c, v)) = self.link_speed {
            if !c.test(link_speed, v) {
                return false;
            }
        }
        if let Some((c, v)) = self.bitrate {
            if !c.test(file.bitrate, v) {
                return false;
            }
        }
        if let Some((c, v)) = self.frequency {
            if !c.test(file.frequency, v) {
                return false;
            }
        }
        if self.wma_only && !name.ends_with(".wma") {
            return false;
        }
        true
    }
}

fn parse_cmp(a: &mut Args<'_>) -> Result<(Compare, u32), SearchParseError> {
    let first = a.str().ok_or(SearchParseError::Invalid)?;
    let cmp = match Compare::parse(first) {
        Some(c) => c,
        // unquoted form: AT LEAST 128
        None => {
            let second = a.str().ok_or(SearchParseError::Invalid)?;
            Compare::parse(&format!("{first} {second}")).ok_or(SearchParseError::Invalid)?
        }
    };
    let v = a.num().ok_or(SearchParseError::Invalid)?;
    Ok((cmp, v))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_b10_format() {
        // exactly what the b10 format strings produce (note trailing space)
        let p = "FILENAME CONTAINS \"sneaker pimps\" MAX_RESULTS 100 FILENAME CONTAINS \"spin spin sugar\" LINESPEED \"AT LEAST\" 3 BITRATE \"AT LEAST\" \"128\" ";
        let q = SearchQuery::parse(p).unwrap();
        assert_eq!(q.terms, vec!["sneaker pimps", "spin spin sugar"]);
        assert_eq!(q.max_results, 100);
        assert_eq!(q.link_speed, Some((Compare::AtLeast, 3)));
        assert_eq!(q.bitrate, Some((Compare::AtLeast, 128)));
        assert_eq!(q.words(), vec!["sneaker", "pimps", "spin", "spin", "sugar"]);
    }

    #[test]
    fn roundtrip() {
        let q = SearchQuery {
            terms: vec!["metallica".into(), "one".into()],
            max_results: 50,
            bitrate: Some((Compare::EqualTo, 192)),
            frequency: Some((Compare::AtBest, 44100)),
            ..Default::default()
        };
        assert_eq!(SearchQuery::parse(&q.to_payload()).unwrap(), q);
    }

    #[test]
    fn rejects_garbage() {
        assert_eq!(SearchQuery::parse("MAX_RESULTS 10"), Err(SearchParseError::NoTerms));
        assert_eq!(SearchQuery::parse("FOO BAR"), Err(SearchParseError::Invalid));
    }

    #[test]
    fn matching() {
        let q = SearchQuery {
            terms: vec!["generic song".into()],
            bitrate: Some((Compare::AtLeast, 128)),
            ..Default::default()
        };
        let words = q.words();
        let mut f = FileInfo {
            filename: "C:\\MP3\\Generic Band - Generic Song.mp3".into(),
            md5: "x".into(),
            size: 1,
            bitrate: 128,
            frequency: 44100,
            seconds: 1,
        };
        assert!(q.matches(&words, &f, 0));
        f.bitrate = 96;
        assert!(!q.matches(&words, &f, 0));
    }
}
