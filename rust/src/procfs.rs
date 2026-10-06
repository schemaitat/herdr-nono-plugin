//! Read-only views of the Linux process table (`/proc`), used to prove that an
//! agent and everything it started really run inside the nono sandbox. Every
//! reader tolerates processes that exit mid-scan and fields the kernel refuses
//! to show; unknown values are `None`, never guessed.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;

/// Where the real process table is mounted.
pub const PROC_ROOT: &str = "/proc";

/// TCP state code of an established connection in /proc/net/tcp.
pub const TCP_ESTABLISHED: &str = "01";

/// TCP state code of a listening socket in /proc/net/tcp.
pub const TCP_LISTEN: &str = "0A";

/// One process as the verification sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessInfo {
    pub pid: u32,
    pub ppid: Option<u32>,
    pub argv: Vec<String>,
    /// `no_new_privs`, which nono sets before it applies Landlock.
    pub no_new_privs: Option<bool>,
    /// The `Seccomp` mode from /proc/PID/status.
    pub seccomp: Option<u32>,
    /// Whether the environment carries `NONO_CAP_FILE`, the marker nono gives sandboxed processes.
    pub nono_cap_file: Option<bool>,
}

/// An `address:port` pair decoded from /proc/net/tcp.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TcpEndpoint {
    pub address: String,
    pub port: u16,
}

/// One row of /proc/net/tcp or tcp6.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TcpConnection {
    pub local: TcpEndpoint,
    pub remote: TcpEndpoint,
    pub state: String,
}

/// Whether this machine exposes a Linux-style /proc.
pub fn procfs_available(root: &Path) -> bool {
    root.join("self").join("status").exists()
}

/// Whether a process with this pid exists on the real /proc.
pub fn process_alive(pid: u32) -> bool {
    Path::new(PROC_ROOT).join(pid.to_string()).exists()
}

fn read_text(file: &Path) -> Option<String> {
    std::fs::read_to_string(file).ok()
}

/// Splits the fields that follow the command name of /proc/PID/stat. The name
/// may contain spaces and parentheses, so fields are counted after the last `)`.
/// The first returned word is field 3 (the state).
pub fn stat_fields_after_name(stat: &str) -> Vec<&str> {
    match stat.rfind(')') {
        Some(index) => stat
            .get(index + 2..)
            .map_or_else(Vec::new, |rest| rest.split(' ').collect()),
        None => Vec::new(),
    }
}

/// Parses the parent pid out of /proc/PID/stat.
pub fn parent_from_stat(stat: &str) -> Option<u32> {
    stat_fields_after_name(stat).get(1)?.parse().ok()
}

/// The numeric value of a `Name:   123` line of /proc/PID/status.
fn status_field(status: &str, name: &str) -> Option<u32> {
    status.lines().find_map(|line| {
        let rest = line.strip_prefix(name)?.strip_prefix(':')?;
        let digits: String = rest
            .trim_start()
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        digits.parse().ok()
    })
}

/// Reads one process, or `None` when it is gone.
pub fn read_process(pid: u32, root: &Path) -> Option<ProcessInfo> {
    let dir = root.join(pid.to_string());
    let stat = read_text(&dir.join("stat"))?;
    let status = read_text(&dir.join("status")).unwrap_or_default();
    let cmdline = read_text(&dir.join("cmdline"));
    let environ = read_text(&dir.join("environ"));
    let no_new_privs = status_field(&status, "NoNewPrivs");
    // cmdline is NUL-terminated, so splitting leaves one empty word at the end.
    let mut argv: Vec<String> = cmdline.map_or_else(Vec::new, |text| {
        text.split('\0').map(str::to_string).collect()
    });
    if argv.last().is_some_and(String::is_empty) {
        argv.pop();
    }
    Some(ProcessInfo {
        pid,
        ppid: parent_from_stat(&stat),
        argv,
        no_new_privs: no_new_privs.map(|value| value == 1),
        seccomp: status_field(&status, "Seccomp"),
        nono_cap_file: environ.map(|text| {
            text.split('\0')
                .any(|entry| entry.starts_with("NONO_CAP_FILE="))
        }),
    })
}

/// Lists every process id currently in /proc, ascending.
pub fn list_pids(root: &Path) -> Vec<u32> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut pids: Vec<u32> = entries
        .filter_map(|entry| entry.ok()?.file_name().to_str()?.parse().ok())
        .collect();
    pids.sort_unstable();
    pids
}

/// Every descendant of `root_pid` (not including it), parents before children.
pub fn descendants(root_pid: u32, root: &Path) -> Vec<ProcessInfo> {
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for pid in list_pids(root) {
        let ppid = read_text(&root.join(pid.to_string()).join("stat"))
            .and_then(|stat| parent_from_stat(&stat));
        if let Some(ppid) = ppid {
            children.entry(ppid).or_default().push(pid);
        }
    }
    let mut found = Vec::new();
    let mut queue: VecDeque<u32> = children.get(&root_pid).cloned().unwrap_or_default().into();
    let mut seen = HashSet::from([root_pid]);
    while let Some(pid) = queue.pop_front() {
        if !seen.insert(pid) {
            continue;
        }
        if let Some(info) = read_process(pid, root) {
            found.push(info);
            queue.extend(children.get(&pid).into_iter().flatten());
        }
    }
    found
}

/// Socket inodes a process holds open, or `None` when its fds cannot be read.
pub fn socket_inodes(pid: u32, root: &Path) -> Option<HashSet<String>> {
    let entries = std::fs::read_dir(root.join(pid.to_string()).join("fd")).ok()?;
    let mut inodes = HashSet::new();
    for entry in entries.flatten() {
        // The fd may close between the listing and the readlink.
        if let Ok(target) = std::fs::read_link(entry.path()) {
            let target = target.to_string_lossy();
            if let Some(inode) = target
                .strip_prefix("socket:[")
                .and_then(|rest| rest.strip_suffix(']'))
            {
                if !inode.is_empty() && inode.bytes().all(|byte| byte.is_ascii_digit()) {
                    inodes.insert(inode.to_string());
                }
            }
        }
    }
    Some(inodes)
}

/// Decodes the hex `address:port` of /proc/net/tcp{,6}; `None` for a malformed value.
pub fn decode_tcp_address(hex: &str) -> Option<TcpEndpoint> {
    let (address_hex, port_hex) = hex.split_once(':')?;
    let port = u16::from_str_radix(port_hex, 16).ok()?;
    if !address_hex.is_ascii() {
        return None;
    }
    let bytes_of = |text: &str| -> Option<Vec<u8>> {
        (0..text.len())
            .step_by(2)
            .map(|index| u8::from_str_radix(text.get(index..index + 2)?, 16).ok())
            .collect()
    };
    if address_hex.len() == 8 {
        let mut bytes = bytes_of(address_hex)?;
        bytes.reverse();
        let address = bytes
            .iter()
            .map(u8::to_string)
            .collect::<Vec<_>>()
            .join(".");
        return Some(TcpEndpoint { address, port });
    }
    if address_hex.len() != 32 {
        return None;
    }
    // IPv6: four little-endian 32-bit words.
    let mut network_order = Vec::with_capacity(16);
    for word in 0..4 {
        let mut bytes = bytes_of(&address_hex[word * 8..word * 8 + 8])?;
        bytes.reverse();
        network_order.extend(bytes);
    }
    let groups: Vec<u16> = network_order
        .chunks(2)
        .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
        .collect();
    if groups[..5].iter().all(|group| *group == 0) && groups[5] == 0xffff {
        let [a, b] = groups[6].to_be_bytes();
        let [c, d] = groups[7].to_be_bytes();
        return Some(TcpEndpoint {
            address: format!("{a}.{b}.{c}.{d}"),
            port,
        });
    }
    if groups[..7].iter().all(|group| *group == 0) && groups[7] == 1 {
        return Some(TcpEndpoint {
            address: "::1".into(),
            port,
        });
    }
    let address = groups
        .iter()
        .map(|group| format!("{group:x}"))
        .collect::<Vec<_>>()
        .join(":");
    Some(TcpEndpoint { address, port })
}

/// TCP connections by socket inode, from /proc/net/tcp and tcp6 (the network
/// namespace of the reading process, which nono shares with the host).
pub fn tcp_connections(root: &Path) -> HashMap<String, TcpConnection> {
    let mut by_inode = HashMap::new();
    for file in ["tcp", "tcp6"] {
        let Some(text) = read_text(&root.join("net").join(file)) else {
            continue;
        };
        for line in text.split('\n').skip(1) {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() < 10 {
                continue;
            }
            if let (Some(local), Some(remote)) =
                (decode_tcp_address(parts[1]), decode_tcp_address(parts[2]))
            {
                by_inode.insert(
                    parts[9].to_string(),
                    TcpConnection {
                        local,
                        remote,
                        state: parts[3].to_string(),
                    },
                );
            }
        }
    }
    by_inode
}

/// A fake /proc tree for tests, the port of `writeFakeProc` in test/helpers.mjs.
#[cfg(test)]
pub mod fake {
    use std::path::Path;

    #[derive(Default)]
    pub struct FakeProcess {
        pub pid: u32,
        pub ppid: u32,
        pub argv: Vec<&'static str>,
        pub no_new_privs: Option<bool>,
        pub cap_file: Option<bool>,
        pub environ: Option<bool>,
        pub sockets: Vec<&'static str>,
    }

    pub const TCP_HEADER: &str = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode";

    pub fn write_fake_proc(root: &Path, processes: &[FakeProcess], tcp: &[&str]) {
        std::fs::create_dir_all(root.join("self")).unwrap();
        std::fs::write(root.join("self/status"), "Name:\tnode\n").unwrap();
        for process in processes {
            let dir = root.join(process.pid.to_string());
            std::fs::create_dir_all(dir.join("fd")).unwrap();
            let name = Path::new(process.argv.first().copied().unwrap_or("x"))
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned();
            std::fs::write(
                dir.join("stat"),
                format!(
                    "{} ({name}) S {} 1 1 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 12345 0 0\n",
                    process.pid, process.ppid
                ),
            )
            .unwrap();
            let no_new_privs = if process.no_new_privs == Some(false) {
                0
            } else {
                1
            };
            std::fs::write(
                dir.join("status"),
                format!(
                    "Name:\t{name}\nPPid:\t{}\nNoNewPrivs:\t{no_new_privs}\nSeccomp:\t2\n",
                    process.ppid
                ),
            )
            .unwrap();
            std::fs::write(
                dir.join("cmdline"),
                format!("{}\0", process.argv.join("\0")),
            )
            .unwrap();
            if process.environ != Some(false) {
                let cap = if process.cap_file == Some(false) {
                    ""
                } else {
                    "NONO_CAP_FILE=/tmp/.nono-x.json\0"
                };
                std::fs::write(dir.join("environ"), format!("PATH=/usr/bin\0{cap}")).unwrap();
            }
            for (index, inode) in process.sockets.iter().enumerate() {
                std::os::unix::fs::symlink(
                    format!("socket:[{inode}]"),
                    dir.join("fd").join((index + 3).to_string()),
                )
                .unwrap();
            }
        }
        std::fs::create_dir_all(root.join("net")).unwrap();
        let mut lines = vec![TCP_HEADER];
        lines.extend_from_slice(tcp);
        std::fs::write(root.join("net/tcp"), format!("{}\n", lines.join("\n"))).unwrap();
        std::fs::write(root.join("net/tcp6"), format!("{TCP_HEADER}\n")).unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::fake::*;
    use super::*;

    fn endpoint(address: &str, port: u16) -> Option<TcpEndpoint> {
        Some(TcpEndpoint {
            address: address.into(),
            port,
        })
    }

    fn fake_tree() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        write_fake_proc(
            dir.path(),
            &[
                FakeProcess { pid: 100, ppid: 1, argv: vec!["/usr/bin/bash"], ..Default::default() },
                FakeProcess { pid: 110, ppid: 100, argv: vec!["/usr/bin/nono", "run"], ..Default::default() },
                FakeProcess { pid: 111, ppid: 110, argv: vec!["/usr/bin/node", "agent.js", "--flag"], sockets: vec!["8888", "9999"], ..Default::default() },
                FakeProcess { pid: 112, ppid: 111, argv: vec!["/bin/sh"], no_new_privs: Some(false), cap_file: Some(false), ..Default::default() },
                FakeProcess { pid: 200, ppid: 1, argv: vec!["/usr/bin/other"], environ: Some(false), ..Default::default() },
            ],
            &["   0: 0100007F:1000 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 8888 1 0000000000000000 100 0 0 10 0"],
        );
        dir
    }

    #[test]
    fn parent_from_stat_copes_with_parentheses_and_spaces_in_the_command_name() {
        assert_eq!(parent_from_stat("42 (my (odd) name) S 7 42 42 0"), Some(7));
        assert_eq!(parent_from_stat("garbage"), None);
        assert_eq!(parent_from_stat("1 (x)"), None);
    }

    #[test]
    fn read_process_and_descendants_walk_the_fake_tree() {
        let dir = fake_tree();
        let info = read_process(111, dir.path()).unwrap();
        assert_eq!(info.ppid, Some(110));
        assert_eq!(info.argv, ["/usr/bin/node", "agent.js", "--flag"]);
        assert_eq!(
            (info.no_new_privs, info.seccomp, info.nono_cap_file),
            (Some(true), Some(2), Some(true))
        );
        let shell = read_process(112, dir.path()).unwrap();
        assert_eq!(
            (shell.no_new_privs, shell.nono_cap_file),
            (Some(false), Some(false))
        );
        assert_eq!(
            read_process(200, dir.path()).unwrap().nono_cap_file,
            None,
            "an unreadable environ is unknown, not false"
        );
        assert_eq!(read_process(999, dir.path()), None);
        let pids: Vec<u32> = descendants(110, dir.path())
            .iter()
            .map(|item| item.pid)
            .collect();
        assert_eq!(pids, [111, 112]);
        assert!(descendants(112, dir.path()).is_empty());
        assert_eq!(list_pids(dir.path()), [100, 110, 111, 112, 200]);
        assert!(procfs_available(dir.path()));
        assert!(!procfs_available(&dir.path().join("nope")));
    }

    #[test]
    fn socket_inodes_lists_the_sockets_a_process_holds() {
        let dir = fake_tree();
        let inodes = socket_inodes(111, dir.path()).unwrap();
        assert_eq!(
            inodes,
            HashSet::from(["8888".to_string(), "9999".to_string()])
        );
        assert_eq!(socket_inodes(100, dir.path()).unwrap(), HashSet::new());
        assert_eq!(socket_inodes(999, dir.path()), None);
    }

    #[test]
    fn decode_tcp_address_reads_ipv4_and_ipv4_mapped_ipv6_addresses() {
        assert_eq!(
            decode_tcp_address("0100007F:1000"),
            endpoint("127.0.0.1", 4096)
        );
        assert_eq!(
            decode_tcp_address("0000000000000000FFFF00000100007F:1000"),
            endpoint("127.0.0.1", 4096)
        );
        assert_eq!(
            decode_tcp_address("00000000000000000000000001000000:0050"),
            endpoint("::1", 80)
        );
        assert_eq!(
            decode_tcp_address("B80D0120000000000000000001000000:01BB"),
            endpoint("2001:db8:0:0:0:0:0:1", 443)
        );
        assert_eq!(decode_tcp_address("zz:1"), None);
        assert_eq!(decode_tcp_address("0100007F"), None);
        assert_eq!(decode_tcp_address("0100:1000"), None);
    }

    #[test]
    fn tcp_connections_are_indexed_by_inode() {
        let dir = fake_tree();
        let connections = tcp_connections(dir.path());
        let listener = &connections["8888"];
        assert_eq!(listener.state, TCP_LISTEN);
        assert_eq!(
            listener.local,
            TcpEndpoint {
                address: "127.0.0.1".into(),
                port: 4096
            }
        );
        assert_eq!(connections.len(), 1);
        assert!(tcp_connections(&dir.path().join("missing")).is_empty());
    }

    #[test]
    fn this_process_is_alive_and_a_far_pid_is_not() {
        assert!(process_alive(std::process::id()));
        assert!(!process_alive(2_147_483_647));
    }
}
