//! Host resource checks (RAM availability) before starting VMs.

use std::fs;

/// RAM requirements for starting VMs.
///
/// Used by [`check_ram`] to verify sufficient memory before each VM launch.
pub struct RamRequirements {
    /// Minimum free RAM per VM (bytes).
    pub per_vm: u64,
    /// Minimum RAM to keep free for the host (bytes).
    pub host_reserve: u64,
}

/// Read `MemAvailable` from `/proc/meminfo` (Linux only).
pub fn available_ram() -> anyhow::Result<u64> {
    let contents = fs::read_to_string("/proc/meminfo")?;
    for line in contents.lines() {
        if let Some(rest) = line.strip_prefix("MemAvailable:") {
            let kb: u64 = rest.trim().strip_suffix("kB").unwrap_or(rest.trim())
                .trim()
                .parse()?;
            return Ok(kb * 1024);
        }
    }
    anyhow::bail!("MemAvailable not found in /proc/meminfo (Linux required)")
}

/// Check whether there is enough RAM to start one more VM.
/// Returns the current available RAM on success, or an error describing the shortfall.
pub fn check_ram(req: &RamRequirements) -> anyhow::Result<u64> {
    let avail = available_ram()?;
    let needed = req.per_vm + req.host_reserve;
    if avail < needed {
        anyhow::bail!(
            "insufficient RAM: {:.1} GB available, need {:.1} GB per VM + {:.1} GB host reserve",
            avail as f64 / 1e9,
            req.per_vm as f64 / 1e9,
            req.host_reserve as f64 / 1e9,
        );
    }
    Ok(avail)
}
