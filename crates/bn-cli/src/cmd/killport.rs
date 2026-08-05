//! `bn kill` — free a TCP port by signalling its listeners.

/// `bn kill <port>` — SIGTERM every PID listening on the TCP port, wait up to ~2s, then
/// SIGKILL survivors. Ports cmd_kill. Returns the process exit code.
pub(crate) fn emit_kill(port: &str) -> i32 {
    if !port.bytes().all(|b| b.is_ascii_digit()) || port.is_empty() {
        eprintln!("Port must be numeric: {port}");
        return 1;
    }
    let Ok(port_num) = port.parse::<u16>() else {
        println!("No process listening on :{port}.");
        return 0;
    };
    let mut pids = listening_pids(port_num);
    if pids.is_empty() {
        println!("No process listening on :{port}.");
        return 0;
    }
    println!("Killing {} process(es) on :{port}:", pids.len());
    for pid in &pids {
        let cmd = process_label(*pid);
        println!("  {pid:<7} {cmd}");
    }
    for pid in &pids {
        let _ = send_signal(*pid, signal_term());
    }

    // Up to 2s for graceful exit, then SIGKILL the survivors.
    for _ in 0..4 {
        std::thread::sleep(std::time::Duration::from_millis(500));
        pids.retain(|pid| process_exists(*pid));
        if pids.is_empty() {
            break;
        }
    }
    if !pids.is_empty() {
        println!(
            "  (forcing SIGKILL on {})",
            pids.iter()
                .map(i32::to_string)
                .collect::<Vec<_>>()
                .join(" ")
        );
        for pid in &pids {
            let _ = send_signal(*pid, signal_kill());
        }
    }
    println!("  ✓ Done.");
    0
}

/// PIDs listening on a TCP port.
pub(crate) fn listening_pids(port: u16) -> Vec<i32> {
    let af = netstat2::AddressFamilyFlags::IPV4 | netstat2::AddressFamilyFlags::IPV6;
    let proto = netstat2::ProtocolFlags::TCP;
    let Ok(sockets) = netstat2::get_sockets_info(af, proto) else {
        return Vec::new();
    };
    listening_pids_from_sockets(sockets, port)
}

pub(crate) fn listening_pids_from_sockets(
    sockets: Vec<netstat2::SocketInfo>,
    port: u16,
) -> Vec<i32> {
    let mut pids: Vec<i32> = sockets
        .into_iter()
        .filter(|socket| match &socket.protocol_socket_info {
            netstat2::ProtocolSocketInfo::Tcp(tcp) => {
                tcp.local_port == port && tcp.state == netstat2::TcpState::Listen
            }
            netstat2::ProtocolSocketInfo::Udp(_) => false,
        })
        .flat_map(|socket| socket.associated_pids)
        .filter_map(|pid| i32::try_from(pid).ok())
        .collect();
    pids.sort_unstable();
    pids.dedup();
    pids
}

pub(crate) fn process_label(pid: i32) -> String {
    #[cfg(target_os = "linux")]
    {
        let cmdline = format!("/proc/{pid}/cmdline");
        if let Ok(bytes) = std::fs::read(cmdline) {
            let parts: Vec<String> = bytes
                .split(|b| *b == 0)
                .filter(|part| !part.is_empty())
                .map(|part| String::from_utf8_lossy(part).to_string())
                .collect();
            if !parts.is_empty() {
                return parts.join(" ").chars().take(80).collect();
            }
        }
        let comm = format!("/proc/{pid}/comm");
        std::fs::read_to_string(comm)
            .unwrap_or_default()
            .trim()
            .chars()
            .take(80)
            .collect()
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        String::new()
    }
}

#[allow(
    unsafe_code,
    reason = "signal 0 is the portable non-Linux Unix process-existence probe"
)]
pub(crate) fn process_exists(pid: i32) -> bool {
    #[cfg(target_os = "linux")]
    {
        std::path::Path::new(&format!("/proc/{pid}")).exists()
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        // SAFETY: kill with signal 0 only checks process existence/permission.
        unsafe { libc::kill(pid, 0) == 0 }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

#[cfg(unix)]
#[allow(
    unsafe_code,
    reason = "sending a caller-selected Unix signal requires libc::kill"
)]
pub(crate) fn send_signal(pid: i32, sig: i32) -> std::io::Result<()> {
    // SAFETY: libc::kill does not dereference pointers; pid/signal are plain integers.
    let rc = unsafe { libc::kill(pid, sig) };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(not(unix))]
pub(crate) fn send_signal(_pid: i32, _sig: i32) -> std::io::Result<()> {
    Ok(())
}

pub(crate) fn signal_term() -> i32 {
    #[cfg(unix)]
    {
        libc::SIGTERM
    }
    #[cfg(not(unix))]
    {
        0
    }
}

pub(crate) fn signal_kill() -> i32 {
    #[cfg(unix)]
    {
        libc::SIGKILL
    }
    #[cfg(not(unix))]
    {
        0
    }
}
