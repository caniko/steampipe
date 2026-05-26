//! Host-side memory admission gate around parallel VM startup.
//!
//! Defends against host OOM-killer claiming a hypervisor process when multiple
//! VMs allocate initial memory simultaneously. Guest-side OOM is handled by
//! virtio-balloon + deflateOnOOM (see `nix/lib/microvm-runner.nix`).

use std::sync::OnceLock;
use std::time::Duration;

use memory_admission::r#async::AdmissionGate;
use memory_admission::providers::ProcMeminfoProvider;
use memory_admission::{Config, MemoryProvider, MemoryStats};

pub const HIGH_WATERMARK_MB: u64 = 5 * 1024;
pub const LOW_WATERMARK_MB: u64 = 6 * 1024;

const MB: u64 = 1024 * 1024;
const POLL_INTERVAL: Duration = Duration::from_millis(500);

static GATE: OnceLock<AdmissionGate> = OnceLock::new();
static POLICY: OnceLock<AdmissionPolicy> = OnceLock::new();

#[derive(Debug, Clone, Copy)]
pub struct AdmissionPolicy {
    pub high_watermark_mb: u64,
    pub low_watermark_mb: u64,
    pub max_ram_fraction: f64,
    pub resume_hysteresis: f64,
    pub memory_scheduler_enabled: bool,
}

pub fn gate() -> &'static AdmissionGate {
    GATE.get_or_init(|| {
        let (policy, config) = config_from_proc_meminfo();
        let _ = POLICY.set(policy);
        AdmissionGate::new(config, ProcMeminfoProvider::shared())
    })
}

pub fn policy() -> AdmissionPolicy {
    *POLICY.get_or_init(|| {
        let (policy, _config) = config_from_proc_meminfo();
        policy
    })
}

/// Block until host memory pressure falls below the configured watermark.
///
/// Provider failures disengage throttling inside `memory-admission`; startup
/// continues because this gate is defense in depth, not a hard dependency.
pub async fn admit() {
    let _permit = gate().acquire().await;
}

pub fn current_stats() -> Option<MemoryStats> {
    match ProcMeminfoProvider.stats() {
        Ok(stats) => Some(stats),
        Err(error) => {
            tracing::warn!(
                error = %error,
                "admission gate provider failed while reading status"
            );
            None
        }
    }
}

fn config_from_proc_meminfo() -> (AdmissionPolicy, Config) {
    let stats = match ProcMeminfoProvider.stats() {
        Ok(stats) => stats,
        Err(error) => {
            tracing::warn!(
                error = %error,
                "admission gate provider failed at startup; proceeding without throttling"
            );
            return disabled_policy_and_config();
        }
    };

    let total_mb = stats.total_bytes / MB;
    if total_mb <= LOW_WATERMARK_MB {
        tracing::warn!(
            total_mb,
            high_watermark_mb = HIGH_WATERMARK_MB,
            low_watermark_mb = LOW_WATERMARK_MB,
            "host memory is below admission gate watermarks; proceeding without throttling"
        );
        return disabled_policy_and_config();
    }

    let max_ram_fraction = watermark_used_fraction(stats.total_bytes, HIGH_WATERMARK_MB);
    let resume_fraction = watermark_used_fraction(stats.total_bytes, LOW_WATERMARK_MB);
    let resume_hysteresis = max_ram_fraction - resume_fraction;
    let config = Config {
        max_ram_fraction,
        resume_hysteresis,
        poll_interval: POLL_INTERVAL,
        memory_scheduler_enabled: true,
    }
    .validate()
    .expect("derived admission gate config must be valid");

    (
        AdmissionPolicy {
            high_watermark_mb: HIGH_WATERMARK_MB,
            low_watermark_mb: LOW_WATERMARK_MB,
            max_ram_fraction,
            resume_hysteresis,
            memory_scheduler_enabled: true,
        },
        config,
    )
}

fn disabled_policy_and_config() -> (AdmissionPolicy, Config) {
    let config = Config {
        poll_interval: POLL_INTERVAL,
        memory_scheduler_enabled: false,
        ..Config::default()
    }
    .validate()
    .expect("disabled admission gate config must be valid");
    (
        AdmissionPolicy {
            high_watermark_mb: HIGH_WATERMARK_MB,
            low_watermark_mb: LOW_WATERMARK_MB,
            max_ram_fraction: config.max_ram_fraction,
            resume_hysteresis: config.resume_hysteresis,
            memory_scheduler_enabled: false,
        },
        config,
    )
}

fn watermark_used_fraction(total_bytes: u64, available_watermark_mb: u64) -> f64 {
    let watermark_bytes = available_watermark_mb.saturating_mul(MB);
    1.0 - (watermark_bytes as f64 / total_bytes as f64)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use memory_admission::r#async::AdmissionGate;
    use memory_admission::provider::SharedMemoryProvider;

    use super::*;

    #[tokio::test]
    async fn gate_blocks_when_free_below_high_watermark() {
        let probes = Arc::new(AtomicUsize::new(0));
        let probes_for_provider = Arc::clone(&probes);
        let provider: SharedMemoryProvider = Arc::new(move || {
            let probe = probes_for_provider.fetch_add(1, Ordering::SeqCst);
            Ok(if probe < 3 { 0.95 } else { 0.50 })
        });
        let gate = AdmissionGate::new(
            Config {
                max_ram_fraction: 0.80,
                resume_hysteresis: 0.10,
                poll_interval: Duration::from_millis(10),
                memory_scheduler_enabled: true,
            }
            .validate()
            .unwrap(),
            provider,
        );

        let acquire = tokio::spawn({
            let gate = gate.clone();
            async move { gate.acquire().await }
        });

        tokio::time::sleep(Duration::from_millis(5)).await;
        assert!(!acquire.is_finished());

        let permit = acquire.await.unwrap();
        drop(permit);
        assert!(probes.load(Ordering::SeqCst) >= 4);
    }

    #[test]
    fn derives_fraction_thresholds_from_available_watermarks() {
        let total = 16 * 1024 * MB;

        assert_eq!(watermark_used_fraction(total, HIGH_WATERMARK_MB), 0.6875);
        assert_eq!(watermark_used_fraction(total, LOW_WATERMARK_MB), 0.625);
    }
}
