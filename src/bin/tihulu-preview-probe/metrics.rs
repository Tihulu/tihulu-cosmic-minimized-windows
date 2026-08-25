// SPDX-License-Identifier: AGPL-3.0-only

use std::{
    collections::VecDeque,
    fs::{self, File},
    io::{BufWriter, Write},
    path::Path,
};

const GROWTH_WINDOW: usize = 6;
const GROWTH_THRESHOLD: usize = 4;

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ProcessMetrics {
    pub(crate) fd_count: usize,
    pub(crate) rss_kb: u64,
    pub(crate) shmem_kb: u64,
    pub(crate) memfd_count: usize,
    pub(crate) capture_memfd_count: usize,
}

pub(crate) fn process_metrics(pid: u32) -> ProcessMetrics {
    let mut metrics = ProcessMetrics::default();
    if let Ok(entries) = fs::read_dir(format!("/proc/{pid}/fd")) {
        for entry in entries.flatten() {
            metrics.fd_count += 1;
            if let Ok(target) = fs::read_link(entry.path()) {
                let target = target.to_string_lossy().to_lowercase();
                if target.contains("memfd:") {
                    metrics.memfd_count += 1;
                    if target.contains("capture")
                        || target.contains("screencopy")
                        || target.contains("minimize-applet")
                    {
                        metrics.capture_memfd_count += 1;
                    }
                }
            }
        }
    }

    if let Ok(status) = fs::read_to_string(format!("/proc/{pid}/status")) {
        metrics.rss_kb = status_value_kb(&status, "VmRSS:").unwrap_or(0);
        metrics.shmem_kb = status_value_kb(&status, "RssShmem:").unwrap_or(0);
    }
    metrics
}

pub(crate) fn find_cosmic_comp() -> Option<u32> {
    let self_status = fs::read_to_string("/proc/self/status").ok()?;
    let self_uid = status_uid(&self_status)?;

    fs::read_dir("/proc")
        .ok()?
        .flatten()
        .filter_map(|entry| entry.file_name().to_string_lossy().parse::<u32>().ok())
        .find(|pid| {
            let comm = fs::read_to_string(format!("/proc/{pid}/comm")).ok();
            let status = fs::read_to_string(format!("/proc/{pid}/status")).ok();
            comm.as_deref().is_some_and(|name| name.trim() == "cosmic-comp")
                && status
                    .as_deref()
                    .and_then(status_uid)
                    .is_some_and(|uid| uid == self_uid)
        })
}

fn status_value_kb(status: &str, key: &str) -> Option<u64> {
    status.lines().find_map(|line| {
        let rest = line.strip_prefix(key)?.trim();
        rest.split_whitespace().next()?.parse().ok()
    })
}

fn status_uid(status: &str) -> Option<u32> {
    status.lines().find_map(|line| {
        let rest = line.strip_prefix("Uid:")?.trim();
        rest.split_whitespace().next()?.parse().ok()
    })
}

#[derive(Default)]
pub(crate) struct GrowthWatch {
    probe_fd: VecDeque<usize>,
    comp_fd: VecDeque<usize>,
    comp_capture_memfd: VecDeque<usize>,
}

impl GrowthWatch {
    pub(crate) fn push(
        &mut self,
        probe: ProcessMetrics,
        compositor: Option<ProcessMetrics>,
    ) -> Option<&'static str> {
        push_window(&mut self.probe_fd, probe.fd_count);
        if monotonic_growth(&self.probe_fd) {
            return Some("probe FD count is growing monotonically");
        }

        if let Some(compositor) = compositor {
            push_window(&mut self.comp_fd, compositor.fd_count);
            push_window(
                &mut self.comp_capture_memfd,
                compositor.capture_memfd_count,
            );
            if monotonic_growth(&self.comp_capture_memfd) {
                return Some("cosmic-comp capture-related memfds are growing monotonically");
            }
            if monotonic_growth(&self.comp_fd) {
                return Some("cosmic-comp FD count is growing monotonically");
            }
        }
        None
    }
}

fn push_window(window: &mut VecDeque<usize>, value: usize) {
    window.push_back(value);
    while window.len() > GROWTH_WINDOW {
        window.pop_front();
    }
}

fn monotonic_growth(window: &VecDeque<usize>) -> bool {
    if window.len() < GROWTH_WINDOW {
        return false;
    }
    let (Some(first), Some(last)) = (window.front(), window.back()) else {
        return false;
    };
    last.saturating_sub(*first) >= GROWTH_THRESHOLD
        && window
            .iter()
            .zip(window.iter().skip(1))
            .all(|(left, right)| right >= left)
}

pub(crate) struct SampleWriter {
    writer: BufWriter<File>,
}

impl SampleWriter {
    pub(crate) fn create(path: &Path) -> std::io::Result<Self> {
        let mut writer = BufWriter::new(File::create(path)?);
        writeln!(
            writer,
            "phase,iteration,capture_ok,probe_fd,probe_rss_kb,probe_shmem_kb,probe_memfd,cosmic_comp_pid,cosmic_comp_fd,cosmic_comp_rss_kb,cosmic_comp_shmem_kb,cosmic_comp_memfd,cosmic_comp_capture_memfd,probe_pid"
        )?;
        writer.flush()?;
        Ok(Self { writer })
    }

    pub(crate) fn record(
        &mut self,
        phase: &str,
        iteration: usize,
        capture_ok: Option<bool>,
        probe: ProcessMetrics,
        comp_pid: Option<u32>,
        comp: Option<ProcessMetrics>,
    ) -> std::io::Result<()> {
        let comp = comp.unwrap_or_default();
        writeln!(
            self.writer,
            "{phase},{iteration},{},{},{},{},{},{},{},{},{},{},{},{}",
            capture_ok
                .map(|ok| if ok { "true" } else { "false" })
                .unwrap_or(""),
            probe.fd_count,
            probe.rss_kb,
            probe.shmem_kb,
            probe.memfd_count,
            comp_pid.unwrap_or(0),
            comp.fd_count,
            comp.rss_kb,
            comp.shmem_kb,
            comp.memfd_count,
            comp.capture_memfd_count,
            std::process::id(),
        )?;
        self.writer.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::{GROWTH_THRESHOLD, GROWTH_WINDOW, monotonic_growth};
    use std::collections::VecDeque;

    #[test]
    fn detects_monotonic_growth() {
        let values = (0..GROWTH_WINDOW)
            .map(|index| 90 + index)
            .collect::<VecDeque<_>>();
        assert!(values.back().unwrap() - values.front().unwrap() >= GROWTH_THRESHOLD);
        assert!(monotonic_growth(&values));
    }

    #[test]
    fn ignores_bounded_oscillation() {
        let values = VecDeque::from([92, 93, 92, 93, 92, 93]);
        assert!(!monotonic_growth(&values));
    }
}
