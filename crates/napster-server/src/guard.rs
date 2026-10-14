//! Abuse limits that span connections: connections per address,
//! failed-login lockouts and account sign-up limits.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::Shared;

/// Failed logins allowed per address before it is locked out.
pub const MAX_LOGIN_FAILURES: u32 = 5;
pub const LOGIN_LOCKOUT: Duration = Duration::from_secs(10 * 60);
/// New accounts allowed per address per window.
pub const MAX_SIGNUPS: u32 = 5;
pub const SIGNUP_WINDOW: Duration = Duration::from_secs(60 * 60);

#[derive(Default)]
pub(crate) struct Guard {
    conns: HashMap<IpAddr, usize>,
    total: usize,
    failures: HashMap<IpAddr, Window>,
    signups: HashMap<IpAddr, Window>,
}

#[derive(Clone, Copy)]
struct Window {
    count: u32,
    since: Instant,
}

/// Count an event in a fixed window; returns the count including it.
fn bump(map: &mut HashMap<IpAddr, Window>, ip: IpAddr, window: Duration) -> u32 {
    let now = Instant::now();
    if map.len() > 10_000 {
        map.retain(|_, w| now.duration_since(w.since) < window);
    }
    let w = map.entry(ip).or_insert(Window { count: 0, since: now });
    if now.duration_since(w.since) >= window {
        *w = Window { count: 0, since: now };
    }
    w.count += 1;
    w.count
}

fn current(map: &HashMap<IpAddr, Window>, ip: IpAddr, window: Duration) -> u32 {
    map.get(&ip).filter(|w| w.since.elapsed() < window).map(|w| w.count).unwrap_or(0)
}

impl Guard {
    pub fn login_locked(&self, ip: IpAddr) -> bool {
        current(&self.failures, ip, LOGIN_LOCKOUT) >= MAX_LOGIN_FAILURES
    }

    pub fn login_failed(&mut self, ip: IpAddr) {
        bump(&mut self.failures, ip, LOGIN_LOCKOUT);
    }

    pub fn login_ok(&mut self, ip: IpAddr) {
        self.failures.remove(&ip);
    }

    /// Record a sign-up if allowed.
    pub fn try_signup(&mut self, ip: IpAddr) -> bool {
        if current(&self.signups, ip, SIGNUP_WINDOW) >= MAX_SIGNUPS {
            return false;
        }
        bump(&mut self.signups, ip, SIGNUP_WINDOW);
        true
    }
}

/// Held for the life of a connection; frees its slot when dropped.
pub(crate) struct ConnSlot {
    shared: Arc<Shared>,
    ip: IpAddr,
}

impl Drop for ConnSlot {
    fn drop(&mut self) {
        let mut g = self.shared.guard();
        g.total = g.total.saturating_sub(1);
        if let Some(n) = g.conns.get_mut(&self.ip) {
            *n -= 1;
            if *n == 0 {
                g.conns.remove(&self.ip);
            }
        }
    }
}

/// Admit a new connection from `ip`, or `None` if it's over the limits.
pub(crate) fn admit(shared: &Arc<Shared>, ip: IpAddr) -> Option<ConnSlot> {
    let (per_ip, max) = (shared.config.max_conns_per_ip, shared.config.max_connections);
    let mut g = shared.guard();
    let n = g.conns.get(&ip).copied().unwrap_or(0);
    if (per_ip > 0 && n >= per_ip) || (max > 0 && g.total >= max) {
        return None;
    }
    g.conns.insert(ip, n + 1);
    g.total += 1;
    Some(ConnSlot { shared: shared.clone(), ip })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lockout_after_failures() {
        let mut g = Guard::default();
        let ip: IpAddr = "10.0.0.1".parse().unwrap();
        for _ in 0..MAX_LOGIN_FAILURES - 1 {
            g.login_failed(ip);
        }
        assert!(!g.login_locked(ip));
        g.login_failed(ip);
        assert!(g.login_locked(ip));
        assert!(!g.login_locked("10.0.0.2".parse().unwrap()));
        g.login_ok(ip);
        assert!(!g.login_locked(ip));
    }

    #[test]
    fn signup_limit() {
        let mut g = Guard::default();
        let ip: IpAddr = "10.0.0.1".parse().unwrap();
        for _ in 0..MAX_SIGNUPS {
            assert!(g.try_signup(ip));
        }
        assert!(!g.try_signup(ip));
    }
}
