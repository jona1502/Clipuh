//! Endpoint loopback (default render device) and microphone sources.
//!
//! Windows version: endpoint loopback (`AUDCLNT_STREAMFLAGS_LOOPBACK`),
//! `AUTOCONVERTPCM` shared-mode format conversion and event-driven loopback
//! all work on Windows 10 and newer; no process-loopback support is needed
//! here.

use std::sync::Arc;

use windows::Win32::Media::Audio::{
    DEVICE_STATE_ACTIVE, EDataFlow, ERole, IAudioClient, IMMDevice, IMMDeviceEnumerator,
    IMMNotificationClient, IMMNotificationClient_Impl, eCapture, eConsole,
};
use windows::Win32::System::Com::CLSCTX_ALL;
use windows::Win32::System::Threading::ResetEvent;
use windows::core::{HSTRING, PCWSTR};

use super::devices::{create_enumerator, data_flow, friendly_name, mix_format};
use super::stream::{ComGuard, OpenedStream, OwnedHandle, StreamSource, init_capture_client};
use crate::types::{AudioError, DeviceFlow, MicrophoneChoice};

use super::stream::map_os_error;

/// COM callback object: signals an event when the default device of one
/// flow (console role) changes.
#[windows_core::implement(IMMNotificationClient)]
struct DefaultDeviceWatcher {
    flow: EDataFlow,
    event: Arc<OwnedHandle>,
}

impl IMMNotificationClient_Impl for DefaultDeviceWatcher_Impl {
    fn OnDeviceStateChanged(
        &self,
        _device_id: &PCWSTR,
        _new_state: windows::Win32::Media::Audio::DEVICE_STATE,
    ) -> windows_core::Result<()> {
        Ok(())
    }

    fn OnDeviceAdded(&self, _device_id: &PCWSTR) -> windows_core::Result<()> {
        Ok(())
    }

    fn OnDeviceRemoved(&self, _device_id: &PCWSTR) -> windows_core::Result<()> {
        Ok(())
    }

    fn OnDefaultDeviceChanged(
        &self,
        flow: EDataFlow,
        role: ERole,
        _default_device_id: &PCWSTR,
    ) -> windows_core::Result<()> {
        if flow == self.flow && role == eConsole {
            self.event.signal();
        }
        Ok(())
    }

    fn OnPropertyValueChanged(
        &self,
        _device_id: &PCWSTR,
        _key: &windows::Win32::Foundation::PROPERTYKEY,
    ) -> windows_core::Result<()> {
        Ok(())
    }
}

/// Registered default-device watcher; unregisters on drop.
struct DefaultFollower {
    enumerator: IMMDeviceEnumerator,
    client: IMMNotificationClient,
    /// Manual-reset; reset on every open, duplicated into `OpenedStream::reopen`.
    event: Arc<OwnedHandle>,
}

// SAFETY: `Send` is needed only because `EndpointSource` (a `Box<dyn
// StreamSource>`) is moved to the capture thread. At that point `follower` is
// always `None`: it is constructed lazily inside `open`, on the capture
// thread, which has joined the MTA, and is used and dropped there (the worker
// drops the source before CoUninitialize). The contained interfaces are MTA
// objects and the callback object is agile, so they never cross threads
// while alive.
unsafe impl Send for DefaultFollower {}

impl DefaultFollower {
    fn register(enumerator: IMMDeviceEnumerator, flow: EDataFlow) -> Result<Self, AudioError> {
        let event = Arc::new(OwnedHandle::new_event(true)?);
        let client: IMMNotificationClient = DefaultDeviceWatcher {
            flow,
            event: Arc::clone(&event),
        }
        .into();
        // SAFETY: valid enumerator and callback object; unregistered in Drop.
        unsafe { enumerator.RegisterEndpointNotificationCallback(&client) }
            .map_err(|e| map_os_error("RegisterEndpointNotificationCallback", &e))?;
        Ok(Self {
            enumerator,
            client,
            event,
        })
    }
}

impl Drop for DefaultFollower {
    fn drop(&mut self) {
        // SAFETY: `client` was registered on this enumerator in `register`.
        let _ = unsafe {
            self.enumerator
                .UnregisterEndpointNotificationCallback(&self.client)
        };
    }
}

/// Which endpoint a source opens.
enum Target {
    Default(DeviceFlow),
    Id(String),
}

/// Capture source for an endpoint (loopback of a render device or a
/// microphone), optionally following default-device changes.
struct EndpointSource {
    target: Target,
    loopback: bool,
    follower: Option<DefaultFollower>,
}

impl EndpointSource {
    fn new(target: Target, loopback: bool) -> Self {
        Self {
            target,
            loopback,
            follower: None,
        }
    }
}

fn activate_stream(
    device: &IMMDevice,
    channels: u16,
    loopback: bool,
) -> Result<OpenedStream, AudioError> {
    let description = friendly_name(device);
    // SAFETY: valid device.
    let client: IAudioClient = unsafe { device.Activate(CLSCTX_ALL, None) }
        .map_err(|e| map_os_error(&format!("IMMDevice::Activate ({description})"), &e))?;
    init_capture_client(client, channels, loopback, description)
}

impl StreamSource for EndpointSource {
    fn open(&mut self, channels: u16) -> Result<OpenedStream, AudioError> {
        let enumerator = match &self.follower {
            Some(follower) => follower.enumerator.clone(),
            None => create_enumerator()?,
        };
        let device = match &self.target {
            Target::Default(flow) => {
                if self.follower.is_none() {
                    self.follower = Some(DefaultFollower::register(
                        enumerator.clone(),
                        data_flow(*flow),
                    )?);
                }
                if let Some(follower) = &self.follower {
                    // Reset before querying the default: a change after this
                    // point re-signals.
                    // SAFETY: valid event handle owned by the follower.
                    let _ = unsafe { ResetEvent(follower.event.0) };
                }
                // SAFETY: valid enumerator.
                unsafe { enumerator.GetDefaultAudioEndpoint(data_flow(*flow), eConsole) }
                    .map_err(|e| map_os_error("GetDefaultAudioEndpoint", &e))?
            }
            Target::Id(id) => {
                // SAFETY: valid enumerator; HSTRING is NUL-terminated.
                let device =
                    unsafe { enumerator.GetDevice(&HSTRING::from(id.as_str())) }.map_err(|e| {
                        // Any failure to resolve an explicit id means "not there".
                        AudioError::SourceUnavailable(format!(
                            "IMMDeviceEnumerator::GetDevice: {e}"
                        ))
                    })?;
                // SAFETY: valid device.
                let state = unsafe { device.GetState() }
                    .map_err(|e| map_os_error("IMMDevice::GetState", &e))?;
                if state != DEVICE_STATE_ACTIVE {
                    return Err(AudioError::SourceUnavailable(format!(
                        "device {id} is not active"
                    )));
                }
                device
            }
        };
        let mut stream = activate_stream(&device, channels, self.loopback)?;
        if let Some(follower) = &self.follower {
            stream.reopen = Some(follower.event.duplicate()?);
        }
        Ok(stream)
    }
}

/// Loopback of the default render device; follows default-device changes.
pub(crate) fn loopback_source() -> Box<dyn StreamSource> {
    Box::new(EndpointSource::new(
        Target::Default(DeviceFlow::Render),
        true,
    ))
}

pub(crate) fn selected_loopback_source(device: Option<&str>) -> Box<dyn StreamSource> {
    match device {
        Some(id) => Box::new(EndpointSource::new(Target::Id(id.to_owned()), true)),
        None => loopback_source(),
    }
}

/// Microphone capture; `Default` follows default-device changes.
pub(crate) fn microphone_source(choice: MicrophoneChoice) -> Box<dyn StreamSource> {
    let target = match choice {
        MicrophoneChoice::Default => Target::Default(DeviceFlow::Capture),
        MicrophoneChoice::Device(id) => Target::Id(id),
    };
    Box::new(EndpointSource::new(target, false))
}

/// Channel count to request for a microphone: its mix format channels,
/// clamped to 1..=2 (2 if unknown).
pub(crate) fn microphone_channels(choice: &MicrophoneChoice) -> u16 {
    let _com = ComGuard::init();
    let channels = (|| {
        let enumerator = create_enumerator().ok()?;
        // SAFETY: valid enumerator.
        let device = unsafe {
            match choice {
                MicrophoneChoice::Default => enumerator.GetDefaultAudioEndpoint(eCapture, eConsole),
                MicrophoneChoice::Device(id) => enumerator.GetDevice(&HSTRING::from(id.as_str())),
            }
        }
        .ok()?;
        mix_format(&device).map(|(channels, _)| channels)
    })();
    channels.map_or(2, |c| c.clamp(1, 2))
}
