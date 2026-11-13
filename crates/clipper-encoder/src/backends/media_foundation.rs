//! Media Foundation hardware H.264 encoder writing MP4.
//!
//! GPU path: capture textures are copied (GPU to GPU) into a small pool of
//! DXGI-backed samples that the Sink Writer feeds to the hardware encoder MFT
//! (BGRA to NV12 conversion and scaling run on the GPU video processor).
#![allow(unsafe_code)]

use std::mem::ManuallyDrop;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use clipper_core::recording::QualityProfile;
use clipper_platform::windows::{D3DContext, WinError};
use windows::Win32::Foundation::{E_NOTIMPL, E_POINTER, VARIANT_BOOL};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_BOX, D3D11_TEXTURE2D_DESC,
    D3D11_USAGE_DEFAULT, ID3D11RenderTargetView, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::Variant::{
    VARIANT, VARIANT_0, VARIANT_0_0, VARIANT_0_0_0, VT_BOOL, VT_UI4,
};
use windows::core::{GUID, Interface, PCWSTR, PWSTR};
use windows_core::{Ref, implement};

use crate::codecs::{H264Profile, H264Tuning, RateControl};

#[derive(Debug)]
pub enum EncoderError {
    /// No hardware H.264 encoder MFT is available, or Media Foundation picked
    /// a software encoder (rejected: no silent CPU fallback).
    NoHardwareEncoder,
    /// The encoder rejected the requested format (resolution, fps, profile).
    UnsupportedFormat(String),
    DiskFull,
    AccessDenied,
    Windows(WinError),
}

impl std::fmt::Display for EncoderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoHardwareEncoder => f.write_str("no hardware H.264 encoder is available"),
            Self::UnsupportedFormat(detail) => {
                write!(f, "the hardware encoder does not support {detail}")
            }
            Self::DiskFull => f.write_str("the output disk is full"),
            Self::AccessDenied => f.write_str("no permission to write the output file"),
            Self::Windows(error) => write!(f, "encoder failed: {error}"),
        }
    }
}

impl std::error::Error for EncoderError {}

/// Number of pooled input textures/samples. Bounds GPU memory and latency.
const POOL_SIZE: usize = 4;

/// Private sample attribute carrying the pool slot id.
const SLOT_ID_KEY: GUID = GUID::from_u128(0x6f1c_3a52_9b7e_4d0a_8c11_5e2d_7a90_b4c3);

fn code_is(error: &WinError, code: u32) -> bool {
    error.code().0 as u32 == code
}

/// Maps disk/permission failures; everything else stays a Windows error.
fn map_error(error: WinError) -> EncoderError {
    // HRESULT_FROM_WIN32(ERROR_DISK_FULL) / (ERROR_HANDLE_DISK_FULL)
    if code_is(&error, 0x8007_0070) || code_is(&error, 0x8007_0027) {
        EncoderError::DiskFull
    } else if code_is(&error, 0x8007_0005) {
        EncoderError::AccessDenied
    } else {
        EncoderError::Windows(error)
    }
}

/// Like [`map_error`], but format negotiation failures become
/// `UnsupportedFormat(detail)`.
fn map_format_error(error: WinError, detail: &str) -> EncoderError {
    let format_failure = [
        MF_E_INVALIDMEDIATYPE,
        MF_E_UNSUPPORTED_D3D_TYPE,
        MF_E_TOPO_CODEC_NOT_FOUND,
        MF_E_INVALIDTYPE,
        MF_E_TRANSFORM_TYPE_NOT_SET,
        MF_E_UNSUPPORTED_FORMAT,
    ]
    .contains(&error.code());
    if format_failure {
        EncoderError::UnsupportedFormat(detail.to_owned())
    } else {
        map_error(error)
    }
}

/// Keeps Media Foundation started (MFStartup/MFShutdown) for its lifetime.
/// The calling thread must already be in a COM apartment (MTA).
pub struct MediaFoundation {
    _not_send: std::marker::PhantomData<*const ()>,
}

impl MediaFoundation {
    pub fn start() -> Result<Self, EncoderError> {
        // SAFETY: plain FFI call; balanced by MFShutdown in Drop on success.
        unsafe { MFStartup(MF_VERSION, MFSTARTUP_FULL) }.map_err(EncoderError::Windows)?;
        Ok(Self {
            _not_send: std::marker::PhantomData,
        })
    }
}

impl Drop for MediaFoundation {
    fn drop(&mut self) {
        // SAFETY: paired with the successful MFStartup in `start`.
        let _ = unsafe { MFShutdown() };
    }
}

fn allocated_string(attributes: &IMFAttributes, key: &GUID) -> Option<String> {
    let mut buffer = PWSTR::null();
    let mut length = 0u32;
    // SAFETY: out-pointers reference live locals; on success the returned
    // buffer is CoTaskMem-allocated and freed below after copying.
    unsafe { attributes.GetAllocatedString(key, &mut buffer, &mut length) }.ok()?;
    // SAFETY: GetAllocatedString returned a valid NUL-terminated string.
    let text = unsafe { buffer.to_string() }.ok();
    // SAFETY: buffer was allocated by CoTaskMemAlloc inside Media Foundation.
    unsafe { CoTaskMemFree(Some(buffer.0 as *const _)) };
    text
}

fn has_attribute(attributes: &IMFAttributes, key: &GUID) -> bool {
    // SAFETY: valid interface and key pointer.
    unsafe { attributes.GetItemType(key) }.is_ok()
}

/// Friendly names of hardware H.264 encoder MFTs on this system.
pub fn hardware_h264_encoders(_mf: &MediaFoundation) -> Result<Vec<String>, EncoderError> {
    let output = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_H264,
    };
    let mut activates: *mut Option<IMFActivate> = std::ptr::null_mut();
    let mut count = 0u32;
    // SAFETY: out-pointers reference live locals; `output` outlives the call.
    unsafe {
        MFTEnumEx(
            MFT_CATEGORY_VIDEO_ENCODER,
            MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER,
            None,
            Some(&output),
            &mut activates,
            &mut count,
        )
    }
    .map_err(EncoderError::Windows)?;
    if activates.is_null() || count == 0 {
        return Ok(Vec::new());
    }
    // SAFETY: MFTEnumEx returned `count` initialized interface pointers in a
    // CoTaskMem array; we take ownership of each (releasing them on drop) and
    // free the array itself afterwards.
    let items = unsafe { std::slice::from_raw_parts_mut(activates, count as usize) };
    let names = items
        .iter_mut()
        .filter_map(Option::take)
        .map(|activate| {
            let attributes: &IMFAttributes = &activate;
            allocated_string(attributes, &MFT_FRIENDLY_NAME_Attribute)
                .unwrap_or_else(|| "Hardware H.264 encoder".to_owned())
        })
        .collect();
    // SAFETY: array allocated by CoTaskMemAlloc; all elements were taken.
    unsafe { CoTaskMemFree(Some(activates as *const _)) };
    Ok(names)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncoderConfig {
    /// Size of textures passed to `submit` (capture size at start, rounded
    /// up to even). Later frames of another size are cropped/padded.
    pub input_width: u32,
    pub input_height: u32,
    /// Encoded size; scaling happens on the GPU.
    pub output_width: u32,
    pub output_height: u32,
    pub fps: u32,
    pub bitrate_mbps: u32,
    pub profile: QualityProfile,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubmitOutcome {
    Submitted,
    /// All pooled input textures are still in use by the encoder; the frame
    /// was not encoded (backpressure).
    Dropped,
}

/// One pooled sample, travelling between the free list and Media Foundation.
struct Slot {
    id: usize,
    sample: IMFSample,
}

// SAFETY: Media Foundation samples are free-threaded COM objects (they are
// handed between MF worker threads by design); windows-rs only marks
// interfaces !Send conservatively.
unsafe impl Send for Slot {}

struct Shared {
    free: Mutex<Vec<Slot>>,
    closed: AtomicBool,
}

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Slot>> {
        self.free
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Receives tracked samples back from Media Foundation once the encoder has
/// released them, and returns them to the free list.
#[implement(IMFAsyncCallback)]
struct ReturnCallback {
    shared: Arc<Shared>,
}

impl IMFAsyncCallback_Impl for ReturnCallback_Impl {
    fn GetParameters(&self, _flags: *mut u32, _queue: *mut u32) -> windows_core::Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn Invoke(&self, result: Ref<'_, IMFAsyncResult>) -> windows_core::Result<()> {
        let result = result.ok()?;
        // SAFETY: valid async result delivered by Media Foundation.
        let object = unsafe { result.GetObject() }?;
        let sample: IMFSample = object.cast()?;
        // SAFETY: valid sample; the attribute was set by `build_sample`.
        let id = unsafe { sample.GetUINT32(&SLOT_ID_KEY) }? as usize;
        if self.shared.closed.load(Ordering::Acquire) {
            // Encoder is gone: let the sample die instead of resurrecting a
            // reference cycle through the free list.
            return Ok(());
        }
        self.shared.lock().push(Slot { id, sample });
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum Property {
    U32(u32),
    Bool(bool),
}

impl Property {
    fn scalar(self) -> u32 {
        match self {
            Self::U32(value) => value,
            Self::Bool(value) => u32::from(value),
        }
    }

    fn variant(self) -> VARIANT {
        let (vt, value) = match self {
            Self::U32(value) => (VT_UI4, VARIANT_0_0_0 { ulVal: value }),
            Self::Bool(value) => (
                VT_BOOL,
                VARIANT_0_0_0 {
                    boolVal: VARIANT_BOOL::from(value),
                },
            ),
        };
        VARIANT {
            Anonymous: VARIANT_0 {
                Anonymous: ManuallyDrop::new(VARIANT_0_0 {
                    vt,
                    wReserved1: 0,
                    wReserved2: 0,
                    wReserved3: 0,
                    Anonymous: value,
                }),
            },
        }
    }
}

/// CODECAPI properties for the encoder, from the platform-independent tuning.
fn codec_properties(tuning: &H264Tuning) -> Vec<(GUID, Property)> {
    let mode = match tuning.rate_control {
        RateControl::Cbr => eAVEncCommonRateControlMode_CBR,
        RateControl::PeakConstrainedVbr => eAVEncCommonRateControlMode_PeakConstrainedVBR,
    };
    let mut properties = vec![
        (
            CODECAPI_AVEncCommonRateControlMode,
            Property::U32(mode.0 as u32),
        ),
        (
            CODECAPI_AVEncCommonMeanBitRate,
            Property::U32(tuning.bitrate_bps),
        ),
    ];
    if tuning.rate_control == RateControl::PeakConstrainedVbr {
        properties.push((
            CODECAPI_AVEncCommonMaxBitRate,
            Property::U32(tuning.peak_bitrate_bps),
        ));
    }
    properties.extend([
        (
            CODECAPI_AVEncCommonQualityVsSpeed,
            Property::U32(tuning.quality_vs_speed),
        ),
        (
            CODECAPI_AVLowLatencyMode,
            Property::Bool(tuning.low_latency),
        ),
        (CODECAPI_AVEncMPVGOPSize, Property::U32(tuning.gop_frames)),
        (
            CODECAPI_AVEncMPVDefaultBPictureCount,
            Property::U32(tuning.b_frames),
        ),
    ]);
    properties
}

fn pack(hi: u32, lo: u32) -> u64 {
    (u64::from(hi) << 32) | u64::from(lo)
}

fn video_type(
    subtype: &GUID,
    (width, height): (u32, u32),
    fps: u32,
) -> Result<IMFMediaType, WinError> {
    // SAFETY: plain FFI calls on a freshly created media type.
    unsafe {
        let media_type = MFCreateMediaType()?;
        media_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        media_type.SetGUID(&MF_MT_SUBTYPE, subtype)?;
        media_type.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        media_type.SetUINT64(&MF_MT_FRAME_SIZE, pack(width, height))?;
        media_type.SetUINT64(&MF_MT_FRAME_RATE, pack(fps, 1))?;
        media_type.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))?;
        Ok(media_type)
    }
}

/// Finds the encoder MFT the sink writer instantiated for `stream`.
fn find_encoder(writer: &IMFSinkWriter, stream: u32) -> Option<IMFTransform> {
    let ex: IMFSinkWriterEx = writer.cast().ok()?;
    for index in 0u32.. {
        let mut category = GUID::zeroed();
        let mut transform: Option<IMFTransform> = None;
        // SAFETY: out-pointers reference live locals.
        let result =
            unsafe { ex.GetTransformForStream(stream, index, Some(&mut category), &mut transform) };
        match result {
            Ok(()) if category == MFT_CATEGORY_VIDEO_ENCODER => return transform,
            Ok(()) => {}
            // MF_E_INVALIDINDEX marks the end of the list.
            Err(_) => return None,
        }
    }
    None
}

/// Applies each property the encoder supports; individual rejections are
/// ignored so only options the real encoder understands take effect.
fn apply_codec_properties(transform: &IMFTransform, properties: &[(GUID, Property)]) {
    let Ok(api) = transform.cast::<ICodecAPI>() else {
        return;
    };
    for (key, property) in properties {
        let value = property.variant();
        // SAFETY: valid ICodecAPI; IsSupported returns S_OK when supported
        // and S_FALSE when not, which windows-rs' Result would conflate, so
        // the raw HRESULT is checked. `value` is a live scalar VARIANT.
        unsafe {
            let supported = (Interface::vtable(&api).IsSupported)(Interface::as_raw(&api), key);
            if supported.0 == 0 {
                let _ = api.SetValue(key, &value);
            }
        }
    }
}

struct Pool {
    textures: Vec<ID3D11Texture2D>,
    shared: Arc<Shared>,
    callback: IMFAsyncCallback,
}

fn create_pool(d3d: &D3DContext, (width, height): (u32, u32)) -> Result<Pool, EncoderError> {
    let shared = Arc::new(Shared {
        free: Mutex::new(Vec::with_capacity(POOL_SIZE)),
        closed: AtomicBool::new(false),
    });
    let callback: IMFAsyncCallback = ReturnCallback {
        shared: Arc::clone(&shared),
    }
    .into();
    let desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let mut textures = Vec::with_capacity(POOL_SIZE);
    for id in 0..POOL_SIZE {
        let mut texture: Option<ID3D11Texture2D> = None;
        // SAFETY: valid device, descriptor and out-pointer.
        unsafe {
            d3d.device()
                .CreateTexture2D(&desc, None, Some(&mut texture))
        }
        .map_err(EncoderError::Windows)?;
        let texture = texture.ok_or_else(|| EncoderError::Windows(E_POINTER.into()))?;

        // Start black so padding around smaller frames is not garbage.
        let mut view: Option<ID3D11RenderTargetView> = None;
        // SAFETY: texture has BIND_RENDER_TARGET; out-pointer is valid.
        unsafe {
            d3d.device()
                .CreateRenderTargetView(&texture, None, Some(&mut view))
        }
        .map_err(EncoderError::Windows)?;
        if let Some(view) = view {
            // SAFETY: valid view; the context is multithread protected.
            unsafe {
                d3d.context()
                    .ClearRenderTargetView(&view, &[0.0, 0.0, 0.0, 1.0]);
            }
        }

        let sample = build_sample(&texture, id).map_err(EncoderError::Windows)?;
        shared.lock().push(Slot { id, sample });
        textures.push(texture);
    }
    Ok(Pool {
        textures,
        shared,
        callback,
    })
}

fn build_sample(texture: &ID3D11Texture2D, id: usize) -> Result<IMFSample, WinError> {
    // SAFETY: plain FFI on live objects created here.
    unsafe {
        let tracked = MFCreateTrackedSample()?;
        let sample: IMFSample = tracked.cast()?;
        let buffer = MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, texture, 0, false)?;
        let length = buffer.cast::<IMF2DBuffer>()?.GetContiguousLength()?;
        buffer.SetCurrentLength(length)?;
        sample.AddBuffer(&buffer)?;
        sample.SetUINT32(&SLOT_ID_KEY, id as u32)?;
        Ok(sample)
    }
}

pub struct HardwareEncoder {
    writer: Option<IMFSinkWriter>,
    stream: u32,
    d3d: D3DContext,
    input: (u32, u32),
    name: String,
    pool: Pool,
    // Keeps the DXGI device manager alive as long as the writer.
    _manager: IMFDXGIDeviceManager,
}

impl HardwareEncoder {
    pub fn create(
        mf: &MediaFoundation,
        d3d: &D3DContext,
        config: &EncoderConfig,
        path: &Path,
    ) -> Result<Self, EncoderError> {
        let encoders = hardware_h264_encoders(mf)?;
        let Some(listed_name) = encoders.into_iter().next() else {
            return Err(EncoderError::NoHardwareEncoder);
        };
        let detail = format!(
            "{}x{}@{}",
            config.output_width, config.output_height, config.fps
        );
        let dimensions = [
            config.input_width,
            config.input_height,
            config.output_width,
            config.output_height,
            config.fps,
        ];
        if dimensions.contains(&0)
            || !config.output_width.is_multiple_of(2)
            || !config.output_height.is_multiple_of(2)
        {
            return Err(EncoderError::UnsupportedFormat(detail));
        }

        match Self::create_inner(d3d, config, path, &detail, listed_name) {
            Ok(encoder) => Ok(encoder),
            Err(error) => {
                let _ = std::fs::remove_file(path);
                Err(error)
            }
        }
    }

    fn create_inner(
        d3d: &D3DContext,
        config: &EncoderConfig,
        path: &Path,
        detail: &str,
        listed_name: String,
    ) -> Result<Self, EncoderError> {
        let tuning = H264Tuning::new(config.profile, config.bitrate_mbps, config.fps);
        let win = EncoderError::Windows;

        let mut token = 0u32;
        let mut manager: Option<IMFDXGIDeviceManager> = None;
        // SAFETY: out-pointers reference live locals; the device is a valid
        // D3D11 device that outlives the manager (the encoder holds a clone).
        let manager = unsafe {
            MFCreateDXGIDeviceManager(&mut token, &mut manager).map_err(win)?;
            let manager = manager.ok_or_else(|| win(E_POINTER.into()))?;
            manager.ResetDevice(d3d.device(), token).map_err(win)?;
            manager
        };

        let mut attributes: Option<IMFAttributes> = None;
        // SAFETY: out-pointer is valid; attributes are set on the fresh object.
        let attributes = unsafe {
            MFCreateAttributes(&mut attributes, 4).map_err(win)?;
            let attributes = attributes.ok_or_else(|| win(E_POINTER.into()))?;
            attributes
                .SetUnknown(&MF_SINK_WRITER_D3D_MANAGER, &manager)
                .map_err(win)?;
            attributes
                .SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1)
                .map_err(win)?;
            // The caller writes to `*.mp4.partial`; do not infer from it.
            attributes
                .SetGUID(&MF_TRANSCODE_CONTAINERTYPE, &MFTranscodeContainerType_MPEG4)
                .map_err(win)?;
            // Keep throttling on: natural backpressure from the encoder.
            attributes
                .SetUINT32(&MF_SINK_WRITER_DISABLE_THROTTLING, 0)
                .map_err(win)?;
            attributes
        };

        let wide: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        // SAFETY: `wide` is NUL-terminated and outlives the call.
        let writer = unsafe {
            MFCreateSinkWriterFromURL(PCWSTR(wide.as_ptr()), None::<&IMFByteStream>, &attributes)
        }
        .map_err(map_error)?;

        let output_type = video_type(
            &MFVideoFormat_H264,
            (config.output_width, config.output_height),
            config.fps,
        )
        .map_err(win)?;
        let profile = match tuning.profile {
            H264Profile::Main => eAVEncH264VProfile_Main,
            H264Profile::High => eAVEncH264VProfile_High,
        };
        // SAFETY: valid media type and writer.
        let stream = unsafe {
            output_type
                .SetUINT32(&MF_MT_AVG_BITRATE, tuning.bitrate_bps)
                .map_err(win)?;
            output_type
                .SetUINT32(&MF_MT_MPEG2_PROFILE, profile.0 as u32)
                .map_err(win)?;
            writer
                .AddStream(&output_type)
                .map_err(|e| map_format_error(e, detail))?
        };

        let input_type = video_type(
            &MFVideoFormat_ARGB32,
            (config.input_width, config.input_height),
            config.fps,
        )
        .map_err(win)?;
        let properties = codec_properties(&tuning);
        let mut encoding: Option<IMFAttributes> = None;
        // SAFETY: out-pointer valid; values are scalar attributes.
        let encoding = unsafe {
            MFCreateAttributes(&mut encoding, 8).map_err(win)?;
            let encoding = encoding.ok_or_else(|| win(E_POINTER.into()))?;
            for (key, property) in &properties {
                let _ = encoding.SetUINT32(key, property.scalar());
            }
            encoding
        };
        // SAFETY: valid writer, media type and attributes.
        let first = unsafe { writer.SetInputMediaType(stream, &input_type, &encoding) };
        if first.is_err() {
            // Some encoders reject unknown parameters up front; retry plain
            // and rely on ICodecAPI below.
            // SAFETY: as above, without encoding parameters.
            unsafe { writer.SetInputMediaType(stream, &input_type, None::<&IMFAttributes>) }
                .map_err(|e| map_format_error(e, detail))?;
        }

        let transform = find_encoder(&writer, stream);
        let hardware = transform.as_ref().is_some_and(|transform| {
            // SAFETY: valid transform.
            unsafe { transform.GetAttributes() }.is_ok_and(|attrs| {
                has_attribute(&attrs, &MFT_ENUM_HARDWARE_URL_Attribute)
                    || has_attribute(&attrs, &MFT_ENUM_HARDWARE_VENDOR_ID_Attribute)
            })
        });
        let Some(transform) = transform.filter(|_| hardware) else {
            return Err(EncoderError::NoHardwareEncoder);
        };
        apply_codec_properties(&transform, &properties);
        // SAFETY: valid transform.
        let name = unsafe { transform.GetAttributes() }
            .ok()
            .and_then(|attrs| allocated_string(&attrs, &MFT_FRIENDLY_NAME_Attribute))
            .unwrap_or(listed_name);

        let input = (config.input_width, config.input_height);
        let pool = create_pool(d3d, input)?;

        // SAFETY: fully configured writer.
        unsafe { writer.BeginWriting() }.map_err(map_error)?;

        Ok(Self {
            writer: Some(writer),
            stream,
            d3d: d3d.clone(),
            input,
            name,
            pool,
            _manager: manager,
        })
    }

    /// Friendly name of the hardware encoder in use.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Copies `texture` on the GPU into a pooled input texture and submits it.
    /// `sample_time` and `duration` are in 100 ns ticks, zero-based.
    pub fn submit(
        &mut self,
        texture: &ID3D11Texture2D,
        content_size: (u32, u32),
        sample_time: i64,
        duration: i64,
    ) -> Result<SubmitOutcome, EncoderError> {
        let Some(writer) = self.writer.as_ref() else {
            return Err(EncoderError::Windows(E_POINTER.into()));
        };
        // Lock released immediately; never held while calling into MF.
        let popped = self.pool.shared.lock().pop();
        let Some(slot) = popped else {
            return Ok(SubmitOutcome::Dropped);
        };
        let Some(target) = self.pool.textures.get(slot.id) else {
            return Ok(SubmitOutcome::Dropped);
        };

        let region = D3D11_BOX {
            left: 0,
            top: 0,
            front: 0,
            right: content_size.0.min(self.input.0),
            bottom: content_size.1.min(self.input.1),
            back: 1,
        };
        // SAFETY: both textures are BGRA on this device; the box lies inside
        // the destination and, by contract, inside the source (content size).
        unsafe {
            self.d3d
                .context()
                .CopySubresourceRegion(target, 0, 0, 0, 0, texture, 0, Some(&region));
        }

        let sample = slot.sample;
        let stream = self.stream;
        let callback = &self.pool.callback;
        // SAFETY: valid sample/writer. SetAllocator must precede every write
        // so the pool gets the sample back after MF releases it. Our own
        // reference is dropped right after WriteSample, so Media Foundation's
        // release is the last one. If the write fails, that drop returns the
        // sample through the callback as well.
        let result = unsafe {
            (|| -> Result<(), WinError> {
                sample.SetSampleTime(sample_time)?;
                sample.SetSampleDuration(duration)?;
                let tracked: IMFTrackedSample = sample.cast()?;
                tracked.SetAllocator(callback, None::<&windows_core::IUnknown>)?;
                writer.WriteSample(stream, &sample)
            })()
        };
        drop(sample);
        result.map_err(map_error)?;
        Ok(SubmitOutcome::Submitted)
    }

    /// Drains the encoder and finalizes the MP4. Only a successful return
    /// means the file is complete and playable.
    pub fn finish(mut self) -> Result<(), EncoderError> {
        let Some(writer) = self.writer.take() else {
            return Err(EncoderError::Windows(E_POINTER.into()));
        };
        // SAFETY: valid writer after BeginWriting; Finalize blocks until the
        // encoder is drained and the MP4 index is written.
        let result = unsafe { writer.Finalize() }.map_err(map_error);
        drop(writer);
        result
    }
}

impl Drop for HardwareEncoder {
    fn drop(&mut self) {
        // Without `finish` the file stays incomplete; the caller deletes it.
        self.writer = None;
        self.pool.shared.closed.store(true, Ordering::Release);
        let slots: Vec<Slot> = self.pool.shared.lock().drain(..).collect();
        for slot in slots {
            // SAFETY: valid tracked sample; clearing the allocator lets it
            // be freed instead of returning to the (closed) pool.
            unsafe {
                if let Ok(tracked) = slot.sample.cast::<IMFTrackedSample>() {
                    let _ = tracked
                        .SetAllocator(None::<&IMFAsyncCallback>, None::<&windows_core::IUnknown>);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clipper_platform::windows::ComApartment;
    use windows::Win32::Graphics::Direct3D11::ID3D11RenderTargetView;

    struct Env {
        _com: ComApartment,
        mf: MediaFoundation,
        d3d: D3DContext,
    }

    fn env() -> Option<Env> {
        let com = ComApartment::init_multithreaded().ok()?;
        let mf = MediaFoundation::start().ok()?;
        let d3d = D3DContext::new().ok()?;
        Some(Env { _com: com, mf, d3d })
    }

    fn config(width: u32, height: u32) -> EncoderConfig {
        EncoderConfig {
            input_width: width,
            input_height: height,
            output_width: width,
            output_height: height,
            fps: 30,
            bitrate_mbps: 8,
            profile: QualityProfile::Balanced,
        }
    }

    fn source_texture(
        d3d: &D3DContext,
        width: u32,
        height: u32,
    ) -> (ID3D11Texture2D, ID3D11RenderTargetView) {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let mut texture = None;
        let mut view = None;
        // SAFETY: valid device, descriptor and out-pointers.
        unsafe {
            d3d.device()
                .CreateTexture2D(&desc, None, Some(&mut texture))
                .expect("texture");
            let texture_ref = texture.as_ref().expect("texture");
            d3d.device()
                .CreateRenderTargetView(texture_ref, None, Some(&mut view))
                .expect("rtv");
        }
        (texture.expect("texture"), view.expect("rtv"))
    }

    fn fill(d3d: &D3DContext, view: &ID3D11RenderTargetView, step: u32) {
        let t = (step % 60) as f32 / 60.0;
        // SAFETY: valid view on the shared device.
        unsafe {
            d3d.context()
                .ClearRenderTargetView(view, &[t, 1.0 - t, 0.5, 1.0]);
        }
    }

    fn temp_mp4(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("clipper-test-{}-{name}.mp4", std::process::id()))
    }

    #[test]
    fn lists_hardware_encoders() {
        let Some(env) = env() else { return };
        let list = hardware_h264_encoders(&env.mf).expect("enumerate");
        println!("hardware H.264 encoders: {list:?}");
    }

    #[test]
    fn encodes_playable_mp4() {
        let Some(env) = env() else { return };
        let path = temp_mp4("roundtrip");
        let listed = hardware_h264_encoders(&env.mf).expect("enumerate");
        let result = HardwareEncoder::create(&env.mf, &env.d3d, &config(1280, 720), &path);
        if listed.is_empty() {
            assert!(matches!(result, Err(EncoderError::NoHardwareEncoder)));
            return;
        }
        let mut encoder = result.expect("create encoder");
        println!("encoder in use: {}", encoder.name());
        let (texture, view) = source_texture(&env.d3d, 1280, 720);
        let mut submitted = 0;
        for i in 0..60u32 {
            fill(&env.d3d, &view, i);
            std::thread::sleep(std::time::Duration::from_millis(16));
            let outcome = encoder
                .submit(&texture, (1280, 720), i64::from(i) * 333_333, 333_333)
                .expect("submit");
            if outcome == SubmitOutcome::Submitted {
                submitted += 1;
            }
        }
        println!("submitted {submitted}/60");
        assert!(submitted >= 50, "paced submission must not drop");
        encoder.finish().expect("finish");

        let (width, height, duration, subtype) = probe(&path);
        println!("read back: {width}x{height}, {duration} (100ns), subtype H264={subtype}");
        let _ = std::fs::remove_file(&path);
        assert_eq!((width, height), (1280, 720));
        assert!(subtype);
        assert!(duration >= 15_000_000, "about 2 s expected");
    }

    fn probe(path: &Path) -> (u32, u32, u64, bool) {
        let wide: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        // SAFETY: NUL-terminated path outlives the calls; interfaces are valid.
        unsafe {
            let reader = MFCreateSourceReaderFromURL(PCWSTR(wide.as_ptr()), None::<&IMFAttributes>)
                .expect("open mp4");
            let duration_var = reader
                .GetPresentationAttribute(MF_SOURCE_READER_MEDIASOURCE.0 as u32, &MF_PD_DURATION)
                .expect("duration");
            let duration = u64::try_from(&duration_var).expect("u64 duration");
            let native = reader
                .GetNativeMediaType(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32, 0)
                .expect("native type");
            let size = native.GetUINT64(&MF_MT_FRAME_SIZE).expect("size");
            let subtype = native.GetGUID(&MF_MT_SUBTYPE).expect("subtype");
            (
                (size >> 32) as u32,
                size as u32,
                duration,
                subtype == MFVideoFormat_H264,
            )
        }
    }

    #[test]
    fn unwritable_path_fails() {
        let Some(env) = env() else { return };
        let missing = std::env::temp_dir()
            .join("clipper-no-such-dir-xyz")
            .join("out.mp4");
        let denied = Path::new(r"C:\Windows\System32\clipper-test.mp4");
        for path in [missing.as_path(), denied] {
            let result = HardwareEncoder::create(&env.mf, &env.d3d, &config(1280, 720), path);
            assert!(result.is_err(), "{path:?} must not succeed");
            println!(
                "{path:?}: {}",
                result.err().map(|e| e.to_string()).unwrap_or_default()
            );
            assert!(!path.exists());
        }
    }

    #[test]
    fn fast_submission_terminates_and_finishes() {
        let Some(env) = env() else { return };
        if hardware_h264_encoders(&env.mf).map_or(true, |l| l.is_empty()) {
            return;
        }
        let path = temp_mp4("burst");
        let mut encoder =
            HardwareEncoder::create(&env.mf, &env.d3d, &config(1920, 1080), &path).expect("create");
        let (texture, view) = source_texture(&env.d3d, 1920, 1080);
        let (mut submitted, mut dropped) = (0, 0);
        for i in 0..240u32 {
            fill(&env.d3d, &view, i);
            if i >= 120 {
                // Real-time pacing after the burst: the pool must recover.
                std::thread::sleep(std::time::Duration::from_millis(16));
            }
            match encoder
                .submit(&texture, (1920, 1080), i64::from(i) * 333_333, 333_333)
                .expect("submit")
            {
                SubmitOutcome::Submitted => submitted += 1,
                SubmitOutcome::Dropped => dropped += 1,
            }
        }
        println!("burst: submitted={submitted} dropped={dropped}");
        assert!(submitted > 0 && submitted + dropped == 240);
        encoder.finish().expect("finish");
        let _ = std::fs::remove_file(&path);
    }
}
