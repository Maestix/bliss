//! Which program opened a connection.
//!
//! A client connects to our inbound from an ephemeral local port; Windows knows
//! which process owns that port, so we can name it. This is the column xray-core
//! can never fill — its API reports counters per user, and it never sees the
//! local socket at all — and it is what makes per-app rules possible later.
//!
//! The lookup enumerates every TCP connection on the machine, which is far too
//! expensive to do per connection, so the table is cached briefly. A stale entry
//! is harmless: ports are not reused within the cache window in practice, and
//! the worst case is an unnamed row. Where the answer decides who to trust —
//! OpenVPN calling bliss back — `process_of` reads one exact row, fresh.
//!
//! A connection is asked about by both its ends (`name_for_connection`).
//! Windows gives every connection a local port of its own, but Linux gives
//! one port to connections to different places, and there the port alone
//! can name another program — and a `process:` rule decide by it.

#[cfg(windows)]
mod imp {
    use std::collections::HashMap;
    use std::net::{Ipv4Addr, SocketAddr};
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::NetworkManagement::IpHelper::{
        GetExtendedTcpTable, GetExtendedUdpTable, MIB_TCPROW_OWNER_PID, MIB_TCPTABLE_OWNER_PID,
        MIB_UDPTABLE_OWNER_PID, TCP_TABLE_OWNER_PID_ALL, UDP_TABLE_OWNER_PID,
    };
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_NAME_FORMAT, PROCESS_QUERY_INFORMATION,
        PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
    };

    /// How long a port table stays usable. Long enough that a burst of new
    /// connections costs one enumeration, short enough that a port which moved
    /// to another process is not mislabelled for long.
    const TTL: Duration = Duration::from_millis(750);

    /// One table each. A TCP port and a UDP port of the same number belong to
    /// different programs as often as not, and a machine carrying no datagrams
    /// never pays for the second enumeration.
    static STREAMS: Mutex<Option<Cache>> = Mutex::new(None);
    static DATAGRAMS: Mutex<Option<Cache>> = Mutex::new(None);

    struct Cache {
        taken: Instant,
        ports: HashMap<u16, u32>,
        names: HashMap<u32, Option<String>>,
    }

    /// Which program has this local TCP port.
    pub fn name_for_port(port: u16) -> Option<String> {
        named(&STREAMS, port, port_table)
    }

    /// Which program has this local UDP port.
    ///
    /// Datagrams have their own table, and a rule about a program applies to a
    /// call as much as to a page: without this, `process:Discord.exe` would
    /// match everything Discord loads and none of what it says.
    pub fn name_for_datagram_port(port: u16) -> Option<String> {
        named(&DATAGRAMS, port, datagram_table)
    }

    /// Which program holds the TCP connection from `from` to `to`: the one
    /// holding its local port, which on Windows is that connection's alone.
    pub fn name_for_connection(from: SocketAddr, _to: SocketAddr) -> Option<String> {
        name_for_port(from.port())
    }

    fn named(
        cache: &Mutex<Option<Cache>>,
        port: u16,
        table: fn() -> Option<HashMap<u16, u32>>,
    ) -> Option<String> {
        let mut guard = cache.lock().ok()?;

        let stale = guard
            .as_ref()
            .is_none_or(|c| c.taken.elapsed() > TTL || !c.ports.contains_key(&port));
        if stale {
            *guard = Some(Cache {
                taken: Instant::now(),
                ports: table()?,
                names: HashMap::new(),
            });
        }

        let cache = guard.as_mut()?;
        let pid = *cache.ports.get(&port)?;
        if let Some(known) = cache.names.get(&pid) {
            return known.clone();
        }
        let name = image_name(pid);
        cache.names.insert(pid, name.clone());
        name
    }

    /// The process that opened the TCP connection from `from` to `to`, read
    /// fresh from the machine's table.
    ///
    /// One exact row, where `name_for_port` settles for whoever held a port
    /// number when the table was last read, and with no need to open the
    /// process at all — which a process started by a service may not allow.
    pub fn process_of(from: SocketAddr, to: SocketAddr) -> Option<u32> {
        let (SocketAddr::V4(from), SocketAddr::V4(to)) = (from, to) else {
            return None;
        };
        let mut found = None;
        stream_rows(|row| {
            if address(row.dwLocalAddr) == *from.ip()
                && port(row.dwLocalPort) == from.port()
                && address(row.dwRemoteAddr) == *to.ip()
                && port(row.dwRemotePort) == to.port()
            {
                found = Some(row.dwOwningPid);
            }
        })?;
        found
    }

    /// Local port -> owning pid, for every TCP connection on the machine.
    fn port_table() -> Option<HashMap<u16, u32>> {
        let mut ports = HashMap::new();
        stream_rows(|row| {
            ports.insert(port(row.dwLocalPort), row.dwOwningPid);
        })?;
        Some(ports)
    }

    /// A port as the tables hold it: in network byte order, in the low word.
    fn port(field: u32) -> u16 {
        u16::from_be((field & 0xFFFF) as u16)
    }

    /// An address as the tables hold it: its four bytes in network order.
    fn address(field: u32) -> Ipv4Addr {
        Ipv4Addr::from(field.to_ne_bytes())
    }

    /// Every row of the machine's IPv4 TCP table, handed to `each` in turn.
    fn stream_rows(mut each: impl FnMut(&MIB_TCPROW_OWNER_PID)) -> Option<()> {
        let mut size = 0u32;
        // First call sizes the buffer; it is expected to "fail" with
        // ERROR_INSUFFICIENT_BUFFER, so the return value is deliberately ignored.
        unsafe {
            let _ = GetExtendedTcpTable(
                None,
                &mut size,
                false,
                windows::Win32::Networking::WinSock::AF_INET.0 as u32,
                TCP_TABLE_OWNER_PID_ALL,
                0,
            );
        }
        if size == 0 {
            return None;
        }

        let mut buf = vec![0u8; size as usize];
        let rc = unsafe {
            GetExtendedTcpTable(
                Some(buf.as_mut_ptr().cast()),
                &mut size,
                false,
                windows::Win32::Networking::WinSock::AF_INET.0 as u32,
                TCP_TABLE_OWNER_PID_ALL,
                0,
            )
        };
        if rc != 0 {
            return None;
        }

        // SAFETY: the buffer now holds a MIB_TCPTABLE_OWNER_PID followed by
        // dwNumEntries rows, which is exactly what the API contracts to write.
        let table = unsafe { &*(buf.as_ptr() as *const MIB_TCPTABLE_OWNER_PID) };
        let rows = unsafe {
            std::slice::from_raw_parts(table.table.as_ptr(), table.dwNumEntries as usize)
        };

        rows.iter().for_each(&mut each);
        Some(())
    }

    /// Local port -> owning pid, for every UDP socket on the machine.
    ///
    /// The same two-call shape as the TCP one, against a different table with
    /// a different row: a UDP socket has no far end, so a row is a bound port
    /// and nothing else.
    fn datagram_table() -> Option<HashMap<u16, u32>> {
        let mut size = 0u32;
        unsafe {
            let _ = GetExtendedUdpTable(
                None,
                &mut size,
                false,
                windows::Win32::Networking::WinSock::AF_INET.0 as u32,
                UDP_TABLE_OWNER_PID,
                0,
            );
        }
        if size == 0 {
            return None;
        }

        let mut buf = vec![0u8; size as usize];
        let rc = unsafe {
            GetExtendedUdpTable(
                Some(buf.as_mut_ptr().cast()),
                &mut size,
                false,
                windows::Win32::Networking::WinSock::AF_INET.0 as u32,
                UDP_TABLE_OWNER_PID,
                0,
            )
        };
        if rc != 0 {
            return None;
        }

        // SAFETY: as above, the buffer holds a MIB_UDPTABLE_OWNER_PID followed
        // by dwNumEntries rows.
        let table = unsafe { &*(buf.as_ptr() as *const MIB_UDPTABLE_OWNER_PID) };
        let rows = unsafe {
            std::slice::from_raw_parts(table.table.as_ptr(), table.dwNumEntries as usize)
        };

        Some(
            rows.iter()
                .map(|row| (port(row.dwLocalPort), row.dwOwningPid))
                .collect(),
        )
    }

    pub(super) fn image_name(pid: u32) -> Option<String> {
        // LIMITED_INFORMATION is the least we can ask for and still get a
        // path, and works against processes at a higher integrity level. But
        // a process whose maker wrote its access list itself may grant only
        // the older, wider right, and Windows checks the very bit asked for:
        // OpenVPN's service gives the user QUERY_INFORMATION and not LIMITED,
        // so asking for LIMITED is refused (ERROR_ACCESS_DENIED, measured).
        // That refusal had bliss turn OpenVPN's call away as a stranger's.
        let handle = [PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_QUERY_INFORMATION]
            .into_iter()
            .find_map(|access| unsafe { OpenProcess(access, false, pid) }.ok())?;

        let mut buf = [0u16; 260];
        let mut len = buf.len() as u32;
        let result = unsafe {
            QueryFullProcessImageNameW(
                handle,
                PROCESS_NAME_FORMAT(0),
                windows::core::PWSTR(buf.as_mut_ptr()),
                &mut len,
            )
        };
        unsafe {
            let _ = CloseHandle(handle);
        }
        result.ok()?;

        let path = String::from_utf16_lossy(&buf[..len as usize]);
        // The file name is what a rule matches on and what the UI shows; the
        // full path is noise in a table.
        path.rsplit(['\\', '/']).next().map(str::to_owned)
    }
}

/// Linux keeps no table of sockets by process. It keeps the two halves of
/// one: `/proc/net/{tcp,tcp6,udp,udp6}`, a socket per line with its inode, and
/// `/proc/<pid>/fd/*`, where a socket is a link reading `socket:[<inode>]`.
/// Joining them is what `ss -p` does, at the price of a `readlink` for every
/// open file of every process — tens of thousands on a desktop with a browser
/// up, tens of milliseconds, and the inbound asks once per connection. So:
///
/// - the socket tables, a few kilobytes, are read as Windows' are, and kept
///   for the same 750 ms;
/// - a socket's owner is looked for first among the processes that owned the
///   last few found (`HOT`): connections come in bursts from one browser,
///   whose own descriptors are a few hundred links;
/// - and only then in every process, at most once per `FULL_SCAN`. A socket
///   that turns up between two such looks goes unnamed rather than costing
///   another.
///
/// Only the user's own processes can be looked into — anyone else's `fd` is
/// closed to us — which is every program that would use bliss's ports.
#[cfg(target_os = "linux")]
mod imp {
    use std::collections::{HashMap, VecDeque};
    use std::net::SocketAddr;
    use std::os::unix::ffi::OsStrExt;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    use super::proc_net::{self, Row};

    /// How long a socket table and the names read with it stay usable: as on
    /// Windows.
    const TTL: Duration = Duration::from_millis(750);
    /// The least time between two looks into every process.
    const FULL_SCAN: Duration = Duration::from_millis(200);
    /// How many recent owners are tried before every process is.
    const HOT: usize = 8;
    /// The most program names remembered between two tables.
    const NAMES: usize = 256;

    static STATE: Mutex<State> = Mutex::new(State {
        streams: None,
        datagrams: None,
        owners: None,
        scanned: None,
        hot: VecDeque::new(),
        names: None,
    });

    struct State {
        streams: Option<Table>,
        datagrams: Option<Table>,
        /// inode -> pid, from the last look into every process.
        owners: Option<HashMap<u64, u32>>,
        scanned: Option<Instant>,
        /// The owners found last, newest first.
        hot: VecDeque<u32>,
        names: Option<(Instant, HashMap<u32, Option<String>>)>,
    }

    struct Table {
        taken: Instant,
        sockets: proc_net::Sockets,
    }

    /// Which program has this local TCP port.
    pub fn name_for_port(port: u16) -> Option<String> {
        let mut state = STATE.lock().ok()?;
        let inode = inode_in(&mut state.streams, proc_net::streams, |sockets| {
            sockets.ports.get(&port).copied()
        })?;
        let pid = owner(&mut state, inode, false)?;
        name(&mut state, pid)
    }

    /// Which program has this local UDP port.
    pub fn name_for_datagram_port(port: u16) -> Option<String> {
        let mut state = STATE.lock().ok()?;
        let inode = inode_in(&mut state.datagrams, proc_net::datagrams, |sockets| {
            sockets.ports.get(&port).copied()
        })?;
        let pid = owner(&mut state, inode, false)?;
        name(&mut state, pid)
    }

    /// Which program holds the TCP connection from `from` to `to`.
    ///
    /// By both ends, from the table read last or from a fresh one: the port
    /// alone is somebody's whenever another connection shares it. Only when
    /// even a fresh table has no such connection is the port asked about —
    /// an address spelled in a way bliss did not foresee — and then it can
    /// misname only a connection that has closed already.
    pub fn name_for_connection(from: SocketAddr, to: SocketAddr) -> Option<String> {
        let mut state = STATE.lock().ok()?;
        let inode = inode_in(&mut state.streams, proc_net::streams, |sockets| {
            sockets.connection(from, to)
        })
        .or_else(|| {
            let table = state.streams.as_ref()?;
            table.sockets.ports.get(&from.port()).copied()
        })?;
        let pid = owner(&mut state, inode, false)?;
        name(&mut state, pid)
    }

    /// The process that opened the TCP connection from `from` to `to`, read
    /// fresh. Every process is looked into if it has to be: this is asked
    /// rarely, and its answer decides who is trusted.
    pub fn process_of(from: SocketAddr, to: SocketAddr) -> Option<u32> {
        let (from, to) = (proc_net::canonical(from), proc_net::canonical(to));
        let rows = proc_net::streams()?;
        let row = rows
            .iter()
            .find(|row| row.local == from && row.remote == to)?;
        let inode = row.inode;
        let mut state = STATE.lock().ok()?;
        owner(&mut state, inode, true)
    }

    /// The socket `find` picks from a table, which is read again when it is
    /// old or does not have what is asked for.
    fn inode_in(
        table: &mut Option<Table>,
        read: fn() -> Option<Vec<Row>>,
        find: impl Fn(&proc_net::Sockets) -> Option<u64>,
    ) -> Option<u64> {
        if let Some(known) = table.as_ref()
            && known.taken.elapsed() <= TTL
            && let Some(inode) = find(&known.sockets)
        {
            return Some(inode);
        }
        let fresh = table.insert(Table {
            taken: Instant::now(),
            sockets: proc_net::Sockets::new(&read()?),
        });
        find(&fresh.sockets)
    }

    fn owner(state: &mut State, inode: u64, any_time: bool) -> Option<u32> {
        if let Some(pid) = state.owners.as_ref().and_then(|o| o.get(&inode)) {
            return Some(*pid);
        }
        let hot: Vec<u32> = state.hot.iter().copied().collect();
        let found = hot
            .into_iter()
            .find(|pid| {
                let mut has = false;
                sockets_of(*pid, |socket| has |= socket == inode);
                has
            })
            .or_else(|| {
                let due = state.scanned.is_none_or(|at| at.elapsed() >= FULL_SCAN);
                if !(due || any_time) {
                    return None;
                }
                let owners = every_socket();
                state.scanned = Some(Instant::now());
                let pid = owners.get(&inode).copied();
                state.owners = Some(owners);
                pid
            })?;
        remember_owner(&mut state.hot, found);
        Some(found)
    }

    /// Put `pid` first among the recent owners, which stay `HOT` at most:
    /// each one is a walk through its descriptors on a miss.
    fn remember_owner(hot: &mut VecDeque<u32>, pid: u32) {
        hot.retain(|known| *known != pid);
        hot.push_front(pid);
        hot.truncate(HOT);
    }

    fn name(state: &mut State, pid: u32) -> Option<String> {
        names_now(&mut state.names)
            .entry(pid)
            .or_insert_with(|| image_name(pid))
            .clone()
    }

    /// The names remembered, begun again once they are older than a table or
    /// there are `NAMES` of them.
    fn names_now(
        names: &mut Option<(Instant, HashMap<u32, Option<String>>)>,
    ) -> &mut HashMap<u32, Option<String>> {
        let fresh = names
            .as_ref()
            .is_some_and(|(taken, known)| taken.elapsed() <= TTL && known.len() < NAMES);
        if !fresh {
            *names = None;
        }
        &mut names
            .get_or_insert_with(|| (Instant::now(), HashMap::new()))
            .1
    }

    /// Every socket of every process we may look into, by inode.
    fn every_socket() -> HashMap<u64, u32> {
        let mut owners = HashMap::new();
        let Ok(processes) = std::fs::read_dir("/proc") else {
            return owners;
        };
        for entry in processes.flatten() {
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|name| name.parse::<u32>().ok())
            else {
                continue;
            };
            sockets_of(pid, |inode| claim(&mut owners, inode, pid));
        }
        owners
    }

    /// A socket's owner, as a scan meets its holders. One two processes hold
    /// is the older one's, the lower pid: a child holds its parent's sockets
    /// from the fork until its exec closes them, and a scan that met it there
    /// had a connection named after whatever the child went on to become.
    /// Seen in the tests, where a parallel test's child took this one's.
    fn claim(owners: &mut HashMap<u64, u32>, inode: u64, pid: u32) {
        owners
            .entry(inode)
            .and_modify(|owner| *owner = (*owner).min(pid))
            .or_insert(pid);
    }

    /// The inode of every socket `pid` holds.
    fn sockets_of(pid: u32, mut each: impl FnMut(u64)) {
        let Ok(files) = std::fs::read_dir(format!("/proc/{pid}/fd")) else {
            return;
        };
        for file in files.flatten() {
            if let Ok(link) = std::fs::read_link(file.path())
                && let Some(inode) = proc_net::socket_inode(link.as_os_str().as_bytes())
            {
                each(inode);
            }
        }
    }

    pub(super) fn image_name(pid: u32) -> Option<String> {
        // The executable's own name: what a person would write in a rule.
        // `comm` is a thread's name, cut at fifteen bytes, and a browser
        // renames its threads.
        if let Ok(path) = std::fs::read_link(format!("/proc/{pid}/exe"))
            && let Some(name) = path.file_name().map(|n| n.to_string_lossy())
        {
            // An executable replaced since it started — an upgrade — reads
            // "name (deleted)".
            let name = name.strip_suffix(" (deleted)").unwrap_or(&name);
            if !name.is_empty() {
                return Some(name.to_owned());
            }
        }
        // A process whose executable cannot be read still has a name.
        let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
        Some(comm.trim_end().to_owned()).filter(|name| !name.is_empty())
    }

    #[cfg(test)]
    mod tests {
        use std::os::fd::AsRawFd;

        use super::*;

        /// A socket a child inherited is still the parent's: here one this
        /// test leaves open across `sleep`'s exec, so both hold it for real.
        #[test]
        fn a_socket_two_processes_hold_is_the_older_ones() {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a port");
            let stream = std::net::TcpStream::connect(listener.local_addr().expect("its address"))
                .expect("connects");
            let fd = stream.as_raw_fd();
            let link = std::fs::read_link(format!("/proc/self/fd/{fd}")).expect("the link");
            let inode = proc_net::socket_inode(link.as_os_str().as_bytes()).expect("a socket");
            // SAFETY: flags of a descriptor this test owns, put back at once.
            unsafe { libc::fcntl(fd, libc::F_SETFD, 0) };
            let child = std::process::Command::new("sleep").arg("30").spawn();
            unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
            let mut child = child.expect("sleep");
            let mut holds = false;
            for _ in 0..200 {
                sockets_of(child.id(), |held| holds |= held == inode);
                if holds {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            let owners = every_socket();
            let _ = child.kill();
            let _ = child.wait();
            assert!(holds, "the child holds the socket too");
            assert_eq!(owners.get(&inode), Some(&std::process::id()));
        }

        #[test]
        fn a_claim_keeps_the_lower_pid_whatever_the_order() {
            let mut owners = HashMap::new();
            claim(&mut owners, 7, 300);
            claim(&mut owners, 7, 200);
            claim(&mut owners, 7, 400);
            assert_eq!(owners.get(&7), Some(&200));
        }

        #[test]
        fn the_recent_owners_stay_few() {
            let mut hot = VecDeque::new();
            let many = HOT as u32 * 4;
            for pid in 0..many {
                remember_owner(&mut hot, pid);
            }
            assert_eq!(hot.len(), HOT);
            assert_eq!(hot.front(), Some(&(many - 1)), "newest first");
            remember_owner(&mut hot, many - 3);
            assert_eq!(hot.len(), HOT, "an owner is in the list once");
            assert_eq!(hot.front(), Some(&(many - 3)));
        }

        #[test]
        fn the_names_remembered_stay_few() {
            let mut names = None;
            for pid in 0..NAMES as u32 * 3 {
                names_now(&mut names).insert(pid, None);
            }
            let held = names.as_ref().map_or(0, |(_, known)| known.len());
            assert!(held <= NAMES, "{held} names held");
        }
    }
}

/// Reading Linux's socket tables. Plain text, so it is tested everywhere.
#[cfg(any(target_os = "linux", test))]
mod proc_net {
    use std::collections::HashMap;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

    /// One socket, as `/proc/net/tcp` and its kin list it.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Row {
        pub local: SocketAddr,
        pub remote: SocketAddr,
        pub listening: bool,
        pub inode: u64,
    }

    /// TCP_LISTEN in the `st` column.
    const LISTEN: u8 = 0x0A;

    #[cfg(target_os = "linux")]
    pub fn streams() -> Option<Vec<Row>> {
        read(&["/proc/net/tcp", "/proc/net/tcp6"])
    }

    #[cfg(target_os = "linux")]
    pub fn datagrams() -> Option<Vec<Row>> {
        read(&["/proc/net/udp", "/proc/net/udp6"])
    }

    /// Every row of the tables that can be read; none only when none can.
    #[cfg(target_os = "linux")]
    fn read(paths: &[&str]) -> Option<Vec<Row>> {
        let mut rows = Vec::new();
        let mut any = false;
        for path in paths {
            if let Ok(text) = std::fs::read_to_string(path) {
                any = true;
                rows.extend(parse(&text));
            }
        }
        any.then_some(rows)
    }

    /// The rows of one table. The header line, and any line that does not
    /// read, are skipped.
    pub fn parse(text: &str) -> impl Iterator<Item = Row> + '_ {
        text.lines().skip(1).filter_map(row)
    }

    fn row(line: &str) -> Option<Row> {
        // sl local_address rem_address st tx:rx tr:when retrnsmt uid timeout inode
        let fields: Vec<&str> = line.split_whitespace().collect();
        let state = u8::from_str_radix(fields.get(3)?, 16).ok()?;
        Some(Row {
            local: endpoint(fields.get(1)?)?,
            remote: endpoint(fields.get(2)?)?,
            listening: state == LISTEN,
            inode: fields.get(9)?.parse().ok()?,
        })
    }

    /// `0100007F:1F90` or the 32-digit IPv6 form. The address is printed as
    /// 32-bit words in the machine's own byte order, the port as a number.
    /// An IPv4 address a dual-stack socket holds (`::ffff:a.b.c.d`) is given
    /// back as IPv4, which is how the other end of the connection names it.
    fn endpoint(text: &str) -> Option<SocketAddr> {
        let (address, port) = text.split_once(':')?;
        let port = u16::from_str_radix(port, 16).ok()?;
        let word = |hex: &str| u32::from_str_radix(hex, 16).ok().map(u32::to_ne_bytes);
        let ip = match address.len() {
            8 => IpAddr::V4(Ipv4Addr::from(word(address)?)),
            32 => {
                let mut bytes = [0u8; 16];
                for (i, chunk) in bytes.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                    *chunk = word(address.get(i * 8..i * 8 + 8)?)?;
                }
                let v6 = Ipv6Addr::from(bytes);
                v6.to_ipv4_mapped().map_or(IpAddr::V6(v6), IpAddr::V4)
            }
            _ => return None,
        };
        Some(SocketAddr::new(ip, port))
    }

    /// Local port -> inode. A port held by a listener and by a connection —
    /// a connection out from a port someone else listens on elsewhere — is
    /// the connection's: the inbound asks about the far end of a connection.
    pub fn ports(rows: &[Row]) -> HashMap<u16, u64> {
        let mut ports = HashMap::new();
        for row in rows.iter().filter(|row| row.inode != 0) {
            let port = row.local.port();
            if !row.listening || !ports.contains_key(&port) {
                ports.insert(port, row.inode);
            }
        }
        ports
    }

    /// A table's sockets, as the lookups ask for them.
    pub struct Sockets {
        /// Local port -> inode, as `ports` gives it.
        pub ports: HashMap<u16, u64>,
        /// (local, remote) -> inode, for every connection.
        connections: HashMap<(SocketAddr, SocketAddr), u64>,
    }

    impl Sockets {
        pub fn new(rows: &[Row]) -> Self {
            let connections = rows
                .iter()
                .filter(|row| row.inode != 0 && !row.listening)
                .map(|row| ((row.local, row.remote), row.inode))
                .collect();
            Self {
                ports: ports(rows),
                connections,
            }
        }

        /// The socket of the connection from `from` to `to`. Linux hands a
        /// local port already in use to a new connection whenever the pair
        /// of ends differs, so only both ends are the connection's own.
        pub fn connection(&self, from: SocketAddr, to: SocketAddr) -> Option<u64> {
            self.connections
                .get(&(canonical(from), canonical(to)))
                .copied()
        }
    }

    /// An address as the tables give it: one a dual-stack socket holds for
    /// IPv4 is IPv4, as `endpoint` reads it, and a socket address's flow
    /// label and scope are in no table at all.
    pub fn canonical(address: SocketAddr) -> SocketAddr {
        let ip = match address.ip() {
            IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(IpAddr::V6(v6), IpAddr::V4),
            v4 => v4,
        };
        SocketAddr::new(ip, address.port())
    }

    /// `socket:[12345]` -> 12345.
    pub fn socket_inode(link: &[u8]) -> Option<u64> {
        let digits = link.strip_prefix(b"socket:[")?.strip_suffix(b"]")?;
        std::str::from_utf8(digits).ok()?.parse().ok()
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// Lines as the kernel writes them on a little-endian machine, with
        /// addresses from the reserved ranges.
        const TCP: &str = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 0100007F:2A38 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 41001 1 0000000000000000 100 0 0 10 0
   1: 0100007F:C350 0100007F:2A38 01 00000000:00000000 00:00000000 00000000  1000        0 41002 1 0000000000000000 20 4 30 10 -1
   2: 0100007F:2A38 0100007F:C350 01 00000000:00000000 00:00000000 00000000  1000        0 41003 1 0000000000000000 20 4 30 10 -1
   3: 0200000A:D431 0A7100CB:01BB 06 00000000:00000000 03:00000F5A 00000000     0        0 0 3 0000000000000000
garbage
";

        const TCP6: &str = "  sl  local_address                         remote_address                        st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 0000000000000000FFFF00000100007F:C351 0000000000000000FFFF00000100007F:2A38 01 00000000:00000000 00:00000000 00000000  1000        0 42001 1 0000000000000000 20 4 30 10 -1
   1: B80D0120000000000000000001000000:0050 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 42002 1 0000000000000000 100 0 0 10 0
";

        #[test]
        fn a_table_reads_as_the_kernel_writes_it() {
            let rows: Vec<Row> = parse(TCP).collect();
            assert_eq!(rows.len(), 4, "the header and the garbage are skipped");
            assert_eq!(rows[1].local, "127.0.0.1:50000".parse().unwrap());
            assert_eq!(rows[1].remote, "127.0.0.1:10808".parse().unwrap());
            assert!(rows[0].listening && !rows[1].listening);
            assert_eq!(rows[1].inode, 41002);
            assert_eq!(rows[3].local, "10.0.0.2:54321".parse().unwrap());
            assert_eq!(rows[3].remote, "203.0.113.10:443".parse().unwrap());
        }

        /// A dual-stack socket connected over IPv4 is named as IPv4, the way
        /// the far end names it; a real IPv6 address stays one.
        #[test]
        fn a_mapped_address_is_given_back_as_ipv4() {
            let rows: Vec<Row> = parse(TCP6).collect();
            assert_eq!(rows[0].local, "127.0.0.1:50001".parse().unwrap());
            assert_eq!(rows[0].remote, "127.0.0.1:10808".parse().unwrap());
            assert_eq!(rows[1].local, "[2001:db8::1]:80".parse().unwrap());
        }

        /// The listener on 10808 and the connection from 50000 are different
        /// ports; the connection *to* 10808 shares the listener's number and
        /// wins it, and a socket with no inode — TIME_WAIT — is nobody's.
        #[test]
        fn a_port_is_the_connections_not_the_listeners() {
            let ports = ports(&parse(TCP).collect::<Vec<_>>());
            assert_eq!(ports.get(&50000), Some(&41002));
            assert_eq!(ports.get(&10808), Some(&41003));
            assert_eq!(ports.get(&54321), None, "no inode, no owner");
        }

        /// One local port, two connections to different places — which
        /// Linux allows — each found by its own two ends, in either order
        /// of the rows; by the port, one of them would have the other's
        /// program.
        #[test]
        fn a_connection_is_found_by_both_its_ends() {
            let browser = "   1: 0100007F:C350 0100007F:2A38 01 00000000:00000000 00:00000000 00000000  1000        0 41002 1 0000000000000000 20 4 30 10 -1";
            let other = "   4: 0200000A:C350 0A7100CB:01BB 01 00000000:00000000 00:00000000 00000000  1000        0 43000 1 0000000000000000 20 4 30 10 -1";
            let to_bliss = (
                "127.0.0.1:50000".parse().unwrap(),
                "127.0.0.1:10808".parse().unwrap(),
            );
            for lines in [[browser, other], [other, browser]] {
                let text = format!("header\n{}\n{}\n", lines[0], lines[1]);
                let sockets = Sockets::new(&parse(&text).collect::<Vec<_>>());
                assert_eq!(sockets.connection(to_bliss.0, to_bliss.1), Some(41002));
                assert_eq!(
                    sockets.connection(
                        "10.0.0.2:50000".parse().unwrap(),
                        "203.0.113.10:443".parse().unwrap()
                    ),
                    Some(43000)
                );
                assert_eq!(
                    sockets.connection(
                        "[::ffff:127.0.0.1]:50000".parse().unwrap(),
                        "[::ffff:127.0.0.1]:10808".parse().unwrap()
                    ),
                    Some(41002),
                    "a dual-stack listener's spelling of the same ends"
                );
                assert_eq!(
                    sockets.connection(to_bliss.1, to_bliss.0),
                    None,
                    "the ends are not interchangeable"
                );
                let by_port = sockets.ports.get(&50000).copied();
                assert!(
                    matches!(by_port, Some(41002 | 43000)),
                    "the port alone is one of the two, whichever was listed last: {by_port:?}"
                );
            }
        }

        #[test]
        fn a_descriptor_names_its_socket() {
            assert_eq!(socket_inode(b"socket:[41002]"), Some(41002));
            assert_eq!(socket_inode(b"pipe:[41002]"), None);
            assert_eq!(socket_inode(b"/dev/null"), None);
            assert_eq!(socket_inode(b"socket:[]"), None);
        }
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
mod imp {
    pub fn name_for_port(_port: u16) -> Option<String> {
        None
    }
    pub fn name_for_datagram_port(_port: u16) -> Option<String> {
        None
    }
    pub fn name_for_connection(
        _from: std::net::SocketAddr,
        _to: std::net::SocketAddr,
    ) -> Option<String> {
        None
    }
    pub fn process_of(_from: std::net::SocketAddr, _to: std::net::SocketAddr) -> Option<u32> {
        None
    }
}

pub use imp::{name_for_connection, name_for_datagram_port, name_for_port, process_of};

#[cfg(all(test, target_os = "linux"))]
mod linux_tests {
    use super::*;

    /// The name of this test's own executable, as the lookup gives it.
    fn me() -> String {
        std::env::current_exe()
            .expect("the test")
            .file_name()
            .expect("a file name")
            .to_string_lossy()
            .into_owned()
    }

    /// A connection is traced to the process that opened it: this one, by
    /// its executable's name and by its pid.
    #[test]
    fn a_connection_is_traced_to_its_process() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a port");
        let to = listener.local_addr().expect("its address");
        let stream = std::net::TcpStream::connect(to).expect("connects");
        let from = stream.local_addr().expect("the caller's address");
        let _accepted = listener.accept().expect("accepts");

        assert_eq!(name_for_port(from.port()), Some(me()));
        assert_eq!(name_for_connection(from, to), Some(me()));
        assert_eq!(process_of(from, to), Some(std::process::id()));
        assert_eq!(
            process_of(to, from),
            Some(std::process::id()),
            "the other end"
        );
        assert_eq!(process_of(from, from), None, "no such connection");
    }

    #[test]
    fn a_datagram_port_is_traced_to_its_process() {
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").expect("a port");
        let port = socket.local_addr().expect("its address").port();
        assert_eq!(name_for_datagram_port(port), Some(me()));
    }

    /// Another program is named by its executable: `sleep` stands in for a
    /// browser. Asked until it is `sleep`, a moment at most: the kernel wakes
    /// the parent of a `vfork` before the child's executable is its own, and
    /// for that moment `/proc/<pid>/exe` is still the parent's — seen once,
    /// as the test binary's name, in a run of every test at once.
    #[test]
    fn another_program_is_named_by_its_executable() {
        let Ok(mut child) = std::process::Command::new("sleep").arg("30").spawn() else {
            return;
        };
        let pid = child.id();
        let mut name = imp::image_name(pid);
        for _ in 0..100 {
            if name.as_deref() == Some("sleep") {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
            name = imp::image_name(pid);
        }
        let _ = child.kill();
        let _ = child.wait();
        assert_eq!(name.as_deref(), Some("sleep"));
    }
}

#[cfg(all(test, windows))]
mod tests {
    use std::os::windows::io::AsRawHandle;
    use std::process::{Command, Stdio};

    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Security::{
        ACL, ACL_REVISION, AddAccessAllowedAce, CreateWellKnownSid, DACL_SECURITY_INFORMATION,
        InitializeAcl, InitializeSecurityDescriptor, PSECURITY_DESCRIPTOR, PSID,
        SECURITY_DESCRIPTOR, SECURITY_MAX_SID_SIZE, SetKernelObjectSecurity,
        SetSecurityDescriptorDacl, WinWorldSid,
    };
    use windows::Win32::System::Threading::{
        PROCESS_QUERY_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, PROCESS_VM_READ,
    };

    use super::*;

    /// A connection is traced to the process that opened it: this one.
    #[test]
    fn a_connection_is_traced_to_its_process() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a port");
        let to = listener.local_addr().expect("its address");
        let stream = std::net::TcpStream::connect(to).expect("connects");
        let from = stream.local_addr().expect("the caller's address");
        let _accepted = listener.accept().expect("accepts");
        assert_eq!(process_of(from, to), Some(std::process::id()));
        assert_eq!(
            process_of(to, from),
            Some(std::process::id()),
            "the other end"
        );
        assert_eq!(process_of(from, from), None, "no such connection");
    }

    /// A process whose maker wrote its access list, as OpenVPN's service does
    /// for `openvpn.exe`: anyone may query it, by the older right alone. Its
    /// name is still found.
    #[test]
    fn a_process_with_its_own_access_list_is_named() {
        let mut child = Command::new("ping")
            .args(["-n", "30", "127.0.0.1"])
            .stdout(Stdio::null())
            .spawn()
            .expect("ping starts");
        // What the service grants the user: READ_CONTROL and the rest.
        let access =
            (PROCESS_SYNCHRONIZE | PROCESS_QUERY_INFORMATION | PROCESS_VM_READ | PROCESS_TERMINATE)
                .0
                | 0x0002_0000;
        let mut sid = [0u8; SECURITY_MAX_SID_SIZE as usize];
        let mut sid_size = SECURITY_MAX_SID_SIZE;
        let mut acl = [0u64; 32];
        let mut descriptor = SECURITY_DESCRIPTOR::default();
        // SAFETY: every buffer outlives the calls that fill and read it, and
        // the handle is the child's own, held by `child` until it is killed.
        unsafe {
            let sid = PSID(sid.as_mut_ptr().cast());
            CreateWellKnownSid(WinWorldSid, None, Some(sid), &mut sid_size).expect("Everyone");
            let acl = acl.as_mut_ptr().cast::<ACL>();
            InitializeAcl(acl, size_of::<[u64; 32]>() as u32, ACL_REVISION).expect("an ACL");
            AddAccessAllowedAce(acl, ACL_REVISION, access, sid).expect("an ACE");
            let descriptor = PSECURITY_DESCRIPTOR((&raw mut descriptor).cast());
            InitializeSecurityDescriptor(descriptor, 1).expect("a descriptor");
            SetSecurityDescriptorDacl(descriptor, true, Some(acl), false).expect("its DACL");
            SetKernelObjectSecurity(
                HANDLE(child.as_raw_handle()),
                DACL_SECURITY_INFORMATION,
                descriptor,
            )
            .expect("the child's new access list");
        }
        let name = imp::image_name(child.id());
        let _ = child.kill();
        let _ = child.wait();
        assert!(
            name.as_deref()
                .is_some_and(|name| name.eq_ignore_ascii_case("ping.exe")),
            "{name:?}"
        );
    }
}
