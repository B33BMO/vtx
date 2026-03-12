//! Gathers system stats and git info for the status bar.
//! Reads from /proc directly — no external crate dependencies.

use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Cached system/git info for the status bar.
struct CachedInfo {
    cpu_pct: f32,
    mem_used_mb: u64,
    mem_total_mb: u64,
    load_avg: String,
    last_cpu_idle: u64,
    last_cpu_total: u64,
    last_update: Instant,
}

static CACHE: Mutex<Option<CachedInfo>> = Mutex::new(None);

/// Git info for a working directory.
pub struct GitInfo {
    pub branch: String,
    pub dirty: bool,
    pub ahead: u32,
    pub behind: u32,
}

/// System stats.
pub struct SysInfo {
    pub cpu_pct: f32,
    pub mem_used_mb: u64,
    pub mem_total_mb: u64,
    pub load_avg: String,
}

/// Get git info for the given directory. Returns None if not a git repo.
pub fn git_info(cwd: &Path) -> Option<GitInfo> {
    // Get branch name
    let branch_output = std::process::Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(cwd)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;

    if !branch_output.status.success() {
        return None;
    }

    let branch = String::from_utf8_lossy(&branch_output.stdout)
        .trim()
        .to_string();

    // Check dirty status (fast — just checks index + worktree)
    let dirty = std::process::Command::new("git")
        .args(["status", "--porcelain", "--untracked-files=no"])
        .current_dir(cwd)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .map(|o| !o.stdout.is_empty())
        .unwrap_or(false);

    // Get ahead/behind counts
    let mut ahead = 0u32;
    let mut behind = 0u32;
    if let Ok(output) = std::process::Command::new("git")
        .args(["rev-list", "--left-right", "--count", "HEAD...@{upstream}"])
        .current_dir(cwd)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
    {
        if output.status.success() {
            let s = String::from_utf8_lossy(&output.stdout);
            let parts: Vec<&str> = s.trim().split('\t').collect();
            if parts.len() == 2 {
                ahead = parts[0].parse().unwrap_or(0);
                behind = parts[1].parse().unwrap_or(0);
            }
        }
    }

    Some(GitInfo { branch, dirty, ahead, behind })
}

/// Read system stats from /proc. Uses a cache to avoid hammering /proc on every frame.
pub fn sys_info() -> SysInfo {
    let mut cache = CACHE.lock().unwrap();

    let now = Instant::now();
    let needs_refresh = cache.as_ref()
        .map(|c| now.duration_since(c.last_update) > Duration::from_secs(2))
        .unwrap_or(true);

    if needs_refresh {
        let (cpu_idle, cpu_total) = read_cpu_times();
        let (mem_used, mem_total) = read_mem_info();
        let load = read_load_avg();

        let (prev_idle, prev_total) = cache.as_ref()
            .map(|c| (c.last_cpu_idle, c.last_cpu_total))
            .unwrap_or((cpu_idle, cpu_total));

        let delta_idle = cpu_idle.saturating_sub(prev_idle) as f32;
        let delta_total = cpu_total.saturating_sub(prev_total) as f32;
        let cpu_pct = if delta_total > 0.0 {
            ((1.0 - delta_idle / delta_total) * 100.0).clamp(0.0, 100.0)
        } else {
            0.0
        };

        *cache = Some(CachedInfo {
            cpu_pct,
            mem_used_mb: mem_used,
            mem_total_mb: mem_total,
            load_avg: load,
            last_cpu_idle: cpu_idle,
            last_cpu_total: cpu_total,
            last_update: now,
        });
    }

    let c = cache.as_ref().unwrap();
    SysInfo {
        cpu_pct: c.cpu_pct,
        mem_used_mb: c.mem_used_mb,
        mem_total_mb: c.mem_total_mb,
        load_avg: c.load_avg.clone(),
    }
}

/// Read cumulative CPU times from /proc/stat. Returns (idle, total).
fn read_cpu_times() -> (u64, u64) {
    let Ok(contents) = std::fs::read_to_string("/proc/stat") else {
        return (0, 0);
    };
    // First line: cpu  user nice system idle iowait irq softirq steal guest guest_nice
    let Some(line) = contents.lines().next() else {
        return (0, 0);
    };
    let vals: Vec<u64> = line
        .split_whitespace()
        .skip(1) // skip "cpu"
        .filter_map(|s| s.parse().ok())
        .collect();
    if vals.len() < 4 {
        return (0, 0);
    }
    let idle = vals[3]; // idle is 4th field
    let total: u64 = vals.iter().sum();
    (idle, total)
}

/// Read memory info from /proc/meminfo. Returns (used_mb, total_mb).
fn read_mem_info() -> (u64, u64) {
    let Ok(contents) = std::fs::read_to_string("/proc/meminfo") else {
        return (0, 0);
    };
    let mut total_kb = 0u64;
    let mut available_kb = 0u64;
    for line in contents.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            total_kb = parse_meminfo_val(rest);
        } else if let Some(rest) = line.strip_prefix("MemAvailable:") {
            available_kb = parse_meminfo_val(rest);
        }
    }
    let used_mb = (total_kb.saturating_sub(available_kb)) / 1024;
    let total_mb = total_kb / 1024;
    (used_mb, total_mb)
}

fn parse_meminfo_val(s: &str) -> u64 {
    s.trim()
        .split_whitespace()
        .next()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}

/// Read 1-minute load average from /proc/loadavg.
fn read_load_avg() -> String {
    std::fs::read_to_string("/proc/loadavg")
        .ok()
        .and_then(|s| s.split_whitespace().next().map(String::from))
        .unwrap_or_else(|| "?".into())
}

/// Get the local time as HH:MM.
pub fn local_time() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    // Read timezone offset from /etc/localtime via libc
    let offset = local_utc_offset();
    let local_secs = (secs as i64 + offset) as u64;

    let h = (local_secs / 3600) % 24;
    let m = (local_secs / 60) % 60;
    format!("{h:02}:{m:02}")
}

/// Get the local UTC offset in seconds using libc localtime_r.
fn local_utc_offset() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        let time_t = secs as libc::time_t;
        libc::localtime_r(&time_t, &mut tm);
        tm.tm_gmtoff
    }
}

/// Format memory as human-readable (e.g., "1.2G" or "512M").
pub fn format_mem(mb: u64) -> String {
    if mb >= 1024 {
        let gb = mb as f32 / 1024.0;
        if gb >= 10.0 {
            format!("{:.0}G", gb)
        } else {
            format!("{:.1}G", gb)
        }
    } else {
        format!("{}M", mb)
    }
}
