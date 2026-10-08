use anyhow::Result;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy)]
struct Entry {
    pid: i32,
    ppid: i32,
    pgid: i32,
    identity: u64,
    zombie: bool,
}

pub struct Group {
    pgid: i32,
    known: BTreeMap<i32, u64>,
}

pub struct TerminalGuard {
    previous: i32,
}

impl TerminalGuard {
    pub fn attach(group: i32) -> Result<Option<Self>> {
        if unsafe { libc::isatty(libc::STDIN_FILENO) } == 0 {
            return Ok(None);
        }
        let previous = unsafe { libc::tcgetpgrp(libc::STDIN_FILENO) };
        if previous < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        if previous != unsafe { libc::getpgrp() } {
            return Ok(None);
        }
        set_foreground(group)?;
        let guard = Self { previous };
        // spawn と端末移譲の間に SIGTTIN で停止した対象も再開する。
        if unsafe { libc::kill(-group, libc::SIGCONT) } < 0
            && std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
        {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(Some(guard))
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = set_foreground(self.previous);
    }
}

fn set_foreground(group: i32) -> Result<()> {
    unsafe {
        let mut mask: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut mask);
        libc::sigaddset(&mut mask, libc::SIGTTOU);
        let mut previous = std::mem::zeroed();
        let code = libc::pthread_sigmask(libc::SIG_BLOCK, &mask, &mut previous);
        if code != 0 {
            return Err(std::io::Error::from_raw_os_error(code).into());
        }
        let result = libc::tcsetpgrp(libc::STDIN_FILENO, group);
        let error = std::io::Error::last_os_error();
        let reset = libc::pthread_sigmask(libc::SIG_SETMASK, &previous, std::ptr::null_mut());
        if result < 0 {
            return Err(error.into());
        }
        if reset != 0 {
            return Err(std::io::Error::from_raw_os_error(reset).into());
        }
    }
    Ok(())
}

impl Group {
    pub fn new(pid: u32) -> Result<Self> {
        let mut group = Self {
            pgid: pid as i32,
            known: BTreeMap::new(),
        };
        if let Err(error) = group.refresh() {
            let _ = group.force();
            return Err(error);
        }
        Ok(group)
    }
    pub fn refresh(&mut self) -> Result<()> {
        let entries = snapshot()?;
        let mut parents = BTreeSet::from([self.pgid]);
        // Linux の subreaper に reparent された、セッションを離れた子孫も回収する。
        #[cfg(target_os = "linux")]
        parents.insert(unsafe { libc::getpid() });
        for entry in &entries {
            if entry.pgid == self.pgid || self.known.get(&entry.pid) == Some(&entry.identity) {
                parents.insert(entry.pid);
            }
        }
        loop {
            let before = parents.len();
            for entry in &entries {
                if parents.contains(&entry.ppid) {
                    parents.insert(entry.pid);
                }
            }
            if before == parents.len() {
                break;
            }
        }
        self.known = entries
            .iter()
            .filter(|entry| {
                parents.contains(&entry.pid)
                    && entry.pid != unsafe { libc::getpid() }
                    && !entry.zombie
            })
            .map(|entry| (entry.pid, entry.identity))
            .collect();
        Ok(())
    }
    pub fn graceful(&mut self, interrupted: bool) -> Result<()> {
        self.signal(if interrupted {
            libc::SIGINT
        } else {
            libc::SIGTERM
        })
    }
    pub fn force(&mut self) -> Result<()> {
        self.signal(libc::SIGKILL)
    }
    fn signal(&mut self, signal: i32) -> Result<()> {
        let refresh = self.refresh();
        // グループ全体への signal は、列挙後に生まれた通常の子プロセスにも届く。
        if unsafe { libc::kill(-self.pgid, signal) } != 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(error.into());
            }
        }
        for entry in snapshot()? {
            if entry.pgid != self.pgid && self.known.get(&entry.pid) == Some(&entry.identity) {
                unsafe {
                    libc::kill(entry.pid, signal);
                }
            }
        }
        refresh?;
        Ok(())
    }
    pub fn alive(&mut self) -> Result<bool> {
        // subreaper の zombie だけを回収する。Tokio が管理する親の wait は先に完了している。
        #[cfg(target_os = "linux")]
        for entry in snapshot()? {
            if entry.pid != self.pgid && entry.ppid == unsafe { libc::getpid() } && entry.zombie {
                unsafe {
                    libc::waitpid(entry.pid, std::ptr::null_mut(), libc::WNOHANG);
                }
            }
        }
        self.refresh()?;
        Ok(!self.known.is_empty())
    }
}

pub fn is_pid_alive(pid: u32) -> bool {
    snapshot().is_ok_and(|entries| {
        entries
            .iter()
            .any(|entry| entry.pid == pid as i32 && !entry.zombie)
    })
}

#[cfg(target_os = "linux")]
fn snapshot() -> Result<Vec<Entry>> {
    use std::os::unix::fs::MetadataExt;
    let mut entries = Vec::new();
    for entry in std::fs::read_dir("/proc")? {
        let Ok(entry) = entry else {
            continue;
        };
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<i32>() else {
            continue;
        };
        if entry
            .metadata()
            .is_ok_and(|metadata| metadata.uid() != unsafe { libc::geteuid() })
        {
            continue;
        }
        let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
            continue;
        };
        let Some(end) = stat.rfind(')') else {
            continue;
        };
        let parts = stat[end + 1..].split_whitespace().collect::<Vec<_>>();
        if parts.len() < 20 {
            continue;
        }
        if let (Ok(ppid), Ok(pgid), Ok(identity)) =
            (parts[1].parse(), parts[2].parse(), parts[19].parse())
        {
            entries.push(Entry {
                pid,
                ppid,
                pgid,
                identity,
                zombie: matches!(parts[0], "Z" | "X"),
            });
        }
    }
    Ok(entries)
}

#[cfg(target_os = "macos")]
fn snapshot() -> Result<Vec<Entry>> {
    let estimate = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    if estimate < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let mut pids = vec![0i32; estimate as usize + 256];
    let count = unsafe {
        libc::proc_listallpids(
            pids.as_mut_ptr().cast(),
            (pids.len() * std::mem::size_of::<i32>()) as i32,
        )
    };
    if count < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let mut entries = Vec::new();
    for pid in pids.into_iter().filter(|pid| *pid > 0) {
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of_val(&info) as i32;
        if unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDTBSDINFO,
                0,
                (&mut info as *mut libc::proc_bsdinfo).cast(),
                size,
            )
        } != size
        {
            continue;
        }
        if info.pbi_uid != unsafe { libc::geteuid() } {
            continue;
        }
        entries.push(Entry {
            pid,
            ppid: info.pbi_ppid as i32,
            pgid: info.pbi_pgid as i32,
            identity: info.pbi_start_tvsec * 1_000_000 + info.pbi_start_tvusec,
            zombie: info.pbi_status == libc::SZOMB,
        });
    }
    Ok(entries)
}
