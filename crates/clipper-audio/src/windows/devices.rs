//! Audio endpoint enumeration (MMDevice API, Windows 10+).

use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Media::Audio::{
    DEVICE_STATE_ACTIVE, EDataFlow, IAudioClient, IMMDevice, IMMDeviceEnumerator,
    MMDeviceEnumerator, eCapture, eConsole, eRender,
};
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance, CoTaskMemFree, STGM_READ};
use windows::Win32::System::Variant::VT_LPWSTR;
use windows::core::PWSTR;

use super::stream::{ComGuard, map_os_error};
use crate::types::{AudioDevice, AudioError, DeviceFlow};

pub(super) fn data_flow(flow: DeviceFlow) -> EDataFlow {
    match flow {
        DeviceFlow::Render => eRender,
        DeviceFlow::Capture => eCapture,
    }
}

pub(super) fn create_enumerator() -> Result<IMMDeviceEnumerator, AudioError> {
    // SAFETY: standard COM activation; the calling thread has COM initialized.
    unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
        .map_err(|e| map_os_error("CoCreateInstance(MMDeviceEnumerator)", &e))
}

/// Takes ownership of a CoTaskMem-allocated wide string.
fn take_pwstr(text: PWSTR) -> String {
    if text.is_null() {
        return String::new();
    }
    // SAFETY: non-null, NUL-terminated string allocated by COM; freed once.
    unsafe {
        let value = text.to_string().unwrap_or_default();
        CoTaskMemFree(Some(text.0.cast_const().cast()));
        value
    }
}

pub(super) fn device_id(device: &IMMDevice) -> Result<String, AudioError> {
    // SAFETY: valid device; GetId returns a CoTaskMem string we take over.
    let id = unsafe { device.GetId() }.map_err(|e| map_os_error("IMMDevice::GetId", &e))?;
    Ok(take_pwstr(id))
}

/// Friendly name, falling back to the endpoint id.
pub(super) fn friendly_name(device: &IMMDevice) -> String {
    let name = (|| {
        // SAFETY: valid device; the property store and variant are released
        // by their destructors (PROPVARIANT's Drop calls PropVariantClear).
        unsafe {
            let store = device.OpenPropertyStore(STGM_READ).ok()?;
            let value = store.GetValue(&PKEY_Device_FriendlyName).ok()?;
            let inner = &value.Anonymous.Anonymous;
            if inner.vt == VT_LPWSTR && !inner.Anonymous.pwszVal.is_null() {
                inner.Anonymous.pwszVal.to_string().ok()
            } else {
                None
            }
        }
    })();
    name.unwrap_or_else(|| device_id(device).unwrap_or_default())
}

/// `(channels, sample_rate)` of the shared-mode mix format.
pub(super) fn mix_format(device: &IMMDevice) -> Option<(u16, u32)> {
    // SAFETY: Activate/GetMixFormat on a valid device; the returned format is
    // copied field-wise (packed struct, read by value) and freed once.
    unsafe {
        let client: IAudioClient = device.Activate(CLSCTX_ALL, None).ok()?;
        let format = client.GetMixFormat().ok()?;
        if format.is_null() {
            return None;
        }
        let channels = std::ptr::addr_of!((*format).nChannels).read_unaligned();
        let rate = std::ptr::addr_of!((*format).nSamplesPerSec).read_unaligned();
        CoTaskMemFree(Some(format.cast_const().cast()));
        Some((channels, rate))
    }
}

/// Active render or capture endpoints with their shared-mode mix format.
pub fn list_devices(flow: DeviceFlow) -> Result<Vec<AudioDevice>, AudioError> {
    let _com = ComGuard::init();
    list_inner(flow)
}

fn list_inner(flow: DeviceFlow) -> Result<Vec<AudioDevice>, AudioError> {
    let enumerator = create_enumerator()?;
    let data_flow = data_flow(flow);
    // SAFETY: valid enumerator.
    let collection = unsafe { enumerator.EnumAudioEndpoints(data_flow, DEVICE_STATE_ACTIVE) }
        .map_err(|e| map_os_error("EnumAudioEndpoints", &e))?;
    // SAFETY: valid collection.
    let count = unsafe { collection.GetCount() }.map_err(|e| map_os_error("GetCount", &e))?;
    // SAFETY: valid enumerator; a missing default is not an error here.
    let default_id = unsafe { enumerator.GetDefaultAudioEndpoint(data_flow, eConsole) }
        .ok()
        .and_then(|device| device_id(&device).ok());
    let mut devices = Vec::with_capacity(count as usize);
    for index in 0..count {
        // SAFETY: index < count.
        let Ok(device) = (unsafe { collection.Item(index) }) else {
            continue;
        };
        let Ok(id) = device_id(&device) else {
            continue;
        };
        let (channels, sample_rate) = mix_format(&device).unwrap_or((0, 0));
        devices.push(AudioDevice {
            name: friendly_name(&device),
            flow,
            is_default: default_id.as_deref() == Some(id.as_str()),
            id,
            channels,
            sample_rate,
        });
    }
    Ok(devices)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "needs audio hardware; CI runners have no render device"]
    fn hardware_list_render_devices_has_default() {
        let devices = list_devices(DeviceFlow::Render).expect("list render");
        for d in &devices {
            println!("render: {d:?}");
        }
        assert!(!devices.is_empty(), "no active render device");
        assert!(devices.iter().any(|d| d.is_default));
        assert!(devices.iter().all(|d| d.channels > 0 && d.sample_rate > 0));
    }

    #[test]
    #[ignore = "needs audio hardware"]
    fn hardware_list_capture_devices_succeeds() {
        let devices = list_devices(DeviceFlow::Capture).expect("list capture");
        println!("capture devices: {}", devices.len());
        for d in &devices {
            println!("capture: {d:?}");
        }
    }
}
