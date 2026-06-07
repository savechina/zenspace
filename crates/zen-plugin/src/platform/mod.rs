pub mod calendar;
pub mod daemon;
pub mod fs_watcher;
pub mod health;
pub mod notifications;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    MacOS,
    Linux,
    Windows,
}

pub fn detect_platform() -> Platform {
    #[cfg(target_os = "macos")]
    return Platform::MacOS;
    #[cfg(target_os = "linux")]
    return Platform::Linux;
    #[cfg(target_os = "windows")]
    return Platform::Windows;
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    compile_error!("Unsupported platform");
}

impl Platform {
    pub fn as_str(&self) -> &'static str {
        match self {
            Platform::MacOS => "macos",
            Platform::Linux => "linux",
            Platform::Windows => "windows",
        }
    }
}
