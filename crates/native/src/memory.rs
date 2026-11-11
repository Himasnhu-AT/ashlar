//! How much memory this process uses, and giving freed memory back to the system.
//!
//! [`process_memory`] reads the numbers the OS's own tools show: on macOS the *footprint*
//! (Activity Monitor's "Memory" column, `footprint <pid>`: dirty private memory including
//! compressed and GPU memory the process owns) and the resident size; on Linux the private
//! resident plus swapped memory and the resident size (`/proc/self/status`); on Windows the
//! private commit and the working set. Where the OS keeps it, it also gives the highest footprint
//! since the process started, so a short spike between two readings still counts.
//! [`thread_count`] says how many threads the process runs.
//!
//! [`release_free_memory`] asks the allocator to return pages it holds free. Where that works
//! (glibc's `malloc_trim`) it shrinks the process at once. On macOS it does nothing: the system
//! call for it (`malloc_zone_pressure_relief`) releases next to
//! nothing (measured: 0–49 KB after freeing gigabytes): the default allocator keeps freed small
//! and medium regions dirty for reuse and the kernel takes them back by itself over the next
//! 30–60 s. A process started with `MallocSpaceEfficient=1` in its environment hands freed
//! regions back at once (a bundled app sets it through `LSEnvironment` in its Info.plist);
//! [`space_efficient_malloc`] says whether this one was. Call it after dropping caches, not per
//! frame.
//!
//! | | macOS | Linux | Windows | Web |
//! |---|---|---|---|---|
//! | [`process_memory`] | `proc_pid_rusage` | `/proc/self/status` | `GetProcessMemoryInfo` | `None` |
//! | peak | lifetime max footprint | – (`None`) | `PeakPagefileUsage` | `None` |
//! | [`thread_count`] | `proc_pidinfo` | `/proc/self/status` | toolhelp snapshot | `None` |
//! | [`release_free_memory`] | no-op (see above) | `malloc_trim` (glibc) | no-op | no-op |

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
    /// The highest `footprint` since the process started, where the OS keeps it: macOS
    /// `ri_lifetime_max_phys_footprint`, Windows peak private commit (`PeakPagefileUsage`).
    /// `None` on Linux, which keeps no peak of the same measure.
    pub peak: Option<u64>,
}

/// This process's memory, or `None` where the platform has no way to ask (the web) or the call
/// failed.
pub fn process_memory() -> Option<ProcessMemory> {
    imp::process_memory()
}

/// How many threads this process runs, or `None` where the platform has no way to ask (the web)
/// or the call failed.
pub fn thread_count() -> Option<u64> {
    imp::thread_count()
}

/// Return the allocator's free pages to the system where the allocator can (glibc's
/// `malloc_trim`). Returns the bytes released where the allocator says; 0 everywhere today (macOS
/// has nothing that works, see the module docs; glibc does not say).
pub fn release_free_memory() -> u64 {
    imp::release_free_memory()
}

/// Whether macOS's allocator runs in its space-efficient mode (`MallocSpaceEfficient=1` in the
/// environment the process started with), which returns freed memory to the system at once.
/// Always false elsewhere.
pub fn space_efficient_malloc() -> bool {
    cfg!(target_os = "macos") && std::env::var_os("MallocSpaceEfficient").is_some_and(|v| v != "0" && !v.is_empty())
}

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod imp {
    use super::ProcessMemory;

    pub fn process_memory() -> Option<ProcessMemory> {
        let mut info = std::mem::MaybeUninit::<libc::rusage_info_v4>::zeroed();
        // SAFETY: `info` is a properly aligned, writable `rusage_info_v4`, the struct the
        // RUSAGE_INFO_V4 flavor fills (macOS 10.14 and later); the kernel writes at most its size. The header declares the
        // buffer as `rusage_info_t *` (void **) but takes the struct's address, as Apple's own
        // callers do.
        let r = unsafe {
            libc::proc_pid_rusage(std::process::id() as libc::c_int, libc::RUSAGE_INFO_V4, info.as_mut_ptr().cast::<libc::rusage_info_t>())
        };
        if r != 0 {
            return None;
        }
        // SAFETY: the call succeeded and filled the struct; every field is a plain integer, for
        // which the zeroed start was already a valid value.
        let info = unsafe { info.assume_init() };
        Some(ProcessMemory {
            footprint: info.ri_phys_footprint,
            resident: info.ri_resident_size,
            peak: Some(info.ri_lifetime_max_phys_footprint.max(info.ri_phys_footprint)),
        })
    }

    pub fn thread_count() -> Option<u64> {
        let mut info = std::mem::MaybeUninit::<libc::proc_taskinfo>::zeroed();
        let size = libc::c_int::try_from(std::mem::size_of::<libc::proc_taskinfo>()).ok()?;
        // SAFETY: `info` is a properly aligned, writable `proc_taskinfo` of `size` bytes, the
        // struct the PROC_PIDTASKINFO flavor fills; the kernel writes at most `size` bytes and
        // returns how many it wrote.
        let n = unsafe { libc::proc_pidinfo(std::process::id() as libc::c_int, libc::PROC_PIDTASKINFO, 0, info.as_mut_ptr().cast(), size) };
        if n != size {
            return None;
        }
        // SAFETY: the call filled all `size` bytes; every field is a plain integer, for which the
        // zeroed start was already a valid value.
        let info = unsafe { info.assume_init() };
        u64::try_from(info.pti_threadnum).ok()
    }

    /// `malloc_zone_pressure_relief` was measured to release next to nothing on macOS 26 (0–49 KB
    /// after freeing gigabytes), so there is no call here; see the module docs.
    pub fn release_free_memory() -> u64 {
        0
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
        Some(ProcessMemory { footprint, resident, peak: None })
    }

    pub fn thread_count() -> Option<u64> {
        super::threads_in_status(&std::fs::read_to_string("/proc/self/status").ok()?)
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
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
    };
    use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX};
    use windows::Win32::System::Threading::GetCurrentProcess;

    pub fn process_memory() -> Option<ProcessMemory> {
        let mut c = PROCESS_MEMORY_COUNTERS_EX { cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32, ..Default::default() };
        // SAFETY: GetCurrentProcess returns a pseudo handle that needs no closing; `c` is a
        // writable PROCESS_MEMORY_COUNTERS_EX whose `cb` says its size, which the call accepts in
        // place of the smaller PROCESS_MEMORY_COUNTERS it is declared with.
        let ok = unsafe { GetProcessMemoryInfo(GetCurrentProcess(), (&raw mut c).cast::<PROCESS_MEMORY_COUNTERS>(), c.cb) };
        ok.ok()?;
        let footprint = c.PrivateUsage as u64;
        Some(ProcessMemory { footprint, resident: c.WorkingSetSize as u64, peak: Some((c.PeakPagefileUsage as u64).max(footprint)) })
    }

    /// Walks a toolhelp snapshot of the processes to this one's entry (its `cntThreads`).
    pub fn thread_count() -> Option<u64> {
        // SAFETY: a process snapshot takes no pointers; the handle is closed below.
        let snap = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }.ok()?;
        let me = std::process::id();
        let mut entry = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        // SAFETY: `snap` is the live snapshot from above and `entry` a writable PROCESSENTRY32W
        // whose `dwSize` says its size, as both calls require.
        let mut more = unsafe { Process32FirstW(snap, &raw mut entry) }.is_ok();
        let mut found = None;
        // bounded: a snapshot lists every process once, far fewer than this
        for _ in 0..1_000_000 {
            if !more {
                break;
            }
            if entry.th32ProcessID == me {
                found = Some(u64::from(entry.cntThreads));
                break;
            }
            // SAFETY: as for Process32FirstW above.
            more = unsafe { Process32NextW(snap, &raw mut entry) }.is_ok();
        }
        // SAFETY: `snap` is a handle this function owns and closes once.
        let _ = unsafe { CloseHandle(snap) };
        found
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

    pub fn thread_count() -> Option<u64> {
        None
    }

    pub fn release_free_memory() -> u64 {
        0
    }
}

/// The `Threads:` field of a `/proc/<pid>/status` text.
#[cfg_attr(not(any(target_os = "linux", test)), allow(dead_code))]
fn threads_in_status(text: &str) -> Option<u64> {
    text.lines().find_map(|l| l.strip_prefix("Threads:")).and_then(|v| v.trim().parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    #[test]
    fn the_footprint_counts_touched_memory() {
        let before = process_memory().expect("process memory");
        assert!(before.footprint > 0 && before.resident > 0, "{before:?}");
        // 256 MB in 1 MB blocks, every page written
        let blocks: Vec<Vec<u8>> = (0..256).map(|_| vec![1u8; 1 << 20]).collect();
        let full = process_memory().expect("process memory");
        assert!(full.footprint >= before.footprint + (200 << 20), "{before:?} → {full:?}");
        drop(blocks);
        // Whether freed pages leave at once is the allocator's business (macOS keeps them for
        // up to a minute unless MallocSpaceEfficient=1): only the call itself is checked here.
        release_free_memory();
        assert!(process_memory().is_some());
    }

    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    #[test]
    fn malloc_trim_hands_freed_heap_back() {
        let blocks: Vec<Vec<u8>> = (0..256).map(|_| vec![1u8; 100 << 10]).collect();
        let full = process_memory().expect("process memory");
        drop(blocks);
        release_free_memory();
        let after = process_memory().expect("process memory");
        assert!(after.footprint + (150 << 20) <= full.footprint, "{full:?} → {after:?}");
    }

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    #[test]
    fn the_os_peak_outlives_a_freed_spike() {
        let before = process_memory().expect("process memory");
        let peak_before = before.peak.expect("peak");
        assert!(peak_before >= before.footprint, "{before:?}");
        // a 128 MB spike, written, then freed before the next reading
        let blocks: Vec<Vec<u8>> = (0..128).map(|_| vec![1u8; 1 << 20]).collect();
        let during = process_memory().expect("process memory");
        drop(blocks);
        let after = process_memory().expect("process memory");
        let peak = after.peak.expect("peak");
        assert!(peak >= during.footprint.max(peak_before), "{before:?} {during:?} {after:?}");
        assert!(peak >= before.footprint + (100 << 20), "{before:?} → {after:?}");
    }

    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    #[test]
    fn threads_are_counted() {
        let before = thread_count().expect("thread count");
        assert!(before >= 1);
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<()>();
        let t = std::thread::spawn(move || {
            let _ = ready_tx.send(());
            let _ = rx.recv();
        });
        let _ = ready_rx.recv();
        // other tests run threads in parallel: only "at least one more than zero" is stable
        let during = thread_count().expect("thread count");
        assert!(during >= 2, "{before} → {during}");
        let _ = tx.send(());
        let _ = t.join();
    }

    #[test]
    fn proc_status_threads_parse() {
        assert_eq!(threads_in_status("Name:\tx\nThreads:\t7\nVmRSS: 1 kB\n"), Some(7));
        assert_eq!(threads_in_status("Name: x\n"), None);
    }

    #[test]
    fn the_space_efficient_mode_follows_the_environment() {
        let set = std::env::var_os("MallocSpaceEfficient").is_some_and(|v| v != "0" && !v.is_empty());
        assert_eq!(space_efficient_malloc(), set && cfg!(target_os = "macos"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_status_is_parsed() {
        let text = "Name:\tx\nVmRSS:\t  2048 kB\nRssAnon:\t  1024 kB\nVmSwap:\t     4 kB\n";
        assert_eq!(imp::parse_status(text), Some(ProcessMemory { footprint: 1028 * 1024, resident: 2048 * 1024, peak: None }));
        assert_eq!(imp::parse_status("VmRSS: x kB"), None);
    }
}
