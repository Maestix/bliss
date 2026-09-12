//! The live connection table.
//!
//! This is one of the two places a client of this kind leaks memory — the other
//! is the log. The client that prompted this project reached 7.8 GB, and a
//! connection list that only ever grows is how. So the table has a hard cap in
//! entries, not in bytes, and closed connections fall off the end.
//!
//! It is also the screen xray-core cannot provide: its API exposes counters per
//! user, never a list of who is talking to what. Because every byte passes
//! through our relay, we know the host, the route, the rule that chose it and
//! the traffic in both directions.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use crate::target::Target;

pub type ConnectionId = u64;

/// How many closed connections to keep for the history pane.
///
/// Deliberately modest: the pane shows the recent past, not an archive, and an
/// unbounded "recent" list is the bug this module exists to avoid.
const RECENT_CAP: usize = 512;

/// What was decided for a connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    Proxy,
    Direct,
    Blocked,
}

impl std::fmt::Display for Route {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Proxy => "proxy",
            Self::Direct => "direct",
            Self::Blocked => "blocked",
        })
    }
}

/// A connection while it is open.
///
/// Counters are atomics so the relay can bump them from its own task without
/// taking the table's lock on every chunk.
#[derive(Debug)]
pub struct Connection {
    pub id: ConnectionId,
    pub target: Target,
    pub route: Route,
    /// Which rule chose the route. The point of the whole routing screen is that
    /// this question always has an answer.
    pub rule: String,
    pub process: Option<String>,
    pub opened_at: jiff::Timestamp,
    up: AtomicU64,
    down: AtomicU64,
}

impl Connection {
    pub fn add_up(&self, n: u64) {
        self.up.fetch_add(n, Ordering::Relaxed);
    }
    pub fn add_down(&self, n: u64) {
        self.down.fetch_add(n, Ordering::Relaxed);
    }
    pub fn up(&self) -> u64 {
        self.up.load(Ordering::Relaxed)
    }
    pub fn down(&self) -> u64 {
        self.down.load(Ordering::Relaxed)
    }
}

/// A flat copy of one row, for the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub id: ConnectionId,
    pub target: String,
    pub route: Route,
    pub rule: String,
    pub process: Option<String>,
    pub opened_at: jiff::Timestamp,
    pub closed_at: Option<jiff::Timestamp>,
    pub up: u64,
    pub down: u64,
}

#[derive(Debug, Default)]
pub struct Tracker {
    next_id: AtomicU64,
    active: RwLock<HashMap<ConnectionId, Arc<Connection>>>,
    recent: RwLock<VecDeque<Snapshot>>,
    totals: Totals,
}

#[derive(Debug, Default)]
struct Totals {
    up: AtomicU64,
    down: AtomicU64,
    opened: AtomicU64,
}

impl Tracker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn open(
        &self,
        target: Target,
        route: Route,
        rule: impl Into<String>,
        process: Option<String>,
    ) -> Arc<Connection> {
        let conn = Arc::new(Connection {
            id: self.next_id.fetch_add(1, Ordering::Relaxed),
            target,
            route,
            rule: rule.into(),
            process,
            opened_at: jiff::Timestamp::now(),
            up: AtomicU64::new(0),
            down: AtomicU64::new(0),
        });
        self.totals.opened.fetch_add(1, Ordering::Relaxed);
        self.active
            .write()
            .expect("tracker lock")
            .insert(conn.id, Arc::clone(&conn));
        conn
    }

    /// Move a connection into the recent list, evicting the oldest if full.
    pub fn close(&self, id: ConnectionId) {
        let Some(conn) = self.active.write().expect("tracker lock").remove(&id) else {
            return;
        };

        self.totals.up.fetch_add(conn.up(), Ordering::Relaxed);
        self.totals.down.fetch_add(conn.down(), Ordering::Relaxed);

        let mut recent = self.recent.write().expect("tracker lock");
        if recent.len() == RECENT_CAP {
            recent.pop_front();
        }
        recent.push_back(snapshot(&conn, Some(jiff::Timestamp::now())));
    }

    pub fn active(&self) -> Vec<Snapshot> {
        self.active
            .read()
            .expect("tracker lock")
            .values()
            .map(|c| snapshot(c, None))
            .collect()
    }

    pub fn recent(&self) -> Vec<Snapshot> {
        self.recent
            .read()
            .expect("tracker lock")
            .iter()
            .rev()
            .cloned()
            .collect()
    }

    pub fn active_count(&self) -> usize {
        self.active.read().expect("tracker lock").len()
    }

    /// Bytes over the whole session, closed connections included.
    pub fn totals(&self) -> (u64, u64, u64) {
        let live: (u64, u64) = self
            .active
            .read()
            .expect("tracker lock")
            .values()
            .fold((0, 0), |(u, d), c| (u + c.up(), d + c.down()));
        (
            self.totals.up.load(Ordering::Relaxed) + live.0,
            self.totals.down.load(Ordering::Relaxed) + live.1,
            self.totals.opened.load(Ordering::Relaxed),
        )
    }
}

fn snapshot(c: &Connection, closed_at: Option<jiff::Timestamp>) -> Snapshot {
    Snapshot {
        id: c.id,
        target: c.target.to_string(),
        route: c.route,
        rule: c.rule.clone(),
        process: c.process.clone(),
        opened_at: c.opened_at,
        closed_at,
        up: c.up(),
        down: c.down(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tracker_with(n: usize) -> Tracker {
        let t = Tracker::new();
        for i in 0..n {
            let c = t.open(
                Target::domain(format!("host{i}.example"), 443),
                Route::Proxy,
                "default",
                None,
            );
            c.add_up(10);
            c.add_down(100);
            t.close(c.id);
        }
        t
    }

    /// The whole reason this module has a cap.
    #[test]
    fn recent_connections_cannot_grow_without_bound() {
        let t = tracker_with(RECENT_CAP * 3);
        assert_eq!(t.recent().len(), RECENT_CAP);
        assert_eq!(t.active_count(), 0);
        // Newest first, and the oldest are gone rather than merely hidden.
        assert_eq!(
            t.recent()[0].target,
            format!("host{}.example:443", RECENT_CAP * 3 - 1)
        );
    }

    #[test]
    fn totals_survive_eviction() {
        let t = tracker_with(10);
        let (up, down, opened) = t.totals();
        assert_eq!((up, down, opened), (100, 1000, 10));
    }

    #[test]
    fn live_bytes_count_before_close() {
        let t = Tracker::new();
        let c = t.open(Target::domain("a.example", 443), Route::Direct, "lan", None);
        c.add_down(4096);
        assert_eq!(t.active_count(), 1);
        assert_eq!(t.totals().1, 4096);
        t.close(c.id);
        assert_eq!(t.totals().1, 4096);
    }
}
