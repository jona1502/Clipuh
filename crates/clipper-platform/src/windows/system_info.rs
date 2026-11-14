//! Read-only facts about the machine for diagnostic exports: the Windows
//! release and the GPU adapters. Nothing here identifies the user.

use std::ffi::c_void;

use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE, IDXGIFactory1,
};
use windows::Win32::System::Registry::{
    HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RegGetValueW,
};
use windows::core::{PCWSTR, w};

const VERSION_KEY: PCWSTR = w!("SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion");

fn registry_string(value: PCWSTR) -> Option<String> {
    let mut buffer = [0u16; 256];
    let mut size = std::mem::size_of_val(&buffer) as u32;
    // SAFETY: `buffer` is writable for `size` bytes, key and value are static
    // NUL-terminated literals; RRF_RT_REG_SZ guarantees a terminated result.
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            VERSION_KEY,
            value,
            RRF_RT_REG_SZ,
            None,
            Some(buffer.as_mut_ptr().cast::<c_void>()),
            Some(&mut size),
        )
    };
    status.is_ok().then(|| {
        let chars = (size as usize / 2).saturating_sub(1);
        String::from_utf16_lossy(&buffer[..chars])
    })
}

fn registry_dword(value: PCWSTR) -> Option<u32> {
    let mut data = 0u32;
    let mut size = std::mem::size_of::<u32>() as u32;
    // SAFETY: `data` is a writable u32 of `size` bytes; static literals.
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            VERSION_KEY,
            value,
            RRF_RT_REG_DWORD,
            None,
            Some((&mut data as *mut u32).cast::<c_void>()),
            Some(&mut size),
        )
    };
    status.is_ok().then_some(data)
}

/// E.g. `Windows 11 Pro 24H2 (build 26100.2605)`. `ProductName` still says
/// "Windows 10" on Windows 11, so the build number decides the major version.
pub fn windows_version() -> Option<String> {
    let build: u32 = registry_string(w!("CurrentBuildNumber"))?
        .trim()
        .parse()
        .ok()?;
    let product = registry_string(w!("ProductName")).unwrap_or_else(|| "Windows".into());
    let product = if build >= 22000 {
        product.replacen("Windows 10", "Windows 11", 1)
    } else {
        product
    };
    let release = registry_string(w!("DisplayVersion"))
        .map(|release| format!(" {release}"))
        .unwrap_or_default();
    let patch = registry_dword(w!("UBR"))
        .map(|ubr| format!(".{ubr}"))
        .unwrap_or_default();
    Some(format!("{product}{release} (build {build}{patch})"))
}

/// Hardware GPU adapters as `name (driver version)` where available.
pub fn gpu_adapters() -> Vec<String> {
    // SAFETY: plain factory creation; the interface is released on drop.
    let Ok(factory) = (unsafe { CreateDXGIFactory1::<IDXGIFactory1>() }) else {
        return Vec::new();
    };
    let mut adapters = Vec::new();
    for index in 0.. {
        // SAFETY: valid factory; enumeration ends with an error.
        let Ok(adapter) = (unsafe { factory.EnumAdapters1(index) }) else {
            break;
        };
        // SAFETY: valid adapter.
        let Ok(desc) = (unsafe { adapter.GetDesc1() }) else {
            continue;
        };
        if desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0 {
            continue;
        }
        let end = desc
            .Description
            .iter()
            .position(|c| *c == 0)
            .unwrap_or(desc.Description.len());
        let name = String::from_utf16_lossy(&desc.Description[..end]);
        if !name.trim().is_empty() && !adapters.contains(&name) {
            adapters.push(name);
        }
    }
    adapters
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_a_windows_version() {
        let version = windows_version().expect("registry readable");
        assert!(version.starts_with("Windows"), "{version}");
        assert!(version.contains("build "), "{version}");
    }

    #[test]
    #[ignore = "needs a hardware GPU"]
    fn lists_a_hardware_gpu() {
        assert!(!gpu_adapters().is_empty());
    }
}
