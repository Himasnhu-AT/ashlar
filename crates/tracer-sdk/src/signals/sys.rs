//! This process's CPU and memory, and static device details. Real values need the `sysinfo` feature; without
//! it every field here is absent and nothing is sampled.

use tracer_proto::Device;

/// One reading of this process.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Sample {
    /// Percent of one core since the previous sample. Absent on the first reading, which has no baseline.
    pub cpu_pct: Option<f32>,
    pub rss_bytes: u64,
}

#[cfg(all(feature = "sysinfo", not(target_arch = "wasm32")))]
mod imp {
    use std::sync::{Mutex, PoisonError};

    use sysinfo::{CpuRefreshKind, MemoryRefreshKind, Pid, ProcessRefreshKind, ProcessesToUpdate, RefreshKind, System};
    use tracer_proto::Device;

    use super::Sample;

    pub(crate) struct Sampler {
        state: Mutex<State>,
    }

    struct State {
        system: System,
        pid: Option<Pid>,
        primed: bool,
    }

    impl Sampler {
        pub(crate) fn new() -> Self {
            Self { state: Mutex::new(State { system: System::new(), pid: sysinfo::get_current_pid().ok(), primed: false }) }
        }

        /// Refreshes only this process, never the whole process list.
        pub(crate) fn sample(&self) -> Option<Sample> {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            let pid = state.pid?;
            let kind = ProcessRefreshKind::nothing().with_cpu().with_memory();
            state.system.refresh_processes_specifics(ProcessesToUpdate::Some(&[pid]), true, kind);
            let process = state.system.process(pid)?;
            let rss_bytes = process.memory();
            let cpu = process.cpu_usage();
            let cpu_pct = (state.primed && cpu.is_finite()).then_some(cpu.max(0.0));
            state.primed = true;
            Some(Sample { cpu_pct, rss_bytes })
        }
    }

    pub(crate) fn fill_device(device: &mut Device) {
        device.os_version = System::os_version().filter(|version| !version.trim().is_empty());
        let system = System::new_with_specifics(
            RefreshKind::nothing().with_cpu(CpuRefreshKind::nothing()).with_memory(MemoryRefreshKind::nothing().with_ram()),
        );
        device.cpu_brand = system.cpus().first().map(|cpu| cpu.brand().trim().to_owned()).filter(|brand| !brand.is_empty());
        let cores = System::physical_core_count().unwrap_or_else(|| system.cpus().len());
        device.cpu_cores = u32::try_from(cores).ok().filter(|cores| *cores > 0);
        device.ram_mb = Some(system.total_memory() / (1024 * 1024)).filter(|mb| *mb > 0);
    }
}

#[cfg(not(all(feature = "sysinfo", not(target_arch = "wasm32"))))]
mod imp {
    use tracer_proto::Device;

    use super::Sample;

    pub(crate) struct Sampler;

    impl Sampler {
        pub(crate) fn new() -> Self {
            Self
        }

        pub(crate) fn sample(&self) -> Option<Sample> {
            None
        }
    }

    pub(crate) fn fill_device(_device: &mut Device) {}
}

pub(crate) use imp::Sampler;

/// Static details about this machine for the envelope header: OS name and architecture always, and with
/// the `sysinfo` feature also OS version, CPU brand and core count, and RAM. The host adds GPU and locale.
pub(crate) fn device_info() -> Device {
    let mut device = Device { os_name: std::env::consts::OS.to_owned(), arch: std::env::consts::ARCH.to_owned(), ..Device::default() };
    imp::fill_device(&mut device);
    device
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_always_has_os_and_arch() {
        let device = device_info();
        assert!(!device.os_name.is_empty());
        assert!(!device.arch.is_empty());
        assert!(device.gpu.is_none());
        assert!(device.locale.is_none());
    }

    #[cfg(all(feature = "sysinfo", not(target_arch = "wasm32")))]
    #[test]
    fn device_has_hardware_details_with_the_feature() {
        let device = device_info();
        assert!(device.cpu_cores.is_some_and(|cores| cores > 0), "{device:?}");
        assert!(device.ram_mb.is_some_and(|mb| mb > 0), "{device:?}");
    }

    #[cfg(not(all(feature = "sysinfo", not(target_arch = "wasm32"))))]
    #[test]
    fn device_lacks_hardware_details_without_the_feature() {
        let device = device_info();
        assert!(device.cpu_brand.is_none() && device.cpu_cores.is_none() && device.ram_mb.is_none() && device.os_version.is_none());
        assert!(Sampler::new().sample().is_none());
    }

    #[cfg(all(feature = "sysinfo", not(target_arch = "wasm32")))]
    #[test]
    fn first_sample_has_memory_but_no_cpu() {
        let sampler = Sampler::new();
        let first = sampler.sample();
        assert!(first.is_some_and(|sample| sample.rss_bytes > 0 && sample.cpu_pct.is_none()), "{first:?}");
        let second = sampler.sample();
        assert!(second.is_some_and(|sample| sample.cpu_pct.is_some()), "{second:?}");
    }
}
