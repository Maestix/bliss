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

#[cfg(not(windows))]
mod imp {
    pub fn name_for_port(_port: u16) -> Option<String> {
        None
    }
    pub fn name_for_datagram_port(_port: u16) -> Option<String> {
        None
    }
    pub fn process_of(_from: std::net::SocketAddr, _to: std::net::SocketAddr) -> Option<u32> {
        None
    }
}

pub use imp::{name_for_datagram_port, name_for_port, process_of};

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
