use std::collections::HashMap;
use vtx_core::cell::{Attr, Cell, Color};

// ---------------------------------------------------------------------------
// Widget trait
// ---------------------------------------------------------------------------

pub trait Widget: Send {
    /// Refresh internal state (re-read /proc, etc.).
    fn update(&mut self);
    /// Render into a 2-D cell grid of size `cols x rows`.
    fn render(&self, cols: u16, rows: u16) -> Vec<Vec<Cell>>;
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn color_for_pct(pct: f64) -> Color {
    if pct < 50.0 {
        Color::Rgb(0, 200, 0) // green
    } else if pct < 80.0 {
        Color::Rgb(200, 200, 0) // yellow
    } else {
        Color::Rgb(200, 0, 0) // red
    }
}

/// Build a progress-bar string like `[████████░░░░] 67%`.
fn progress_bar(pct: f64, width: usize) -> String {
    let inner = width.saturating_sub(2); // subtract brackets
    let filled = ((pct / 100.0) * inner as f64).round() as usize;
    let empty = inner.saturating_sub(filled);
    format!(
        "[{}{}] {:>3.0}%",
        "█".repeat(filled),
        "░".repeat(empty),
        pct
    )
}

/// Render a single line of text as a row of `Cell`s, with given fg color.
fn text_to_cells(text: &str, cols: u16, fg: Color, attr: Attr) -> Vec<Cell> {
    let mut row: Vec<Cell> = Vec::with_capacity(cols as usize);
    for ch in text.chars().take(cols as usize) {
        row.push(Cell {
            c: ch,
            fg,
            bg: Color::Default,
            attr,
        });
    }
    // Pad to full width
    while row.len() < cols as usize {
        row.push(Cell::default());
    }
    row
}

/// Convenience: plain white text row.
fn plain_row(text: &str, cols: u16) -> Vec<Cell> {
    text_to_cells(text, cols, Color::Rgb(220, 220, 220), Attr::empty())
}

/// Bold header row.
fn header_row(text: &str, cols: u16) -> Vec<Cell> {
    text_to_cells(text, cols, Color::Rgb(100, 180, 255), Attr::BOLD)
}

/// Colored progress-bar row (label + bar).
fn bar_row(label: &str, pct: f64, cols: u16) -> Vec<Cell> {
    let bar_width = (cols as usize).saturating_sub(label.len() + 7); // 7 = " ] xx%"
    let bar = progress_bar(pct, bar_width);
    let line = format!("{label}{bar}");
    text_to_cells(&line, cols, color_for_pct(pct), Attr::empty())
}

fn empty_row(cols: u16) -> Vec<Cell> {
    vec![Cell::default(); cols as usize]
}

// ---------------------------------------------------------------------------
// CpuWidget
// ---------------------------------------------------------------------------

pub struct CpuWidget {
    /// Previous /proc/stat values per core: (user+nice, total).
    prev: Vec<(u64, u64)>,
    /// Usage percentages per core.
    usage: Vec<f64>,
}

impl CpuWidget {
    pub fn new() -> Self {
        let mut w = CpuWidget {
            prev: Vec::new(),
            usage: Vec::new(),
        };
        // Initial read to seed prev values
        w.prev = Self::read_stat();
        w.usage = vec![0.0; w.prev.len()];
        w
    }

    fn read_stat() -> Vec<(u64, u64)> {
        let content = match std::fs::read_to_string("/proc/stat") {
            Ok(c) => c,
            Err(_) => return Vec::new(),
        };
        let mut cores = Vec::new();
        for line in content.lines() {
            if line.starts_with("cpu") && !line.starts_with("cpu ") {
                let parts: Vec<u64> = line
                    .split_whitespace()
                    .skip(1)
                    .filter_map(|s| s.parse().ok())
                    .collect();
                if parts.len() >= 4 {
                    let user_nice = parts[0] + parts[1];
                    let total: u64 = parts.iter().sum();
                    cores.push((user_nice, total));
                }
            }
        }
        cores
    }
}

impl Widget for CpuWidget {
    fn update(&mut self) {
        let cur = Self::read_stat();
        let mut usage = Vec::with_capacity(cur.len());
        for (i, &(busy, total)) in cur.iter().enumerate() {
            if i < self.prev.len() {
                let d_busy = busy.saturating_sub(self.prev[i].0) as f64;
                let d_total = total.saturating_sub(self.prev[i].1) as f64;
                if d_total > 0.0 {
                    usage.push((d_busy / d_total) * 100.0);
                } else {
                    usage.push(0.0);
                }
            } else {
                usage.push(0.0);
            }
        }
        self.prev = cur;
        self.usage = usage;
    }

    fn render(&self, cols: u16, rows: u16) -> Vec<Vec<Cell>> {
        let mut grid: Vec<Vec<Cell>> = Vec::new();
        grid.push(header_row("── CPU Usage ──", cols));
        grid.push(empty_row(cols));

        for (i, &pct) in self.usage.iter().enumerate() {
            let label = format!("CPU {:<3} ", i);
            grid.push(bar_row(&label, pct, cols));
        }

        // Overall average
        if !self.usage.is_empty() {
            let avg: f64 = self.usage.iter().sum::<f64>() / self.usage.len() as f64;
            grid.push(empty_row(cols));
            let label = "Avg     ";
            grid.push(bar_row(label, avg, cols));
        }

        // Pad or truncate to rows
        while grid.len() < rows as usize {
            grid.push(empty_row(cols));
        }
        grid.truncate(rows as usize);
        grid
    }
}

// ---------------------------------------------------------------------------
// MemWidget
// ---------------------------------------------------------------------------

pub struct MemWidget {
    total_kb: u64,
    available_kb: u64,
    buffers_kb: u64,
    cached_kb: u64,
    swap_total_kb: u64,
    swap_free_kb: u64,
}

impl MemWidget {
    pub fn new() -> Self {
        let mut w = MemWidget {
            total_kb: 0,
            available_kb: 0,
            buffers_kb: 0,
            cached_kb: 0,
            swap_total_kb: 0,
            swap_free_kb: 0,
        };
        w.update();
        w
    }

    fn parse_meminfo() -> HashMap<String, u64> {
        let content = std::fs::read_to_string("/proc/meminfo").unwrap_or_default();
        let mut map = HashMap::new();
        for line in content.lines() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 2 {
                let key = parts[0].trim_end_matches(':').to_string();
                if let Ok(val) = parts[1].parse::<u64>() {
                    map.insert(key, val);
                }
            }
        }
        map
    }
}

impl Widget for MemWidget {
    fn update(&mut self) {
        let m = Self::parse_meminfo();
        self.total_kb = *m.get("MemTotal").unwrap_or(&0);
        self.available_kb = *m.get("MemAvailable").unwrap_or(&0);
        self.buffers_kb = *m.get("Buffers").unwrap_or(&0);
        self.cached_kb = *m.get("Cached").unwrap_or(&0);
        self.swap_total_kb = *m.get("SwapTotal").unwrap_or(&0);
        self.swap_free_kb = *m.get("SwapFree").unwrap_or(&0);
    }

    fn render(&self, cols: u16, rows: u16) -> Vec<Vec<Cell>> {
        let mut grid: Vec<Vec<Cell>> = Vec::new();
        grid.push(header_row("── Memory Usage ──", cols));
        grid.push(empty_row(cols));

        let used_kb = self.total_kb.saturating_sub(self.available_kb);
        let pct = if self.total_kb > 0 {
            (used_kb as f64 / self.total_kb as f64) * 100.0
        } else {
            0.0
        };

        let total_mb = self.total_kb as f64 / 1024.0;
        let used_mb = used_kb as f64 / 1024.0;
        grid.push(plain_row(
            &format!("Total:     {:.0} MB", total_mb),
            cols,
        ));
        grid.push(plain_row(
            &format!("Used:      {:.0} MB", used_mb),
            cols,
        ));
        grid.push(plain_row(
            &format!("Buffers:   {:.0} MB", self.buffers_kb as f64 / 1024.0),
            cols,
        ));
        grid.push(plain_row(
            &format!("Cached:    {:.0} MB", self.cached_kb as f64 / 1024.0),
            cols,
        ));
        grid.push(empty_row(cols));
        grid.push(bar_row("RAM   ", pct, cols));

        // Swap
        if self.swap_total_kb > 0 {
            let swap_used = self.swap_total_kb.saturating_sub(self.swap_free_kb);
            let swap_pct = (swap_used as f64 / self.swap_total_kb as f64) * 100.0;
            grid.push(empty_row(cols));
            grid.push(plain_row(
                &format!(
                    "Swap:      {:.0} / {:.0} MB",
                    swap_used as f64 / 1024.0,
                    self.swap_total_kb as f64 / 1024.0
                ),
                cols,
            ));
            grid.push(bar_row("Swap  ", swap_pct, cols));
        }

        while grid.len() < rows as usize {
            grid.push(empty_row(cols));
        }
        grid.truncate(rows as usize);
        grid
    }
}

// ---------------------------------------------------------------------------
// DiskWidget
// ---------------------------------------------------------------------------

pub struct DiskWidget {
    /// (mount, total_bytes, used_bytes)
    mounts: Vec<(String, u64, u64)>,
}

impl DiskWidget {
    pub fn new() -> Self {
        let mut w = DiskWidget { mounts: Vec::new() };
        w.update();
        w
    }

    fn read_mounts() -> Vec<(String, u64, u64)> {
        // Parse /proc/mounts then stat each mount point with libc::statvfs
        let content = std::fs::read_to_string("/proc/mounts").unwrap_or_default();
        let mut results = Vec::new();

        for line in content.lines() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() < 3 {
                continue;
            }
            let mount = parts[1];
            let fstype = parts[2];

            // Skip virtual filesystems
            if matches!(
                fstype,
                "proc" | "sysfs" | "devtmpfs" | "devpts" | "tmpfs"
                    | "cgroup" | "cgroup2" | "securityfs" | "pstore"
                    | "debugfs" | "tracefs" | "hugetlbfs" | "mqueue"
                    | "fusectl" | "configfs" | "binfmt_misc" | "autofs"
                    | "overlay" | "nsfs" | "bpf"
            ) {
                continue;
            }

            // Use statvfs via libc
            let c_path = std::ffi::CString::new(mount).unwrap_or_default();
            let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
            let ret = unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) };
            if ret == 0 && stat.f_blocks > 0 {
                let block_size = stat.f_frsize as u64;
                let total = stat.f_blocks as u64 * block_size;
                let free = stat.f_bfree as u64 * block_size;
                let used = total.saturating_sub(free);
                results.push((mount.to_string(), total, used));
            }
        }
        results
    }
}

impl Widget for DiskWidget {
    fn update(&mut self) {
        self.mounts = Self::read_mounts();
    }

    fn render(&self, cols: u16, rows: u16) -> Vec<Vec<Cell>> {
        let mut grid: Vec<Vec<Cell>> = Vec::new();
        grid.push(header_row("── Disk Usage ──", cols));
        grid.push(empty_row(cols));

        for (mount, total, used) in &self.mounts {
            let pct = if *total > 0 {
                (*used as f64 / *total as f64) * 100.0
            } else {
                0.0
            };
            let total_gb = *total as f64 / 1_073_741_824.0;
            let used_gb = *used as f64 / 1_073_741_824.0;
            let info = format!("{mount}: {used_gb:.1}/{total_gb:.1} GB");
            grid.push(plain_row(&info, cols));
            grid.push(bar_row("      ", pct, cols));
            grid.push(empty_row(cols));
        }

        if self.mounts.is_empty() {
            grid.push(plain_row("No block devices found.", cols));
        }

        while grid.len() < rows as usize {
            grid.push(empty_row(cols));
        }
        grid.truncate(rows as usize);
        grid
    }
}

// ---------------------------------------------------------------------------
// NetworkWidget
// ---------------------------------------------------------------------------

pub struct NetworkWidget {
    /// Previous snapshot: iface -> (rx_bytes, tx_bytes).
    prev: HashMap<String, (u64, u64)>,
    /// Throughput in bytes/sec: iface -> (rx, tx).
    rates: Vec<(String, f64, f64)>,
    last_update: std::time::Instant,
}

impl NetworkWidget {
    pub fn new() -> Self {
        let prev = Self::read_dev();
        NetworkWidget {
            prev,
            rates: Vec::new(),
            last_update: std::time::Instant::now(),
        }
    }

    fn read_dev() -> HashMap<String, (u64, u64)> {
        let content = std::fs::read_to_string("/proc/net/dev").unwrap_or_default();
        let mut map = HashMap::new();
        for line in content.lines().skip(2) {
            let line = line.trim();
            if let Some(colon) = line.find(':') {
                let iface = line[..colon].trim().to_string();
                let rest: Vec<u64> = line[colon + 1..]
                    .split_whitespace()
                    .filter_map(|s| s.parse().ok())
                    .collect();
                if rest.len() >= 9 {
                    // rest[0] = rx_bytes, rest[8] = tx_bytes
                    map.insert(iface, (rest[0], rest[8]));
                }
            }
        }
        map
    }

    fn format_rate(bytes_per_sec: f64) -> String {
        if bytes_per_sec >= 1_000_000.0 {
            format!("{:.1} MB/s", bytes_per_sec / 1_000_000.0)
        } else if bytes_per_sec >= 1_000.0 {
            format!("{:.1} KB/s", bytes_per_sec / 1_000.0)
        } else {
            format!("{:.0} B/s", bytes_per_sec)
        }
    }
}

impl Widget for NetworkWidget {
    fn update(&mut self) {
        let cur = Self::read_dev();
        let elapsed = self.last_update.elapsed().as_secs_f64();
        let dt = if elapsed > 0.0 { elapsed } else { 1.0 };

        let mut rates = Vec::new();
        for (iface, &(rx, tx)) in &cur {
            if let Some(&(prev_rx, prev_tx)) = self.prev.get(iface) {
                let rx_rate = rx.saturating_sub(prev_rx) as f64 / dt;
                let tx_rate = tx.saturating_sub(prev_tx) as f64 / dt;
                rates.push((iface.clone(), rx_rate, tx_rate));
            } else {
                rates.push((iface.clone(), 0.0, 0.0));
            }
        }
        rates.sort_by(|a, b| a.0.cmp(&b.0));
        self.prev = cur;
        self.rates = rates;
        self.last_update = std::time::Instant::now();
    }

    fn render(&self, cols: u16, rows: u16) -> Vec<Vec<Cell>> {
        let mut grid: Vec<Vec<Cell>> = Vec::new();
        grid.push(header_row("── Network Throughput ──", cols));
        grid.push(empty_row(cols));

        for (iface, rx, tx) in &self.rates {
            if iface == "lo" {
                continue; // skip loopback
            }
            grid.push(text_to_cells(
                &format!("  {iface}"),
                cols,
                Color::Rgb(180, 220, 255),
                Attr::BOLD,
            ));
            grid.push(text_to_cells(
                &format!("    RX: {}", Self::format_rate(*rx)),
                cols,
                Color::Rgb(0, 200, 0),
                Attr::empty(),
            ));
            grid.push(text_to_cells(
                &format!("    TX: {}", Self::format_rate(*tx)),
                cols,
                Color::Rgb(200, 200, 0),
                Attr::empty(),
            ));
            grid.push(empty_row(cols));
        }

        if self.rates.is_empty() || (self.rates.len() == 1 && self.rates[0].0 == "lo") {
            grid.push(plain_row("No network interfaces found.", cols));
        }

        while grid.len() < rows as usize {
            grid.push(empty_row(cols));
        }
        grid.truncate(rows as usize);
        grid
    }
}

// ---------------------------------------------------------------------------
// SysInfoWidget — combines all widgets
// ---------------------------------------------------------------------------

pub struct SysInfoWidget {
    cpu: CpuWidget,
    mem: MemWidget,
    disk: DiskWidget,
    net: NetworkWidget,
}

impl SysInfoWidget {
    pub fn new() -> Self {
        SysInfoWidget {
            cpu: CpuWidget::new(),
            mem: MemWidget::new(),
            disk: DiskWidget::new(),
            net: NetworkWidget::new(),
        }
    }
}

impl Widget for SysInfoWidget {
    fn update(&mut self) {
        self.cpu.update();
        self.mem.update();
        self.disk.update();
        self.net.update();
    }

    fn render(&self, cols: u16, rows: u16) -> Vec<Vec<Cell>> {
        // Render each sub-widget with a portion of the rows
        let cpu_rows = (self.cpu.usage.len() as u16 + 4).min(rows / 4);
        let mem_rows = 12.min(rows / 4);
        let disk_rows = ((self.disk.mounts.len() as u16) * 3 + 3).min(rows / 4);
        let net_rows = rows
            .saturating_sub(cpu_rows)
            .saturating_sub(mem_rows)
            .saturating_sub(disk_rows);

        let mut grid: Vec<Vec<Cell>> = Vec::new();

        grid.push(header_row(
            "═══════ VTX System Monitor ═══════",
            cols,
        ));
        grid.push(empty_row(cols));

        let cpu_render = self.cpu.render(cols, cpu_rows);
        grid.extend(cpu_render);

        let mem_render = self.mem.render(cols, mem_rows);
        grid.extend(mem_render);

        let disk_render = self.disk.render(cols, disk_rows);
        grid.extend(disk_render);

        let net_render = self.net.render(cols, net_rows);
        grid.extend(net_render);

        while grid.len() < rows as usize {
            grid.push(empty_row(cols));
        }
        grid.truncate(rows as usize);
        grid
    }
}

// ---------------------------------------------------------------------------
// Factory
// ---------------------------------------------------------------------------

/// Create a widget by name. Returns None for unknown kinds.
pub fn create_widget(kind: &str) -> Option<Box<dyn Widget>> {
    match kind {
        "cpu" => Some(Box::new(CpuWidget::new())),
        "mem" => Some(Box::new(MemWidget::new())),
        "disk" => Some(Box::new(DiskWidget::new())),
        "net" => Some(Box::new(NetworkWidget::new())),
        "sysinfo" => Some(Box::new(SysInfoWidget::new())),
        _ => None,
    }
}
