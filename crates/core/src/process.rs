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
//! the worst case is an unnamed row.

#[cfg(windows)]
mod imp {
    use std::collections::HashMap;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::NetworkManagement::IpHelper::{
        GetExtendedTcpTable, GetExtendedUdpTable, MIB_TCPTABLE_OWNER_PID, MIB_UDPTABLE_OWNER_PID,
        TCP_TABLE_OWNER_PID_ALL, UDP_TABLE_OWNER_PID,
    };
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_NAME_FORMAT, PROCESS_QUERY_LIMITED_INFORMATION,
        QueryFullProcessImageNameW,
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

    /// Local port -> owning pid, for every TCP connection on the machine.
    fn port_table() -> Option<HashMap<u16, u32>> {
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

        let mut ports = HashMap::with_capacity(rows.len());
        for row in rows {
            // dwLocalPort holds the port in network byte order, in the low word.
            let port = u16::from_be((row.dwLocalPort & 0xFFFF) as u16);
            ports.insert(port, row.dwOwningPid);
        }
        Some(ports)
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

        let mut ports = HashMap::with_capacity(rows.len());
        for row in rows {
            let port = u16::from_be((row.dwLocalPort & 0xFFFF) as u16);
            ports.insert(port, row.dwOwningPid);
        }
        Some(ports)
    }

    fn image_name(pid: u32) -> Option<String> {
        // LIMITED_INFORMATION is the least we can ask for and still get a path;
        // it also works against processes at a higher integrity level.
        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;

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

#[cfg(not(windows))]
mod imp {
    pub fn name_for_port(_port: u16) -> Option<String> {
        None
    }
    pub fn name_for_datagram_port(_port: u16) -> Option<String> {
        None
    }
}

pub use imp::{name_for_datagram_port, name_for_port};
