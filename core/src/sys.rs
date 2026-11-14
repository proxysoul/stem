use std::collections::HashMap;
use std::ffi::OsStr;
use std::process::Command;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[cfg(windows)]
pub fn command(program: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(program);
    std::os::windows::process::CommandExt::creation_flags(&mut command, CREATE_NO_WINDOW);
    command
}

#[cfg(not(windows))]
pub fn command(program: impl AsRef<OsStr>) -> Command {
    Command::new(program)
}

#[derive(Clone, Copy, Default)]
pub struct Usage {
    pub rss_mb: u64,
    pub cpu: f32,
    pub processes: usize,
}

pub fn sample() -> Option<Usage> {
    if cfg!(windows) {
        return None;
    }
    let out = command("ps").args(["-A", "-o", "pid=,ppid=,rss=,pcpu=,comm="]).output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    let mut stats: HashMap<u32, (u64, f32)> = HashMap::new();
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        let (Some(pid), Some(ppid), Some(rss), Some(cpu)) = (parts.next(), parts.next(), parts.next(), parts.next()) else { continue };
        if parts.next().is_some_and(|c| c == "ps" || c.ends_with("/ps")) {
            continue;
        }
        let (Ok(pid), Ok(ppid)) = (pid.parse::<u32>(), ppid.parse::<u32>()) else { continue };
        stats.insert(pid, (rss.parse().unwrap_or(0), cpu.replace(',', ".").parse().unwrap_or(0.0)));
        children.entry(ppid).or_default().push(pid);
    }
    let me = std::process::id();
    let mut stack = vec![me];
    let mut usage = Usage::default();
    let mut kb = 0u64;
    while let Some(pid) = stack.pop() {
        if let Some((rss, cpu)) = stats.get(&pid) {
            kb += rss;
            usage.cpu += cpu;
            usage.processes += 1;
        }
        if let Some(kids) = children.get(&pid) {
            stack.extend(kids.iter().copied().filter(|k| *k != pid));
        }
    }
    usage.rss_mb = kb / 1024;
    Some(usage)
}
