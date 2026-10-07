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
    /// Through the VPN beside the tunnel; see `Serving::set_vpn`.
    Vpn,
}

impl std::fmt::Display for Route {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Proxy => "proxy",
            Self::Direct => "direct",
            Self::Blocked => "blocked",
            Self::Vpn => "vpn",
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

    // `active()` and `recent()` used to be separate readers, and every caller
    // wanted both. Composing them was the bug: two locks taken one after the
    // other are two moments, and a connection can close in between. `table()`
    // is the only way to read the rows now, so it cannot be reassembled wrongly.

    /// The whole table as the connections screen shows it: live first, newest
    /// of each group first, then the recently closed.
    ///
    /// Taken under both locks at once, and that is the point. Reading `active`
    /// and then `recent` as two calls left a gap: a connection that closed
    /// between them appeared in both lists, so the same `id` arrived twice —
    /// and the table keys its rows by `id`.
    ///
    /// Lock order is active then recent, the same order `close` acquires them
    /// in, so the two can never wedge against each other.
    pub fn table(&self) -> Vec<Snapshot> {
        let active = self.active.read().expect("tracker lock");
        let recent = self.recent.read().expect("tracker lock");

        let mut rows: Vec<Snapshot> = active.values().map(|c| snapshot(c, None)).collect();
        rows.sort_by_key(|c| std::cmp::Reverse(c.id));
        rows.extend(recent.iter().rev().cloned());
        rows
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
        assert_eq!(t.table().len(), RECENT_CAP);
        assert_eq!(t.active_count(), 0);
        // Newest first, and the oldest are gone rather than merely hidden.
        assert_eq!(
            t.table()[0].target,
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

    /// The table has to be one consistent view. Taken as two reads, a
    /// connection closing in between landed in the live list and in the recent
    /// list, and the screen keys its rows by id.
    #[test]
    fn the_table_never_shows_one_connection_twice() {
        let t = Tracker::new();
        let a = t.open(Target::domain("a.example", 443), Route::Proxy, "r", None);
        let b = t.open(Target::domain("b.example", 443), Route::Proxy, "r", None);
        t.close(a.id);

        let rows = t.table();
        let mut ids: Vec<_> = rows.iter().map(|r| r.id).collect();
        let before = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), before, "every row is a different connection");

        // And it is still the whole picture: one live, one closed.
        assert_eq!(rows.len(), 2);
        assert_eq!(rows.iter().filter(|r| r.closed_at.is_none()).count(), 1);
        assert_eq!(rows[0].id, b.id, "live first, newest first");
    }
}
