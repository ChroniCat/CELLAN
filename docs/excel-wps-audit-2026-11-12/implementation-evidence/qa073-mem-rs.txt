//! 进程工作集读数：给保存/重算的内存诊断用（SC_SAVE_PROF 等钩子）。
//!
//! 注意这只是给探针用的观测点，业务代码不该依赖它做决策。

/// 当前工作集（MB）；非 Windows 返回 0。
pub fn rss_mb() -> f64 {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::System::ProcessStatus::{
            GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
        };
        use windows_sys::Win32::System::Threading::GetCurrentProcess;
        let mut c: PROCESS_MEMORY_COUNTERS = std::mem::zeroed();
        c.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        if GetProcessMemoryInfo(GetCurrentProcess(), &mut c, c.cb) != 0 {
            return c.WorkingSetSize as f64 / 1_048_576.0;
        }
    }
    0.0
}
