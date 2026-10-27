//! How much memory this process uses, and giving freed memory back to the system.
//!
//! [`process_memory`] reads the numbers the OS's own tools show: on macOS the *footprint*
//! (Activity Monitor's "Memory" column, `footprint <pid>`: dirty private memory including
//! compressed and GPU memory the process owns) and the resident size; on Linux the private
//! resident plus swapped memory and the resident size (`/proc/self/status`); on Windows the
//! private commit and the working set.
//!
//! [`release_free_memory`] asks the allocator to return pages it holds free. Freeing a cache does
//! not always make a process smaller at once: glibc keeps freed heap at the top of its arenas, and
//! macOS's allocator keeps freed small and medium regions for reuse (macOS 26 hands them back by
//! itself within seconds; earlier releases only under memory pressure). Call it after dropping
//! caches (an app going idle), not per frame: it walks every allocator zone.
//!
//! | | macOS | Linux | Windows | Web |
//! |---|---|---|---|---|
//! | [`process_memory`] | `proc_pid_rusage` | `/proc/self/status` | `GetProcessMemoryInfo` | `None` |
//! | [`release_free_memory`] | `malloc_zone_pressure_relief` | `malloc_trim` (glibc) | no-op | no-op |

use serde::Serialize;

/// Memory of this process, in bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessMemory {
    /// What the system charges the process for: macOS phys_footprint, Linux private resident +
    /// swap (`RssAnon + VmSwap`), Windows private commit (`PrivateUsage`).
    pub footprint: u64,
    /// Pages resident in RAM right now, shared ones included (RSS / working set).
    pub resident: u64,
}

/// This process's memory, or `None` where the platform has no way to ask (the web) or the call
/// failed.
pub fn process_memory() -> Option<ProcessMemory> {
    imp::process_memory()
}

/// Return the allocator's free pages to the system. Returns the bytes it reports releasing
/// (macOS; 0 where the allocator does not say, or nothing was done).
pub fn release_free_memory() -> u64 {
    imp::release_free_memory()
}

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod imp {
    use super::ProcessMemory;

    // libmalloc (part of libSystem, always linked). `zone` null means every zone; `goal` 0 means
    // release as much as possible. Returns the bytes released.
    unsafe extern "C" {
        fn malloc_zone_pressure_relief(zone: *mut libc::c_void, goal: libc::size_t) -> libc::size_t;
    }

    pub fn process_memory() -> Option<ProcessMemory> {
        let mut info = std::mem::MaybeUninit::<libc::rusage_info_v2>::zeroed();
        // SAFETY: `info` is a properly aligned, writable `rusage_info_v2`, the struct the
        // RUSAGE_INFO_V2 flavor fills; the kernel writes at most its size. The header declares the
        // buffer as `rusage_info_t *` (void **) but takes the struct's address, as Apple's own
        // callers do.
        let r = unsafe {
            libc::proc_pid_rusage(std::process::id() as libc::c_int, libc::RUSAGE_INFO_V2, info.as_mut_ptr().cast::<libc::rusage_info_t>())
        };
        if r != 0 {
            return None;
        }
        // SAFETY: the call succeeded and filled the struct; every field is a plain integer, for
        // which the zeroed start was already a valid value.
        let info = unsafe { info.assume_init() };
        Some(ProcessMemory { footprint: info.ri_phys_footprint, resident: info.ri_resident_size })
    }

    pub fn release_free_memory() -> u64 {
        // SAFETY: a null zone is documented to mean all zones; the call only returns free pages
        // to the kernel and touches no memory the program uses.
        let n = unsafe { malloc_zone_pressure_relief(std::ptr::null_mut(), 0) };
        n as u64
    }
}

#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
mod imp {
    use super::ProcessMemory;

    pub fn process_memory() -> Option<ProcessMemory> {
        parse_status(&std::fs::read_to_string("/proc/self/status").ok()?)
    }

    /// `RssAnon`, `VmSwap` and `VmRSS` of a `/proc/<pid>/status` text (kB values).
    pub(super) fn parse_status(text: &str) -> Option<ProcessMemory> {
        let field = |name: &str| -> Option<u64> {
            let line = text.lines().find(|l| l.starts_with(name) && l.as_bytes().get(name.len()) == Some(&b':'))?;
            let kb: u64 = line.get(name.len() + 1..)?.trim().trim_end_matches("kB").trim().parse().ok()?;
            Some(kb.saturating_mul(1024))
        };
        let resident = field("VmRSS")?;
        let footprint = field("RssAnon").map_or(resident, |anon| anon.saturating_add(field("VmSwap").unwrap_or(0)));
        Some(ProcessMemory { footprint, resident })
    }

    pub fn release_free_memory() -> u64 {
        #[cfg(target_env = "gnu")]
        {
            // SAFETY: malloc_trim only returns free heap pages to the kernel; 0 = keep no padding.
            unsafe { libc::malloc_trim(0) };
        }
        0
    }
}

#[cfg(target_os = "windows")]
#[allow(unsafe_code)]
mod imp {
    use super::ProcessMemory;
    use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX};
    use windows::Win32::System::Threading::GetCurrentProcess;

    pub fn process_memory() -> Option<ProcessMemory> {
        let mut c = PROCESS_MEMORY_COUNTERS_EX { cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32, ..Default::default() };
        // SAFETY: GetCurrentProcess returns a pseudo handle that needs no closing; `c` is a
        // writable PROCESS_MEMORY_COUNTERS_EX whose `cb` says its size, which the call accepts in
        // place of the smaller PROCESS_MEMORY_COUNTERS it is declared with.
        let ok = unsafe { GetProcessMemoryInfo(GetCurrentProcess(), (&raw mut c).cast::<PROCESS_MEMORY_COUNTERS>(), c.cb) };
        ok.ok()?;
        Some(ProcessMemory { footprint: c.PrivateUsage as u64, resident: c.WorkingSetSize as u64 })
    }

    pub fn release_free_memory() -> u64 {
        0
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
mod imp {
    use super::ProcessMemory;

    pub fn process_memory() -> Option<ProcessMemory> {
        None
    }

    pub fn release_free_memory() -> u64 {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    #[test]
    fn the_footprint_grows_with_touched_memory_and_shrinks_after_release() {
        let before = process_memory().expect("process memory");
        assert!(before.footprint > 0 && before.resident > 0, "{before:?}");
        // 256 MB in 1 MB blocks (macOS's medium-block regions, which stay dirty once freed)
        let mut blocks: Vec<Vec<u8>> = (0..256).map(|_| vec![1u8; 1 << 20]).collect();
        let full = process_memory().expect("process memory");
        assert!(full.footprint >= before.footprint + (200 << 20), "{before:?} → {full:?}");
        blocks.clear();
        blocks.shrink_to_fit();
        release_free_memory();
        // the system may take back freed pages a few seconds later (macOS 26)
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        let after = loop {
            let now = process_memory().expect("process memory");
            if now.footprint + (150 << 20) <= full.footprint || std::time::Instant::now() > deadline {
                break now;
            }
            std::thread::sleep(std::time::Duration::from_millis(250));
        };
        assert!(after.footprint + (150 << 20) <= full.footprint, "released: {full:?} → {after:?}");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_status_is_parsed() {
        let text = "Name:\tx\nVmRSS:\t  2048 kB\nRssAnon:\t  1024 kB\nVmSwap:\t     4 kB\n";
        assert_eq!(imp::parse_status(text), Some(ProcessMemory { footprint: 1028 * 1024, resident: 2048 * 1024 }));
        assert_eq!(imp::parse_status("VmRSS: x kB"), None);
    }
}
