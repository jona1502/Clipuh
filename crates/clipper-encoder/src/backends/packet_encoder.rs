//! Hardware packet encoder for the instant replay buffer.
//!
//! Unlike [`super::media_foundation::HardwareEncoder`] there is no sink
//! writer and no file: the encoder hands out [`EncodedPacket`]s (H.264 AVCC
//! on track 0, raw AAC-LC access units on tracks 1..) on the session timeline
//! (100 ns ticks) as defined by `clipper-replay`.
//!
//! Video path: capture texture -> GPU copy into one BGRA staging texture ->
//! D3D11 video processor (BGRA to NV12, scaling) into a pooled NV12 texture
//! -> DXGI surface sample -> asynchronous hardware H.264 MFT.
//!
//! Async MFT protocol: `METransformNeedInput` events are counted as input
//! credits and `ProcessInput` is only called while a credit exists (at most
//! [`MAX_PENDING`] frames wait for one, otherwise the frame is dropped).
//! `METransformHaveOutput` triggers `ProcessOutput`. Events are drained with
//! `MF_EVENT_FLAG_NO_WAIT` from `submit`, `poll` and `finish`; nothing blocks.
//!
//! Audio path: one synchronous Microsoft AAC encoder MFT per track.
//!
//! Audio track labels: track 1 is "Mix", then "Desktop", "Microphone" and
//! "Audio N" (see [`default_label`]); use [`PacketEncoder::create_with_labels`]
//! to name them explicitly.
#![allow(unsafe_code)]

use std::cmp::Reverse;
use std::collections::{BinaryHeap, VecDeque};
use std::mem::ManuallyDrop;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use clipper_platform::windows::{D3DContext, WinError};
use clipper_replay::h264::{annexb_to_avcc, nal_units};
use clipper_replay::{CodecConfig, EncodedPacket, StreamLayout, TICKS_PER_SECOND, TrackInfo};
use windows::Win32::Foundation::{E_POINTER, RECT};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_NV12, DXGI_RATIONAL, DXGI_SAMPLE_DESC,
};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree};
use windows::core::{GUID, Interface};
use windows_core::BOOL;

use super::media_foundation::{
    EncoderConfig, EncoderError, MediaFoundation, ReturnCallback, Shared, Slot, SubmitOutcome,
    apply_codec_properties, build_sample, codec_properties, hardware_h264_encoders, has_attribute,
    map_error, map_format_error, video_type,
};
use crate::codecs::{H264Profile, H264Tuning};

/// Pooled NV12 textures (bounds GPU memory; in flight inside the encoder).
const NV12_POOL: usize = 6;
/// Frames that may wait for an input credit before `submit` reports Dropped.
const MAX_PENDING: usize = 2;
/// Bounded wait for the encoder to drain in `finish`.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(2);
/// Audio packets held back until the layout is known (about 40 s).
const MAX_HELD_AUDIO: usize = 2000;
const AAC_FRAME_SAMPLES: i64 = 1024;
const SAMPLE_RATE: i64 = 48_000;
/// Duration of one AAC access unit in ticks (1024 / 48000 s).
const AAC_FRAME_TICKS: i64 = AAC_FRAME_SAMPLES * TICKS_PER_SECOND / SAMPLE_RATE;
const MAX_AUDIO_TRACKS: usize = 5;

/// Default label of audio track `index` (0-based among audio tracks).
pub fn default_label(index: usize) -> String {
    match index {
        0 => "Mix".to_owned(),
        1 => "Desktop".to_owned(),
        2 => "Microphone".to_owned(),
        n => format!("Audio {}", n + 1),
    }
}

fn win(error: WinError) -> EncoderError {
    EncoderError::Windows(error)
}

fn no_pointer() -> EncoderError {
    EncoderError::Windows(E_POINTER.into())
}

fn code_is(error: &WinError, code: windows_core::HRESULT) -> bool {
    error.code() == code
}

// ---------------------------------------------------------------------------
// Video processor (BGRA -> NV12)
// ---------------------------------------------------------------------------

struct Converter {
    d3d: D3DContext,
    video_context: ID3D11VideoContext,
    processor: ID3D11VideoProcessor,
    staging: ID3D11Texture2D,
    staging_view: ID3D11VideoProcessorInputView,
    output_views: Vec<ID3D11VideoProcessorOutputView>,
    input: (u32, u32),
    output: (u32, u32),
    /// Content size of the frame currently held in `staging`; `None` until
    /// the first frame was staged. The staging texture doubles as the repeat
    /// source for static scenes, so no extra copy of the last frame exists.
    staged: Option<(u32, u32)>,
    // Keep alive for the processor's lifetime.
    _enumerator: ID3D11VideoProcessorEnumerator,
    _video_device: ID3D11VideoDevice,
}

fn nv12_texture(
    d3d: &D3DContext,
    (width, height): (u32, u32),
) -> Result<ID3D11Texture2D, EncoderError> {
    let mut last = no_pointer();
    // Some drivers refuse NV12 render targets that are also shader
    // resources; the video processor only needs the render target bind.
    for flags in [
        D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0,
        D3D11_BIND_RENDER_TARGET.0,
    ] {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_NV12,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: flags as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let mut texture: Option<ID3D11Texture2D> = None;
        // SAFETY: valid device, descriptor and out-pointer.
        match unsafe {
            d3d.device()
                .CreateTexture2D(&desc, None, Some(&mut texture))
        } {
            Ok(()) => {
                if let Some(texture) = texture {
                    return Ok(texture);
                }
            }
            Err(error) => last = win(error),
        }
    }
    Err(last)
}

impl Converter {
    fn new(
        d3d: &D3DContext,
        input: (u32, u32),
        output: (u32, u32),
        fps: u32,
        textures: &[ID3D11Texture2D],
    ) -> Result<Self, EncoderError> {
        let video_device: ID3D11VideoDevice = d3d.device().cast().map_err(win)?;
        let video_context: ID3D11VideoContext = d3d.context().cast().map_err(win)?;
        let rate = DXGI_RATIONAL {
            Numerator: fps,
            Denominator: 1,
        };
        let content = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
            InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            InputFrameRate: rate,
            InputWidth: input.0,
            InputHeight: input.1,
            OutputFrameRate: rate,
            OutputWidth: output.0,
            OutputHeight: output.1,
            Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
        };
        // SAFETY: valid device and descriptor; every call below is plain FFI
        // on live objects created here, with valid out-pointers.
        let (enumerator, processor) = unsafe {
            let enumerator = video_device
                .CreateVideoProcessorEnumerator(&content)
                .map_err(win)?;
            let processor = video_device
                .CreateVideoProcessor(&enumerator, 0)
                .map_err(win)?;
            (enumerator, processor)
        };

        let staging_desc = D3D11_TEXTURE2D_DESC {
            Width: input.0,
            Height: input.1,
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
        let mut staging: Option<ID3D11Texture2D> = None;
        let input_desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
            FourCC: 0,
            ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                Texture2D: D3D11_TEX2D_VPIV {
                    MipSlice: 0,
                    ArraySlice: 0,
                },
            },
        };
        let mut staging_view: Option<ID3D11VideoProcessorInputView> = None;
        // SAFETY: valid descriptors and out-pointers; staging was just created.
        unsafe {
            d3d.device()
                .CreateTexture2D(&staging_desc, None, Some(&mut staging))
                .map_err(win)?;
            let staging_ref = staging.as_ref().ok_or_else(no_pointer)?;
            video_device
                .CreateVideoProcessorInputView(
                    staging_ref,
                    &enumerator,
                    &input_desc,
                    Some(&mut staging_view),
                )
                .map_err(win)?;
        }
        let staging = staging.ok_or_else(no_pointer)?;
        let staging_view = staging_view.ok_or_else(no_pointer)?;

        let output_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
            ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 {
                Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 },
            },
        };
        let mut output_views = Vec::with_capacity(textures.len());
        for texture in textures {
            let mut view: Option<ID3D11VideoProcessorOutputView> = None;
            // SAFETY: NV12 texture with the render target bind; valid out-pointer.
            unsafe {
                video_device
                    .CreateVideoProcessorOutputView(
                        texture,
                        &enumerator,
                        &output_desc,
                        Some(&mut view),
                    )
                    .map_err(win)?;
            }
            output_views.push(view.ok_or_else(no_pointer)?);
        }

        // SAFETY: valid context/processor. Color spaces: stream is full range
        // RGB; output is limited range (16-235) BT.709 YCbCr, matching the
        // MF_MT_YUV_MATRIX / nominal range set on the encoder input type.
        // Bit layout of D3D11_VIDEO_PROCESSOR_COLOR_SPACE: Usage:1,
        // RGB_Range:1, YCbCr_Matrix:1, YCbCr_xvYCC:1, Nominal_Range:2.
        unsafe {
            let stream_space = D3D11_VIDEO_PROCESSOR_COLOR_SPACE { _bitfield: 0 };
            let output_space = D3D11_VIDEO_PROCESSOR_COLOR_SPACE {
                _bitfield: (1 << 2) | (1 << 4),
            };
            video_context.VideoProcessorSetStreamColorSpace(&processor, 0, &stream_space);
            video_context.VideoProcessorSetOutputColorSpace(&processor, &output_space);
            let black = D3D11_VIDEO_COLOR {
                Anonymous: D3D11_VIDEO_COLOR_0 {
                    RGBA: D3D11_VIDEO_COLOR_RGBA {
                        R: 0.0,
                        G: 0.0,
                        B: 0.0,
                        A: 1.0,
                    },
                },
            };
            video_context.VideoProcessorSetOutputBackgroundColor(&processor, false, &black);
            let target = RECT {
                left: 0,
                top: 0,
                right: output.0 as i32,
                bottom: output.1 as i32,
            };
            video_context.VideoProcessorSetOutputTargetRect(&processor, true, Some(&target));
            video_context.VideoProcessorSetStreamAutoProcessingMode(&processor, 0, false);
        }

        Ok(Self {
            d3d: d3d.clone(),
            video_context,
            processor,
            staging,
            staging_view,
            output_views,
            input,
            output,
            staged: None,
            _enumerator: enumerator,
            _video_device: video_device,
        })
    }

    /// GPU-copies the content area of `texture` into the staging texture,
    /// where it stays as the input of every following [`Self::blit`].
    fn stage(
        &mut self,
        texture: &ID3D11Texture2D,
        content_size: (u32, u32),
    ) -> Result<(), EncoderError> {
        let content = (
            content_size.0.clamp(1, self.input.0),
            content_size.1.clamp(1, self.input.1),
        );
        let region = D3D11_BOX {
            left: 0,
            top: 0,
            front: 0,
            right: content.0,
            bottom: content.1,
            back: 1,
        };
        // SAFETY: both textures live on this device; the box lies inside the
        // staging texture and (by contract) inside the source. The immediate
        // context is multithread protected and orders this copy after every
        // earlier blit that still reads the staging texture.
        unsafe {
            self.d3d.context().CopySubresourceRegion(
                &self.staging,
                0,
                0,
                0,
                0,
                texture,
                0,
                Some(&region),
            );
        }
        self.staged = Some(content);
        Ok(())
    }

    /// Converts/scales the staged frame into output view `slot`. Content
    /// keeps its place in the top-left corner; the rest of the NV12 frame is
    /// background black.
    fn blit(&self, slot: usize) -> Result<(), EncoderError> {
        let content = self.staged.ok_or_else(no_pointer)?;
        let scale = |value: u32, from: u32, to: u32| -> i32 {
            let scaled = (u64::from(value) * u64::from(to)).div_ceil(u64::from(from));
            let even = (scaled + 1) & !1;
            even.clamp(2, u64::from(to)) as i32
        };
        let source = RECT {
            left: 0,
            top: 0,
            right: content.0 as i32,
            bottom: content.1 as i32,
        };
        let dest = RECT {
            left: 0,
            top: 0,
            right: scale(content.0, self.input.0, self.output.0),
            bottom: scale(content.1, self.input.1, self.output.1),
        };
        let view = self.output_views.get(slot).ok_or_else(no_pointer)?;
        let stream = D3D11_VIDEO_PROCESSOR_STREAM {
            Enable: BOOL(1),
            OutputIndex: 0,
            InputFrameOrField: 0,
            PastFrames: 0,
            FutureFrames: 0,
            ppPastSurfaces: std::ptr::null_mut(),
            pInputSurface: ManuallyDrop::new(Some(self.staging_view.clone())),
            ppFutureSurfaces: std::ptr::null_mut(),
            ppPastSurfacesRight: std::ptr::null_mut(),
            pInputSurfaceRight: ManuallyDrop::new(None),
            ppFutureSurfacesRight: std::ptr::null_mut(),
        };
        // SAFETY: valid processor and views on this device. The immediate
        // context is multithread protected. The stream's input surface was
        // cloned above and is released exactly once below.
        let result = unsafe {
            self.video_context.VideoProcessorSetStreamSourceRect(
                &self.processor,
                0,
                true,
                Some(&source),
            );
            self.video_context.VideoProcessorSetStreamDestRect(
                &self.processor,
                0,
                true,
                Some(&dest),
            );
            self.video_context.VideoProcessorBlt(
                &self.processor,
                view,
                0,
                std::slice::from_ref(&stream),
            )
        };
        // SAFETY: drops the single reference cloned into the stream struct.
        unsafe {
            let mut stream = stream;
            ManuallyDrop::drop(&mut stream.pInputSurface);
        }
        result.map_err(win)
    }
}

// ---------------------------------------------------------------------------
// Video stage
// ---------------------------------------------------------------------------

/// One encoder output access unit before conversion to a packet.
struct RawVideo {
    avcc: Vec<u8>,
    idr: bool,
    clean_point: bool,
    sps: Option<Vec<u8>>,
    pps: Option<Vec<u8>>,
    pts: i64,
    dts: i64,
    duration: i64,
}

/// Largest reorder depth the assigner grows to.
const MAX_REORDER_DEPTH: usize = 8;

/// Decode timestamp assignment. Uses the encoder's DecodeTimestamp when
/// present. Without it and with B-frames the decode order differs from the
/// presentation order, so dts is derived: with reorder depth R the dts of the
/// i-th emitted packet is the i-th smallest pts minus R frames of lead-in
/// for the first R packets (can therefore be below the first pts by up to
/// R frame durations).
///
/// The presentation timestamp is never changed. When the encoder ignores the
/// "no B-frames" setting (depth 0 but pts goes backwards in output order) the
/// assigner switches to derived mode; when the real reorder depth exceeds the
/// configured one the depth grows. Packets whose dts cannot be kept below
/// their pts and above the previous dts (only the ones right at such a
/// switch) are returned with `dts <= previous dts` so the replay buffer
/// rejects exactly those packets instead of corrupting timestamps.
struct DtsAssigner {
    depth: usize,
    frame_ticks: i64,
    first_pts: Option<i64>,
    heap: BinaryHeap<Reverse<i64>>,
    last: Option<i64>,
    last_pts: Option<i64>,
    #[cfg_attr(not(test), allow(dead_code))]
    switches: u32,
    #[cfg_attr(not(test), allow(dead_code))]
    violations: u32,
}

impl DtsAssigner {
    fn new(depth: usize, frame_ticks: i64) -> Self {
        Self {
            depth,
            frame_ticks,
            first_pts: None,
            heap: BinaryHeap::new(),
            last: None,
            last_pts: None,
            switches: 0,
            violations: 0,
        }
    }

    /// i-th smallest pts, with `depth` frames of lead-in at the start.
    fn derive(&mut self, pts: i64) -> i64 {
        let first = *self.first_pts.get_or_insert(pts);
        self.heap.push(Reverse(pts));
        if self.heap.len() > self.depth {
            self.heap.pop().map_or(pts, |Reverse(min)| min)
        } else {
            first - (self.depth + 1 - self.heap.len()) as i64 * self.frame_ticks
        }
    }

    fn assign(&mut self, pts: i64, external: Option<i64>) -> i64 {
        let previous_pts = self.last_pts.replace(pts);
        let derived = external.is_none();
        let mut dts = match external {
            Some(dts) => dts,
            None if self.depth == 0 => {
                if let Some(previous) = previous_pts.filter(|previous| pts < *previous) {
                    // B-frames although none were configured.
                    self.depth = 2;
                    self.switches += 1;
                    self.first_pts = Some(previous);
                    self.heap.push(Reverse(previous));
                    self.derive(pts)
                } else {
                    pts
                }
            }
            None => self.derive(pts),
        };
        if let Some(last) = self.last
            && dts <= last
        {
            let next = last + 1;
            if next <= pts || !derived {
                dts = next;
            } else {
                // The real reorder depth is larger than assumed.
                self.violations += 1;
                self.depth = (self.depth + 1).min(MAX_REORDER_DEPTH);
                dts = dts.min(pts);
            }
        }
        if derived {
            dts = dts.min(pts);
        }
        self.last = Some(self.last.map_or(dts, |last| last.max(dts)));
        dts
    }
}

struct Nv12Pool {
    _textures: Vec<ID3D11Texture2D>,
    shared: Arc<Shared>,
    callback: IMFAsyncCallback,
}

struct VideoStage {
    transform: IMFTransform,
    events: IMFMediaEventGenerator,
    provides_samples: bool,
    output_size: u32,
    credits: u32,
    pending: VecDeque<IMFSample>,
    drained: bool,
    converter: Converter,
    pool: Nv12Pool,
    sequence_header: Option<(Vec<u8>, Vec<u8>)>,
    dts: DtsAssigner,
    frame_ticks: i64,
    // Keeps the DXGI device manager alive as long as the transform.
    _manager: IMFDXGIDeviceManager,
}

fn parameter_sets(annex_b: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
    let mut sps = None;
    let mut pps = None;
    for unit in nal_units(annex_b) {
        match unit[0] & 0x1f {
            7 if sps.is_none() => sps = Some(unit.to_vec()),
            8 if pps.is_none() => pps = Some(unit.to_vec()),
            _ => {}
        }
    }
    sps.zip(pps)
}

/// SPS/PPS from `MF_MT_MPEG_SEQUENCE_HEADER` of the current output type.
fn sequence_header(transform: &IMFTransform) -> Option<(Vec<u8>, Vec<u8>)> {
    // SAFETY: valid transform; the blob is read into a buffer of the size the
    // media type reports.
    unsafe {
        let media_type = transform.GetOutputCurrentType(0).ok()?;
        let size = media_type.GetBlobSize(&MF_MT_MPEG_SEQUENCE_HEADER).ok()? as usize;
        let mut blob = vec![0u8; size];
        media_type
            .GetBlob(&MF_MT_MPEG_SEQUENCE_HEADER, &mut blob, None)
            .ok()?;
        parameter_sets(&blob)
    }
}

impl VideoStage {
    fn output_type(
        config: &EncoderConfig,
        tuning: &H264Tuning,
    ) -> Result<IMFMediaType, EncoderError> {
        let media_type = video_type(
            &MFVideoFormat_H264,
            (config.output_width, config.output_height),
            config.fps,
        )
        .map_err(win)?;
        let profile = match tuning.profile {
            H264Profile::Main => eAVEncH264VProfile_Main,
            H264Profile::High => eAVEncH264VProfile_High,
        };
        // SAFETY: scalar attributes on a fresh media type.
        unsafe {
            media_type
                .SetUINT32(&MF_MT_AVG_BITRATE, tuning.bitrate_bps)
                .map_err(win)?;
            media_type
                .SetUINT32(&MF_MT_MPEG2_PROFILE, profile.0 as u32)
                .map_err(win)?;
        }
        Ok(media_type)
    }

    fn input_type(config: &EncoderConfig) -> Result<IMFMediaType, EncoderError> {
        let media_type = video_type(
            &MFVideoFormat_NV12,
            (config.output_width, config.output_height),
            config.fps,
        )
        .map_err(win)?;
        // SAFETY: scalar attributes on a fresh media type. Matches the video
        // processor's output color space (BT.709, limited range).
        unsafe {
            media_type
                .SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE, MFNominalRange_16_235.0 as u32)
                .map_err(win)?;
            media_type
                .SetUINT32(&MF_MT_YUV_MATRIX, MFVideoTransferMatrix_BT709.0 as u32)
                .map_err(win)?;
        }
        Ok(media_type)
    }

    /// Activates and configures one hardware encoder candidate.
    fn configure(
        activate: &IMFActivate,
        d3d: &D3DContext,
        config: &EncoderConfig,
        tuning: &H264Tuning,
        detail: &str,
    ) -> Result<Self, EncoderError> {
        // SAFETY: plain FFI on live COM objects; every pointer passed is to a
        // live local. The D3D manager pointer is valid for the call and the
        // MFT takes its own reference.
        unsafe {
            let transform: IMFTransform = activate.ActivateObject().map_err(win)?;
            let attributes = transform.GetAttributes().map_err(win)?;
            let hardware = has_attribute(&attributes, &MFT_ENUM_HARDWARE_URL_Attribute)
                || has_attribute(&attributes, &MFT_ENUM_HARDWARE_VENDOR_ID_Attribute);
            if !hardware {
                return Err(EncoderError::NoHardwareEncoder);
            }
            // Hardware MFTs are asynchronous and must be unlocked first.
            attributes
                .SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1)
                .map_err(win)?;

            let mut token = 0u32;
            let mut manager: Option<IMFDXGIDeviceManager> = None;
            MFCreateDXGIDeviceManager(&mut token, &mut manager).map_err(win)?;
            let manager = manager.ok_or_else(no_pointer)?;
            manager.ResetDevice(d3d.device(), token).map_err(win)?;
            transform
                .ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, manager.as_raw() as usize)
                .map_err(|e| map_format_error(e, "Direct3D11 input"))?;

            apply_codec_properties(&transform, &codec_properties(tuning));
            transform
                .SetOutputType(0, &Self::output_type(config, tuning)?, 0)
                .map_err(|e| map_format_error(e, detail))?;
            transform
                .SetInputType(0, &Self::input_type(config)?, 0)
                .map_err(|e| map_format_error(e, detail))?;

            let info = transform.GetOutputStreamInfo(0).map_err(win)?;
            let provides_samples = info.dwFlags
                & (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 | MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0)
                    as u32
                != 0;
            let events: IMFMediaEventGenerator = transform.cast().map_err(win)?;

            // NV12 pool + converter.
            let output = (config.output_width, config.output_height);
            let shared = Arc::new(Shared {
                free: Mutex::new(Vec::with_capacity(NV12_POOL)),
                closed: AtomicBool::new(false),
            });
            let callback: IMFAsyncCallback = ReturnCallback {
                shared: Arc::clone(&shared),
            }
            .into();
            let mut textures = Vec::with_capacity(NV12_POOL);
            for _ in 0..NV12_POOL {
                textures.push(nv12_texture(d3d, output)?);
            }
            let converter = Converter::new(
                d3d,
                (config.input_width, config.input_height),
                output,
                config.fps,
                &textures,
            )?;
            for (id, texture) in textures.iter().enumerate() {
                let sample = build_sample(texture, id).map_err(win)?;
                shared.lock().push(Slot { id, sample });
            }

            transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)
                .map_err(win)?;
            transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)
                .map_err(win)?;

            let frame_ticks = TICKS_PER_SECOND / i64::from(config.fps.max(1));
            Ok(Self {
                sequence_header: sequence_header(&transform),
                transform,
                events,
                provides_samples,
                output_size: info.cbSize.max(1 << 20),
                credits: 0,
                pending: VecDeque::new(),
                drained: false,
                converter,
                pool: Nv12Pool {
                    _textures: textures,
                    shared,
                    callback,
                },
                dts: DtsAssigner::new(tuning.b_frames as usize, frame_ticks),
                frame_ticks,
                _manager: manager,
            })
        }
    }

    /// Drains pending async events: credits, outputs, drain completion.
    fn pump(&mut self, out: &mut Vec<RawVideo>) -> Result<(), EncoderError> {
        loop {
            // SAFETY: valid event generator; NO_WAIT never blocks.
            let event = match unsafe { self.events.GetEvent(MF_EVENT_FLAG_NO_WAIT) } {
                Ok(event) => event,
                Err(error) if code_is(&error, MF_E_NO_EVENTS_AVAILABLE) => break,
                Err(error) => return Err(map_error(error)),
            };
            // SAFETY: valid event.
            let kind = unsafe { event.GetType() }.map_err(win)?;
            if kind == METransformNeedInput.0 as u32 {
                self.credits += 1;
            } else if kind == METransformHaveOutput.0 as u32 {
                self.process_output(out)?;
            } else if kind == METransformDrainComplete.0 as u32 {
                self.drained = true;
            }
        }
        self.feed_pending()
    }

    fn feed_pending(&mut self) -> Result<(), EncoderError> {
        while self.credits > 0 {
            let Some(sample) = self.pending.pop_front() else {
                break;
            };
            self.credits -= 1;
            // SAFETY: valid sample and transform; a credit exists. Our own
            // reference is dropped right after, so the MFT's release is the
            // last one and returns the pooled sample via the callback.
            unsafe { self.transform.ProcessInput(0, &sample, 0) }.map_err(map_error)?;
        }
        Ok(())
    }

    fn renegotiate_output(&mut self) -> Result<(), EncoderError> {
        // SAFETY: valid transform; the offered type is set back unchanged.
        unsafe {
            let available = self.transform.GetOutputAvailableType(0, 0).map_err(win)?;
            self.transform
                .SetOutputType(0, &available, 0)
                .map_err(|e| map_format_error(e, "output stream change"))?;
        }
        if let Some(header) = sequence_header(&self.transform) {
            self.sequence_header.get_or_insert(header);
        }
        Ok(())
    }

    fn process_output(&mut self, out: &mut Vec<RawVideo>) -> Result<(), EncoderError> {
        for _ in 0..2 {
            let own = if self.provides_samples {
                None
            } else {
                // SAFETY: plain allocation FFI.
                let sample = unsafe {
                    let sample = MFCreateSample().map_err(win)?;
                    let buffer = MFCreateMemoryBuffer(self.output_size).map_err(win)?;
                    sample.AddBuffer(&buffer).map_err(win)?;
                    sample
                };
                Some(sample)
            };
            let mut buffer = MFT_OUTPUT_DATA_BUFFER {
                dwStreamID: 0,
                pSample: ManuallyDrop::new(own),
                dwStatus: 0,
                pEvents: ManuallyDrop::new(None),
            };
            let mut status = 0u32;
            // SAFETY: one valid output buffer; `buffer` lives across the call.
            let result = unsafe {
                self.transform
                    .ProcessOutput(0, std::slice::from_mut(&mut buffer), &mut status)
            };
            // SAFETY: takes back ownership of what the MFT (or we) stored in
            // the ManuallyDrop fields; each is taken exactly once.
            let (sample, _events) = unsafe {
                (
                    ManuallyDrop::take(&mut buffer.pSample),
                    ManuallyDrop::take(&mut buffer.pEvents),
                )
            };
            match result {
                Ok(()) => {
                    if let Some(sample) = sample {
                        self.convert_sample(&sample, out)?;
                    }
                    return Ok(());
                }
                Err(error) if code_is(&error, MF_E_TRANSFORM_NEED_MORE_INPUT) => return Ok(()),
                Err(error) if code_is(&error, MF_E_TRANSFORM_STREAM_CHANGE) => {
                    self.renegotiate_output()?;
                }
                Err(error) => return Err(map_error(error)),
            }
        }
        Ok(())
    }

    fn convert_sample(
        &mut self,
        sample: &IMFSample,
        out: &mut Vec<RawVideo>,
    ) -> Result<(), EncoderError> {
        // SAFETY: valid sample; the contiguous buffer is locked only while
        // the bytes are converted and is unlocked on every path.
        let (unit, pts, duration, external_dts, clean) = unsafe {
            let pts = sample.GetSampleTime().map_err(win)?;
            let duration = sample.GetSampleDuration().ok().filter(|d| *d > 0);
            let external_dts = sample
                .GetUINT64(&MFSampleExtension_DecodeTimestamp)
                .ok()
                .map(|v| v as i64);
            let clean = sample
                .GetUINT32(&MFSampleExtension_CleanPoint)
                .is_ok_and(|v| v != 0);
            let buffer = sample.ConvertToContiguousBuffer().map_err(win)?;
            let mut data = std::ptr::null_mut();
            let mut length = 0u32;
            buffer
                .Lock(&mut data, None, Some(&mut length))
                .map_err(win)?;
            let bytes = std::slice::from_raw_parts(data, length as usize);
            let unit = annexb_to_avcc(bytes);
            let _ = buffer.Unlock();
            (unit, pts, duration, external_dts, clean)
        };
        if unit.avcc.is_empty() {
            return Ok(());
        }
        let dts = self.dts.assign(pts, external_dts);
        out.push(RawVideo {
            avcc: unit.avcc,
            idr: unit.idr,
            clean_point: clean,
            sps: unit.sps,
            pps: unit.pps,
            pts,
            dts,
            duration: duration.unwrap_or(self.frame_ticks),
        });
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Audio stage
// ---------------------------------------------------------------------------

struct AudioStage {
    transform: IMFTransform,
    output: IMFSample,
    asc: Vec<u8>,
    produced: i64,
    first_time: Option<i64>,
    last_pts: Option<i64>,
    /// Output times replaced by the input-derived time.
    #[cfg_attr(not(test), allow(dead_code))]
    pts_corrections: u64,
}

/// Input-derived time of the `produced`-th AAC frame (exact, no per-frame
/// rounding error).
fn aac_expected_time(first_time: i64, produced: i64) -> i64 {
    first_time
        + (i128::from(produced) * i128::from(AAC_FRAME_SAMPLES) * i128::from(TICKS_PER_SECOND)
            / i128::from(SAMPLE_RATE)) as i64
}

/// Presentation time of an AAC access unit: the MFT's output time unless it is
/// missing or more than one AAC frame away from the input-derived `expected`
/// time, in which case `expected` is used. Returns whether it was corrected.
fn aac_presentation_time(output: Option<i64>, expected: i64) -> (i64, bool) {
    match output {
        Some(time) if (time - expected).abs() <= AAC_FRAME_TICKS => (time, false),
        Some(_) => (expected, true),
        None => (expected, false),
    }
}

fn audio_type(subtype: &GUID, bits: u32, aac: bool) -> Result<IMFMediaType, WinError> {
    // SAFETY: fresh media type; scalar attributes match the AAC encoder's
    // supported PCM16 input and AAC-LC output formats.
    unsafe {
        let media_type = MFCreateMediaType()?;
        media_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
        media_type.SetGUID(&MF_MT_SUBTYPE, subtype)?;
        media_type.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, 2)?;
        media_type.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, 48_000)?;
        media_type.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, bits)?;
        if aac {
            media_type.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, 24_000)?;
            media_type.SetUINT32(&MF_MT_AAC_PAYLOAD_TYPE, 0)?;
            media_type.SetUINT32(&MF_MT_AAC_AUDIO_PROFILE_LEVEL_INDICATION, 0x29)?;
        } else {
            media_type.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, 4)?;
            media_type.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, 192_000)?;
            media_type.SetUINT32(&MF_MT_ALL_SAMPLES_INDEPENDENT, 1)?;
        }
        Ok(media_type)
    }
}

/// MPEG-4 AudioSpecificConfig for AAC-LC 48 kHz stereo.
const FALLBACK_ASC: [u8; 2] = [0x11, 0x90];

impl AudioStage {
    fn new() -> Result<Self, EncoderError> {
        // SAFETY: plain FFI; the AAC encoder MFT is an in-process synchronous
        // transform, configured output first, then input.
        unsafe {
            let transform: IMFTransform =
                CoCreateInstance(&AACMFTEncoder, None, CLSCTX_INPROC_SERVER).map_err(win)?;
            transform
                .SetOutputType(
                    0,
                    &audio_type(&MFAudioFormat_AAC, 16, true).map_err(win)?,
                    0,
                )
                .map_err(|e| map_format_error(e, "AAC 48 kHz stereo"))?;
            transform
                .SetInputType(
                    0,
                    &audio_type(&MFAudioFormat_PCM, 16, false).map_err(win)?,
                    0,
                )
                .map_err(|e| map_format_error(e, "PCM16 48 kHz stereo"))?;

            let asc = transform
                .GetOutputCurrentType(0)
                .ok()
                .and_then(|media_type| {
                    let size = media_type.GetBlobSize(&MF_MT_USER_DATA).ok()? as usize;
                    let mut blob = vec![0u8; size];
                    media_type.GetBlob(&MF_MT_USER_DATA, &mut blob, None).ok()?;
                    // Skip the 12-byte HEAACWAVEINFO prefix.
                    (blob.len() > 12).then(|| blob[12..].to_vec())
                })
                .unwrap_or_else(|| FALLBACK_ASC.to_vec());

            let info = transform.GetOutputStreamInfo(0).map_err(win)?;
            let output_size = info.cbSize.max(8192);
            let output = MFCreateSample().map_err(win)?;
            output
                .AddBuffer(&MFCreateMemoryBuffer(output_size).map_err(win)?)
                .map_err(win)?;

            transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)
                .map_err(win)?;
            transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)
                .map_err(win)?;
            Ok(Self {
                transform,
                output,
                asc,
                produced: 0,
                first_time: None,
                last_pts: None,
                pts_corrections: 0,
            })
        }
    }

    /// Pulls every available AAC access unit.
    fn collect(&mut self, track: u8, out: &mut Vec<EncodedPacket>) -> Result<(), EncoderError> {
        loop {
            // SAFETY: the reusable output sample is reset before each call;
            // buffer locks are paired with unlocks.
            unsafe {
                let buffer = self.output.GetBufferByIndex(0).map_err(win)?;
                buffer.SetCurrentLength(0).map_err(win)?;
                let mut data = MFT_OUTPUT_DATA_BUFFER {
                    dwStreamID: 0,
                    pSample: ManuallyDrop::new(Some(self.output.clone())),
                    dwStatus: 0,
                    pEvents: ManuallyDrop::new(None),
                };
                let mut status = 0u32;
                let result =
                    self.transform
                        .ProcessOutput(0, std::slice::from_mut(&mut data), &mut status);
                let _ = ManuallyDrop::take(&mut data.pSample);
                let _ = ManuallyDrop::take(&mut data.pEvents);
                match result {
                    Ok(()) => {}
                    Err(error) if code_is(&error, MF_E_TRANSFORM_NEED_MORE_INPUT) => return Ok(()),
                    Err(error) => return Err(map_error(error)),
                }
                let mut bytes = std::ptr::null_mut();
                let mut length = 0u32;
                let mut max = 0u32;
                buffer
                    .Lock(&mut bytes, Some(&mut max), Some(&mut length))
                    .map_err(win)?;
                let payload: Arc<[u8]> =
                    Arc::from(std::slice::from_raw_parts(bytes, length as usize));
                let _ = buffer.Unlock();
                if payload.is_empty() {
                    continue;
                }
                let time = self.output.GetSampleTime().ok().filter(|t| *t >= 0);
                let duration = self
                    .output
                    .GetSampleDuration()
                    .ok()
                    .filter(|d| *d > 0)
                    .unwrap_or(AAC_FRAME_TICKS);
                let expected = aac_expected_time(self.first_time.unwrap_or(0), self.produced);
                let (mut pts, corrected) = aac_presentation_time(time, expected);
                self.pts_corrections += u64::from(corrected);
                // Strictly increasing per track (documented nudge of 1 tick).
                if let Some(last) = self.last_pts
                    && pts <= last
                {
                    pts = last + 1;
                }
                self.last_pts = Some(pts);
                self.produced += 1;
                out.push(EncodedPacket {
                    track,
                    pts,
                    dts: pts,
                    duration,
                    keyframe: true,
                    data: payload,
                });
            }
        }
    }

    fn submit(
        &mut self,
        track: u8,
        samples: &[f32],
        time: i64,
        out: &mut Vec<EncodedPacket>,
    ) -> Result<(), EncoderError> {
        let bytes = (samples.len() * 2) as u32;
        // SAFETY: MF allocates a buffer of `bytes`; Lock returns a writable
        // region of at least that size; the lock is released before use.
        let sample = unsafe {
            let buffer = MFCreateMemoryBuffer(bytes).map_err(map_error)?;
            let mut data = std::ptr::null_mut();
            buffer.Lock(&mut data, None, None).map_err(map_error)?;
            for (index, sample) in samples.iter().enumerate() {
                let clean = if sample.is_finite() {
                    sample.clamp(-1.0, 1.0)
                } else {
                    0.0
                };
                let value = (clean * 32767.0).round() as i16;
                std::ptr::copy_nonoverlapping(value.to_le_bytes().as_ptr(), data.add(index * 2), 2);
            }
            buffer.Unlock().map_err(map_error)?;
            buffer.SetCurrentLength(bytes).map_err(map_error)?;
            let sample = MFCreateSample().map_err(map_error)?;
            sample.AddBuffer(&buffer).map_err(map_error)?;
            sample.SetSampleTime(time).map_err(map_error)?;
            sample
                .SetSampleDuration((samples.len() / 2) as i64 * TICKS_PER_SECOND / SAMPLE_RATE)
                .map_err(map_error)?;
            sample
        };
        self.first_time.get_or_insert(time);
        // SAFETY: valid transform and sample.
        let mut result = unsafe { self.transform.ProcessInput(0, &sample, 0) };
        if let Err(error) = &result
            && code_is(error, MF_E_NOTACCEPTING)
        {
            self.collect(track, out)?;
            // SAFETY: as above, after draining the pending output.
            result = unsafe { self.transform.ProcessInput(0, &sample, 0) };
        }
        result.map_err(map_error)?;
        self.collect(track, out)
    }

    fn drain(&mut self, track: u8, out: &mut Vec<EncodedPacket>) -> Result<(), EncoderError> {
        // SAFETY: valid transform; messages carry no pointers.
        unsafe {
            self.transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0)
                .map_err(win)?;
            self.transform
                .ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0)
                .map_err(win)?;
        }
        self.collect(track, out)
    }
}

// ---------------------------------------------------------------------------
// PacketEncoder
// ---------------------------------------------------------------------------

pub struct PacketEncoder {
    video: VideoStage,
    audio: Vec<AudioStage>,
    labels: Vec<String>,
    name: String,
    size: (u32, u32),
    layout: Option<StreamLayout>,
    raw: Vec<RawVideo>,
    ready: VecDeque<EncodedPacket>,
    scratch: Vec<EncodedPacket>,
    submitted: u64,
}

impl PacketEncoder {
    /// Creates the encoder with default track labels ([`default_label`]).
    pub fn create(
        mf: &MediaFoundation,
        d3d: &D3DContext,
        config: &EncoderConfig,
        audio_tracks: usize,
    ) -> Result<Self, EncoderError> {
        let labels: Vec<String> = (0..audio_tracks).map(default_label).collect();
        Self::create_with_labels(mf, d3d, config, &labels)
    }

    /// One AAC track per label; track 1 should be the primary mix.
    pub fn create_with_labels(
        mf: &MediaFoundation,
        d3d: &D3DContext,
        config: &EncoderConfig,
        labels: &[String],
    ) -> Result<Self, EncoderError> {
        if labels.len() > MAX_AUDIO_TRACKS {
            return Err(EncoderError::UnsupportedFormat(
                "too many audio tracks".into(),
            ));
        }
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
        let _ = hardware_h264_encoders(mf)?;
        let tuning = H264Tuning::new(config.profile, config.bitrate_mbps, config.fps);

        let (video, name) = Self::open_video(d3d, config, &tuning, &detail)?;
        let mut audio = Vec::with_capacity(labels.len());
        for _ in labels {
            audio.push(AudioStage::new()?);
        }
        let mut encoder = Self {
            video,
            audio,
            labels: labels.to_vec(),
            name,
            size: (config.output_width, config.output_height),
            layout: None,
            raw: Vec::new(),
            ready: VecDeque::new(),
            scratch: Vec::new(),
            submitted: 0,
        };
        if let Some((sps, pps)) = encoder.video.sequence_header.clone() {
            encoder.set_layout(sps, pps);
        }
        Ok(encoder)
    }

    fn open_video(
        d3d: &D3DContext,
        config: &EncoderConfig,
        tuning: &H264Tuning,
        detail: &str,
    ) -> Result<(VideoStage, String), EncoderError> {
        let output = MFT_REGISTER_TYPE_INFO {
            guidMajorType: MFMediaType_Video,
            guidSubtype: MFVideoFormat_H264,
        };
        let input = MFT_REGISTER_TYPE_INFO {
            guidMajorType: MFMediaType_Video,
            guidSubtype: MFVideoFormat_NV12,
        };
        let mut activates: *mut Option<IMFActivate> = std::ptr::null_mut();
        let mut count = 0u32;
        // SAFETY: out-pointers reference live locals; type infos outlive the call.
        unsafe {
            MFTEnumEx(
                MFT_CATEGORY_VIDEO_ENCODER,
                MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER,
                Some(&input),
                Some(&output),
                &mut activates,
                &mut count,
            )
        }
        .map_err(win)?;
        if activates.is_null() || count == 0 {
            return Err(EncoderError::NoHardwareEncoder);
        }
        // SAFETY: MFTEnumEx returned `count` initialized interface pointers in
        // a CoTaskMem array; each is taken (released on drop), then the array
        // is freed.
        let candidates: Vec<IMFActivate> = unsafe {
            let items = std::slice::from_raw_parts_mut(activates, count as usize);
            let taken = items.iter_mut().filter_map(Option::take).collect();
            CoTaskMemFree(Some(activates as *const _));
            taken
        };

        let mut last = EncoderError::NoHardwareEncoder;
        for activate in &candidates {
            match VideoStage::configure(activate, d3d, config, tuning, detail) {
                Ok(stage) => {
                    let attributes: &IMFAttributes = activate;
                    let name = super::media_foundation::allocated_string(
                        attributes,
                        &MFT_FRIENDLY_NAME_Attribute,
                    )
                    .unwrap_or_else(|| "Hardware H.264 encoder".to_owned());
                    return Ok((stage, name));
                }
                Err(error) => last = error,
            }
        }
        Err(last)
    }

    /// Friendly name of the hardware encoder in use.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Track list; `Some` once the H.264 SPS/PPS are known. Poll never
    /// returns a packet before this is `Some`.
    pub fn layout(&self) -> Option<StreamLayout> {
        self.layout.clone()
    }

    fn set_layout(&mut self, sps: Vec<u8>, pps: Vec<u8>) {
        let mut tracks = vec![TrackInfo {
            codec: CodecConfig::H264 {
                width: self.size.0,
                height: self.size.1,
                sps,
                pps,
            },
            label: "Video".to_owned(),
        }];
        for (stage, label) in self.audio.iter().zip(&self.labels) {
            tracks.push(TrackInfo {
                codec: CodecConfig::Aac {
                    sample_rate: 48_000,
                    channels: 2,
                    audio_specific_config: stage.asc.clone(),
                },
                label: label.clone(),
            });
        }
        self.layout = Some(StreamLayout { tracks });
    }

    /// Turns raw encoder output into packets; the first parameter sets seen
    /// define the layout. Video before the layout is known is discarded
    /// (only possible if the encoder emitted no parameter sets at all).
    fn absorb(&mut self) {
        let raw = std::mem::take(&mut self.raw);
        for unit in raw {
            if self.layout.is_none()
                && let (Some(sps), Some(pps)) = (unit.sps.clone(), unit.pps.clone())
            {
                self.set_layout(sps, pps);
            }
            if self.layout.is_none() {
                continue;
            }
            self.ready.push_back(EncodedPacket {
                track: StreamLayout::VIDEO_TRACK,
                pts: unit.pts,
                dts: unit.dts,
                duration: unit.duration,
                keyframe: unit.idr || unit.clean_point,
                data: Arc::from(unit.avcc),
            });
        }
    }

    fn hold_audio(&mut self) {
        for packet in self.scratch.drain(..) {
            self.ready.push_back(packet);
        }
        if self.layout.is_none() {
            while self.ready.len() > MAX_HELD_AUDIO {
                self.ready.pop_front();
            }
        }
    }

    /// Copies `texture` on the GPU, converts/scales it to NV12 and queues it
    /// for the hardware encoder. `sample_time`/`duration` are 100 ns ticks on
    /// the session timeline. Never blocks: returns `Dropped` when the NV12
    /// pool is exhausted or the encoder has no input credit and two frames
    /// are already waiting. A dropped frame is not staged.
    pub fn submit(
        &mut self,
        texture: &ID3D11Texture2D,
        content_size: (u32, u32),
        sample_time: i64,
        duration: i64,
    ) -> Result<SubmitOutcome, EncoderError> {
        self.queue(Some((texture, content_size)), sample_time, duration)
    }

    /// Queues the most recently staged frame again (static scene fill)
    /// without another copy of the source. Same outcomes as
    /// [`Self::submit`]; an error when nothing was staged yet.
    pub fn submit_repeat(
        &mut self,
        sample_time: i64,
        duration: i64,
    ) -> Result<SubmitOutcome, EncoderError> {
        if !self.has_frame() {
            return Err(EncoderError::UnsupportedFormat("no frame to repeat".into()));
        }
        self.queue(None, sample_time, duration)
    }

    /// GPU-copies `texture` into the staging texture without encoding it, so
    /// [`Self::submit_repeat`] can start from it.
    pub fn stage(
        &mut self,
        texture: &ID3D11Texture2D,
        content_size: (u32, u32),
    ) -> Result<(), EncoderError> {
        self.video.converter.stage(texture, content_size)
    }

    /// `true` once a frame was staged (submitted or [`Self::stage`]d).
    pub fn has_frame(&self) -> bool {
        self.video.converter.staged.is_some()
    }

    fn queue(
        &mut self,
        source: Option<(&ID3D11Texture2D, (u32, u32))>,
        sample_time: i64,
        duration: i64,
    ) -> Result<SubmitOutcome, EncoderError> {
        self.video.pump(&mut self.raw)?;
        self.absorb();
        if self.video.pending.len() >= MAX_PENDING {
            return Ok(SubmitOutcome::Dropped);
        }
        let popped = self.video.pool.shared.lock().pop();
        let Some(slot) = popped else {
            return Ok(SubmitOutcome::Dropped);
        };
        let converter = &mut self.video.converter;
        let converted = match source {
            Some((texture, content_size)) => converter
                .stage(texture, content_size)
                .and_then(|()| converter.blit(slot.id)),
            None => converter.blit(slot.id),
        };
        if let Err(error) = converted {
            self.video.pool.shared.lock().push(slot);
            return Err(error);
        }
        let callback = &self.video.pool.callback;
        // SAFETY: valid sample. Until SetAllocator succeeds the callback is
        // not armed, so on failure the slot goes back to the pool by hand.
        let armed = unsafe {
            (|| -> Result<(), WinError> {
                slot.sample.SetSampleTime(sample_time)?;
                slot.sample.SetSampleDuration(duration)?;
                let tracked: IMFTrackedSample = slot.sample.cast()?;
                tracked.SetAllocator(callback, None::<&windows_core::IUnknown>)
            })()
        };
        if let Err(error) = armed {
            self.video.pool.shared.lock().push(slot);
            return Err(map_error(error));
        }
        self.video.pending.push_back(slot.sample);
        self.submitted += 1;
        self.video.feed_pending()?;
        Ok(SubmitOutcome::Submitted)
    }

    /// Encodes aligned stereo f32 at 48 kHz (same validation as
    /// `HardwareEncoder::submit_audio`). `time` is on the session timeline.
    pub fn submit_audio(
        &mut self,
        track: usize,
        samples: &[f32],
        time: i64,
    ) -> Result<(), EncoderError> {
        if time < 0
            || samples.is_empty()
            || !samples.len().is_multiple_of(2)
            || samples.len() > 96_000
        {
            return Err(EncoderError::UnsupportedFormat(
                "invalid audio packet".into(),
            ));
        }
        let stage = self
            .audio
            .get_mut(track)
            .ok_or_else(|| EncoderError::UnsupportedFormat("unknown audio stream".into()))?;
        let result = stage.submit(track as u8 + 1, samples, time, &mut self.scratch);
        self.hold_audio();
        result
    }

    /// Non-blocking: appends every packet available now to `out`.
    pub fn poll(&mut self, out: &mut Vec<EncodedPacket>) -> Result<(), EncoderError> {
        self.video.pump(&mut self.raw)?;
        self.absorb();
        if self.layout.is_some() {
            out.extend(self.ready.drain(..));
        }
        Ok(())
    }

    /// Drains video and audio (video wait bounded by 2 s) and appends the
    /// remaining packets to `out`.
    pub fn finish(mut self, out: &mut Vec<EncodedPacket>) -> Result<(), EncoderError> {
        let deadline = Instant::now() + DRAIN_TIMEOUT;
        // Let queued frames reach the encoder first.
        while !self.video.pending.is_empty() && Instant::now() < deadline {
            self.video.pump(&mut self.raw)?;
            std::thread::sleep(Duration::from_millis(1));
        }
        // SAFETY: valid transform; messages carry no pointers.
        unsafe {
            self.video
                .transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0)
                .map_err(map_error)?;
            self.video
                .transform
                .ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0)
                .map_err(map_error)?;
        }
        while !self.video.drained && Instant::now() < deadline {
            self.video.pump(&mut self.raw)?;
            if !self.video.drained {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        self.absorb();
        for index in 0..self.audio.len() {
            let mut scratch = std::mem::take(&mut self.scratch);
            self.audio[index].drain(index as u8 + 1, &mut scratch)?;
            self.scratch = scratch;
        }
        self.hold_audio();
        if self.layout.is_some() {
            out.extend(self.ready.drain(..));
        }
        Ok(())
    }
}

impl Drop for PacketEncoder {
    fn drop(&mut self) {
        self.video.pool.shared.closed.store(true, Ordering::Release);
        self.video.pending.clear();
        // SAFETY: valid transform; best-effort stop messages without pointers.
        unsafe {
            let _ = self
                .video
                .transform
                .ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0);
            let _ = self
                .video
                .transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0);
        }
        let slots: Vec<Slot> = self.video.pool.shared.lock().drain(..).collect();
        for slot in slots {
            // SAFETY: valid tracked sample; clearing the allocator lets it be
            // freed instead of returning to the (closed) pool.
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
    use clipper_core::recording::QualityProfile;
    use clipper_platform::windows::ComApartment;

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

    fn source(d3d: &D3DContext, w: u32, h: u32) -> (ID3D11Texture2D, ID3D11RenderTargetView) {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: w,
            Height: h,
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
            d3d.device()
                .CreateRenderTargetView(texture.as_ref().expect("texture"), None, Some(&mut view))
                .expect("rtv");
        }
        (texture.expect("texture"), view.expect("rtv"))
    }

    /// Gradient background plus moving blocks so the encoder has real work.
    fn draw(d3d: &D3DContext, view: &ID3D11RenderTargetView, step: u32, w: u32, h: u32) {
        let t = (step % 90) as f32 / 90.0;
        // SAFETY: valid view on the shared device; rects lie inside the target.
        unsafe {
            d3d.context()
                .ClearRenderTargetView(view, &[t, 0.2, 1.0 - t, 1.0]);
            if let Ok(context1) = d3d.context().cast::<ID3D11DeviceContext1>() {
                for block in 0..6u32 {
                    let x = ((step * (3 + block * 2) + block * 97) % (w - 80)) as i32;
                    let y = ((step * (2 + block) + block * 61) % (h - 80)) as i32;
                    let rect = RECT {
                        left: x,
                        top: y,
                        right: x + 80,
                        bottom: y + 80,
                    };
                    let c = [(block as f32) / 6.0, 1.0 - t, t, 1.0];
                    context1.ClearView(view, &c, Some(&[rect]));
                }
            }
        }
    }

    struct Run {
        packets: Vec<EncodedPacket>,
        layout: StreamLayout,
        early: usize,
        dropped: u32,
        name: String,
    }

    fn run(env: &Env, profile: QualityProfile, seconds: u32) -> Option<Run> {
        let (w, h, fps) = (1280u32, 720u32, 60u32);
        let config = EncoderConfig {
            input_width: w,
            input_height: h,
            output_width: w,
            output_height: h,
            fps,
            bitrate_mbps: 8,
            profile,
        };
        let mut encoder = match PacketEncoder::create(&env.mf, &env.d3d, &config, 1) {
            Ok(encoder) => encoder,
            Err(EncoderError::NoHardwareEncoder) => return None,
            Err(error) => panic!("create failed: {error}"),
        };
        let name = encoder.name().to_owned();
        let (texture, view) = source(&env.d3d, w, h);
        let frame = TICKS_PER_SECOND / i64::from(fps);
        let mut packets = Vec::new();
        let mut early = 0;
        let mut dropped = 0;
        let mut audio_time = 0i64;
        for index in 0..seconds * fps {
            draw(&env.d3d, &view, index, w, h);
            let time = i64::from(index) * frame;
            // Audio up to this video time, in 10 ms chunks.
            while audio_time <= time {
                let n0 = (audio_time * 48_000 / TICKS_PER_SECOND) as usize;
                let chunk: Vec<f32> = (0..480)
                    .flat_map(|i| {
                        let v = (2.0 * std::f32::consts::PI * 440.0 * (n0 + i) as f32 / 48_000.0)
                            .sin()
                            * 0.5;
                        [v, v]
                    })
                    .collect();
                encoder.submit_audio(0, &chunk, audio_time).expect("audio");
                audio_time += 100_000;
            }
            let mut tries = 0;
            loop {
                match encoder
                    .submit(&texture, (w, h), time, frame)
                    .expect("submit")
                {
                    SubmitOutcome::Submitted => break,
                    SubmitOutcome::Dropped => {
                        tries += 1;
                        if tries > 2000 {
                            dropped += 1;
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(1));
                    }
                }
            }
            let before = packets.len();
            encoder.poll(&mut packets).expect("poll");
            if encoder.layout().is_none() {
                early += packets.len() - before;
            }
        }
        let layout_before_finish = encoder.layout();
        encoder.finish(&mut packets).expect("finish");
        let layout = layout_before_finish.expect("layout known before finish");
        Some(Run {
            packets,
            layout,
            early,
            dropped,
            name,
        })
    }

    fn check(run: &Run, seconds: u32, b_frames: bool) {
        println!("encoder: {}", run.name);
        assert_eq!(run.early, 0, "packet returned before layout");
        assert_eq!(run.dropped, 0);
        let Some(CodecConfig::H264 {
            width,
            height,
            sps,
            pps,
        }) = run.layout.video().map(|t| t.codec.clone())
        else {
            panic!("track 0 must be H.264");
        };
        assert_eq!((width, height), (1280, 720));
        assert!(!sps.is_empty() && sps[0] & 0x1f == 7);
        assert!(!pps.is_empty() && pps[0] & 0x1f == 8);
        let Some(CodecConfig::Aac {
            sample_rate,
            channels,
            audio_specific_config,
        }) = run.layout.tracks.get(1).map(|t| t.codec.clone())
        else {
            panic!("track 1 must be AAC");
        };
        assert_eq!((sample_rate, channels), (48_000, 2));
        println!("ASC {audio_specific_config:02x?}");
        assert_eq!(audio_specific_config, vec![0x11, 0x90]);

        let video: Vec<&EncodedPacket> = run.packets.iter().filter(|p| p.track == 0).collect();
        let audio: Vec<&EncodedPacket> = run.packets.iter().filter(|p| p.track == 1).collect();
        println!(
            "video packets {} audio packets {}",
            video.len(),
            audio.len()
        );
        assert_eq!(video.len() as u32, seconds * 60);
        assert!(video[0].keyframe, "first video packet must be a keyframe");
        let keys: Vec<i64> = video.iter().filter(|p| p.keyframe).map(|p| p.pts).collect();
        println!("keyframe pts {keys:?}");
        assert!(keys.len() >= 2 && keys.len() as u32 <= seconds);
        for pair in keys.windows(2) {
            let gap = (pair[1] - pair[0]) as f64 / TICKS_PER_SECOND as f64;
            assert!((1.5..=2.5).contains(&gap), "gop gap {gap}");
        }
        let mut reordered = false;
        for pair in video.windows(2) {
            assert!(pair[1].dts > pair[0].dts, "dts must increase");
            reordered |= pair[1].pts < pair[0].pts;
        }
        for p in &video {
            assert!(p.pts >= p.dts && p.duration > 0);
            assert!(avcc_tiles(&p.data));
        }
        println!("pts reordered (B-frames): {reordered}");
        if !b_frames {
            assert!(!reordered);
        }
        let expected = (seconds as f64 * 48_000.0 / 1024.0) as i64;
        assert!(
            (audio.len() as i64 - expected).abs() <= 3,
            "audio packets {} vs ~{expected}",
            audio.len()
        );
        for pair in audio.windows(2) {
            assert!(pair[1].dts > pair[0].dts);
            let gap = pair[1].pts - pair[0].pts;
            assert!((gap - AAC_FRAME_TICKS).abs() < 20_000, "audio gap {gap}");
        }
        assert!(audio.iter().all(|p| p.keyframe && !p.data.is_empty()));
        println!(
            "audio first pts {} last pts {}",
            audio[0].pts,
            audio.last().map_or(0, |p| p.pts)
        );
    }

    /// AVCC payload sanity: lengths tile the buffer exactly.
    fn avcc_tiles(data: &[u8]) -> bool {
        let mut rest = data;
        while !rest.is_empty() {
            if rest.len() < 4 {
                return false;
            }
            let len = u32::from_be_bytes([rest[0], rest[1], rest[2], rest[3]]) as usize;
            if rest.len() < 4 + len {
                return false;
            }
            rest = &rest[4 + len..];
        }
        true
    }

    fn small_config() -> EncoderConfig {
        EncoderConfig {
            input_width: 640,
            input_height: 360,
            output_width: 640,
            output_height: 360,
            fps: 30,
            bitrate_mbps: 4,
            profile: QualityProfile::Balanced,
        }
    }

    #[test]
    fn encodes_replay_packets_performance() {
        let Some(env) = env() else { return };
        let Some(run) = run(&env, QualityProfile::Performance, 3) else {
            return;
        };
        check(&run, 3, false);
    }

    #[test]
    fn quality_profile_keeps_ordering_invariants() {
        let Some(env) = env() else { return };
        let Some(run) = run(&env, QualityProfile::Quality, 3) else {
            return;
        };
        check(&run, 3, true);
    }

    #[test]
    fn drop_without_finish_is_clean() {
        let Some(env) = env() else { return };
        let Ok(mut encoder) = PacketEncoder::create(&env.mf, &env.d3d, &small_config(), 2) else {
            return;
        };
        let (texture, view) = source(&env.d3d, 640, 360);
        draw(&env.d3d, &view, 1, 640, 360);
        let _ = encoder.submit(&texture, (640, 360), 0, 333_333);
        drop(encoder);
    }

    #[test]
    fn rejects_bad_audio() {
        let Some(env) = env() else { return };
        let Ok(mut encoder) = PacketEncoder::create(&env.mf, &env.d3d, &small_config(), 1) else {
            return;
        };
        assert!(encoder.submit_audio(0, &[0.0; 3], 0).is_err());
        assert!(encoder.submit_audio(1, &[0.0; 4], 0).is_err());
        assert!(encoder.submit_audio(0, &[0.0; 4], -1).is_err());
    }

    #[test]
    fn dts_assigner_orders_b_frame_streams() {
        // Decode order I0 P3 B1 B2 P6 B4 B5 (pts in frames), depth 2.
        let mut assigner = DtsAssigner::new(2, 10);
        let pts = [0i64, 30, 10, 20, 60, 40, 50];
        let dts: Vec<i64> = pts.iter().map(|p| assigner.assign(*p, None)).collect();
        assert!(dts.windows(2).all(|w| w[1] > w[0]), "{dts:?}");
        assert!(pts.iter().zip(&dts).all(|(p, d)| d <= p), "{dts:?}");
        // Equal external timestamps are nudged to stay strictly increasing.
        let mut assigner = DtsAssigner::new(0, 10);
        assert_eq!(assigner.assign(5, Some(5)), 5);
        assert_eq!(assigner.assign(5, Some(5)), 6);
    }

    #[test]
    fn encoder_ignoring_no_b_frames_switches_to_derived_dts() {
        // Configured without B-frames, but the encoder emits I0 P3 B1 B2 ...
        let mut assigner = DtsAssigner::new(0, 10);
        let pts: Vec<i64> = (0..30)
            .flat_map(|g| [g * 30 + 30, g * 30 + 10, g * 30 + 20])
            .collect();
        let mut stream = vec![0i64];
        stream.extend(pts);
        let dts: Vec<i64> = stream.iter().map(|p| assigner.assign(*p, None)).collect();
        assert_eq!(assigner.switches, 1);
        assert!(assigner.depth >= 2);
        // Only the packet right at the switch can be unrepresentable; every
        // later packet keeps dts strictly increasing and never above pts.
        let settled = 8;
        assert!(dts[settled..].windows(2).all(|w| w[1] > w[0]), "{dts:?}");
        assert!(
            stream[settled..]
                .iter()
                .zip(&dts[settled..])
                .all(|(p, d)| d <= p),
            "{dts:?}"
        );
        assert_eq!(dts[0], 0, "packets before the switch keep their dts");
    }

    #[test]
    fn reorder_deeper_than_configured_grows_the_depth_not_the_pts() {
        // Depth 1 configured, pyramid stream needs more: P4 B2 B1 B3 P8 ...
        let mut assigner = DtsAssigner::new(1, 10);
        let mut stream = vec![0i64];
        for g in 0..20 {
            let base = g * 40;
            stream.extend([base + 40, base + 20, base + 10, base + 30]);
        }
        let dts: Vec<i64> = stream.iter().map(|p| assigner.assign(*p, None)).collect();
        assert!(assigner.violations > 0 && assigner.depth >= 2, "{dts:?}");
        let tail = stream.len() - 20;
        assert!(dts[tail..].windows(2).all(|w| w[1] > w[0]), "{dts:?}");
        assert!(
            stream[tail..].iter().zip(&dts[tail..]).all(|(p, d)| d <= p),
            "{dts:?}"
        );
    }

    #[test]
    fn aac_time_follows_the_output_unless_it_diverges_by_a_frame() {
        let first = 1_000_000;
        let expected = aac_expected_time(first, 100);
        assert_eq!(aac_expected_time(first, 0), first);
        // 100 frames of 1024 samples at 48 kHz, exactly.
        assert_eq!(expected, first + 21_333_333);
        assert_eq!(
            aac_presentation_time(Some(expected + 5), expected),
            (expected + 5, false)
        );
        assert_eq!(
            aac_presentation_time(Some(expected + AAC_FRAME_TICKS), expected),
            (expected + AAC_FRAME_TICKS, false)
        );
        assert_eq!(
            aac_presentation_time(Some(expected + AAC_FRAME_TICKS + 1), expected),
            (expected, true)
        );
        assert_eq!(aac_presentation_time(Some(0), expected), (expected, true));
        assert_eq!(aac_presentation_time(None, expected), (expected, false));
    }

    #[test]
    fn scales_smaller_content_into_output() {
        let Some(env) = env() else { return };
        let config = EncoderConfig {
            input_width: 1920,
            input_height: 1080,
            output_width: 1280,
            output_height: 720,
            fps: 30,
            bitrate_mbps: 8,
            profile: QualityProfile::Balanced,
        };
        let Ok(mut encoder) = PacketEncoder::create(&env.mf, &env.d3d, &config, 0) else {
            return;
        };
        let (texture, view) = source(&env.d3d, 1920, 1080);
        let mut packets = Vec::new();
        for index in 0..30u32 {
            draw(&env.d3d, &view, index, 1920, 1080);
            let time = i64::from(index) * 333_333;
            while encoder
                .submit(&texture, (1600, 900), time, 333_333)
                .expect("submit")
                == SubmitOutcome::Dropped
            {
                encoder.poll(&mut packets).expect("poll");
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        encoder.finish(&mut packets).expect("finish");
        assert_eq!(packets.len(), 30);
        assert!(packets[0].keyframe);
    }
}
