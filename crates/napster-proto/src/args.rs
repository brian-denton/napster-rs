//! Field tokenizer matching the original client's parser (`0x42df60` in
//! napster.exe): fields are separated by spaces, a field starting with `"`
//! runs to the next `"` with no escaping.

/// Iterator-like cursor over the fields of a message payload.
#[derive(Debug, Clone)]
pub struct Args<'a> {
    rest: &'a str,
}

impl<'a> Args<'a> {
    pub fn new(s: &'a str) -> Self {
        Self { rest: s }
    }

    /// Next field, with surrounding quotes removed.
    pub fn next_field(&mut self) -> Option<&'a str> {
        let s = self.rest.trim_start_matches(' ');
        if s.is_empty() {
            self.rest = s;
            return None;
        }
        if let Some(body) = s.strip_prefix('"') {
            match body.find('"') {
                Some(end) => {
                    self.rest = &body[end + 1..];
                    Some(&body[..end])
                }
                None => {
                    self.rest = "";
                    Some(body)
                }
            }
        } else {
            let end = s.find(' ').unwrap_or(s.len());
            self.rest = &s[end..];
            Some(&s[..end])
        }
    }

    pub fn str(&mut self) -> Option<&'a str> {
        self.next_field()
    }

    pub fn num<T: std::str::FromStr>(&mut self) -> Option<T> {
        self.next_field()?.parse().ok()
    }

    /// Everything not yet consumed, without the separating space.
    pub fn rest(&mut self) -> &'a str {
        let r = self.rest.strip_prefix(' ').unwrap_or(self.rest);
        self.rest = "";
        r
    }

    pub fn is_empty(&self) -> bool {
        self.rest.trim().is_empty()
    }
}

impl<'a> Iterator for Args<'a> {
    type Item = &'a str;
    fn next(&mut self) -> Option<&'a str> {
        self.next_field()
    }
}

/// Wrap a string in double quotes, dropping any embedded quotes since the
/// protocol has no way to escape them.
pub fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    out.extend(s.chars().filter(|&c| c != '"'));
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_quoted() {
        let mut a = Args::new("\"random band - random song.mp3\" 7d733c1e 2558199 128 44100 159 lefty 3437166285 4");
        assert_eq!(a.str(), Some("random band - random song.mp3"));
        assert_eq!(a.str(), Some("7d733c1e"));
        assert_eq!(a.num::<u64>(), Some(2558199));
        assert_eq!(a.by_ref().count(), 6);
    }

    #[test]
    fn rest_keeps_spaces() {
        let mut a = Args::new("80's espinozaf hello  there");
        assert_eq!(a.str(), Some("80's"));
        assert_eq!(a.str(), Some("espinozaf"));
        assert_eq!(a.rest(), "hello  there");
    }

    #[test]
    fn empty_quoted() {
        let mut a = Args::new("lefty \"\" 3");
        assert_eq!(a.str(), Some("lefty"));
        assert_eq!(a.str(), Some(""));
        assert_eq!(a.num::<u32>(), Some(3));
        assert_eq!(a.str(), None);
    }

    #[test]
    fn quote_strips() {
        assert_eq!(quote("a \"b\" c"), "\"a b c\"");
    }
}
