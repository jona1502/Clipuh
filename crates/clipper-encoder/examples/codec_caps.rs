//! Lists what each hardware H.264 encoder MFT reports through ICodecAPI for
//! the properties relevant to encoder tuning: supported, modifiable, default
//! value and the allowed range or value list.
//!
//! `cargo run --release -p clipper-encoder --example codec_caps`
//!
//! "Supported" only means the MFT accepts the property; whether it changes
//! the output has to be checked in the bitstream (`encoder_ab` + the
//! scripts in `scripts/perf/`).
#[cfg(windows)]
#[allow(unsafe_code)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use clipper_encoder::backends::media_foundation::MediaFoundation;
    use clipper_platform::windows::ComApartment;
    use windows::Win32::Media::MediaFoundation::*;
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::System::Variant::VARIANT;
    use windows::core::{GUID, Interface, PWSTR};

    let _com = ComApartment::init_multithreaded()?;
    let _mf = MediaFoundation::start()?;
    let properties: &[(&str, GUID)] = &[
        ("RateControlMode", CODECAPI_AVEncCommonRateControlMode),
        ("QualityVsSpeed", CODECAPI_AVEncCommonQualityVsSpeed),
        ("Quality", CODECAPI_AVEncCommonQuality),
        ("MeanBitRate", CODECAPI_AVEncCommonMeanBitRate),
        ("MaxBitRate", CODECAPI_AVEncCommonMaxBitRate),
        ("BufferSize", CODECAPI_AVEncCommonBufferSize),
        ("LowLatency(Common)", CODECAPI_AVEncCommonLowLatency),
        ("RealTime", CODECAPI_AVEncCommonRealTime),
        ("MultipassMode", CODECAPI_AVEncCommonMultipassMode),
        ("GOPSize", CODECAPI_AVEncMPVGOPSize),
        ("GOPOpen", CODECAPI_AVEncMPVGOPOpen),
        ("BPictureCount", CODECAPI_AVEncMPVDefaultBPictureCount),
        ("SceneDetection", CODECAPI_AVEncMPVSceneDetection),
        ("MaxNumRefFrame", CODECAPI_AVEncVideoMaxNumRefFrame),
        ("CABACEnable", CODECAPI_AVEncH264CABACEnable),
        ("EncodeQP", CODECAPI_AVEncVideoEncodeQP),
        ("MinQP", CODECAPI_AVEncVideoMinQP),
        ("MaxQP", CODECAPI_AVEncVideoMaxQP),
        ("LTRBufferControl", CODECAPI_AVEncVideoLTRBufferControl),
        ("MaxTemporalLayers", CODECAPI_AVEncVideoMaxTemporalLayers),
        ("TemporalLayerCount", CODECAPI_AVEncVideoTemporalLayerCount),
        ("SliceControlMode", CODECAPI_AVEncSliceControlMode),
        ("SliceControlSize", CODECAPI_AVEncSliceControlSize),
        ("AdaptiveMode", CODECAPI_AVEncAdaptiveMode),
        ("ContentType", CODECAPI_AVEncVideoContentType),
        ("Usage", CODECAPI_AVEncVideoUsage),
        ("LowPowerEncoder", CODECAPI_AVEncLowPowerEncoder),
        ("NumWorkerThreads", CODECAPI_AVEncNumWorkerThreads),
        (
            "MaxKeyframeDistance",
            CODECAPI_AVEncVideoMaxKeyframeDistance,
        ),
        ("NumGOPsPerIDR", CODECAPI_AVEncVideoNumGOPsPerIDR),
        ("MPVProfile", CODECAPI_AVEncMPVProfile),
        ("MPVLevel", CODECAPI_AVEncMPVLevel),
        ("LowLatencyMode", CODECAPI_AVLowLatencyMode),
    ];
    let text = |v: &VARIANT| -> String {
        if let Ok(x) = u32::try_from(v) {
            x.to_string()
        } else if let Ok(x) = u64::try_from(v) {
            x.to_string()
        } else if let Ok(x) = i32::try_from(v) {
            x.to_string()
        } else if let Ok(x) = bool::try_from(v) {
            x.to_string()
        } else if v.is_empty() {
            "-".into()
        } else {
            "?".into()
        }
    };

    let output = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_H264,
    };
    let mut activates: *mut Option<IMFActivate> = std::ptr::null_mut();
    let mut count = 0u32;
    // SAFETY: out-pointers reference live locals.
    unsafe {
        MFTEnumEx(
            MFT_CATEGORY_VIDEO_ENCODER,
            MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER,
            None,
            Some(&output),
            &mut activates,
            &mut count,
        )?;
    }
    // SAFETY: MFTEnumEx returned `count` interface pointers in a CoTaskMem array.
    let items = unsafe { std::slice::from_raw_parts_mut(activates, count as usize) };
    for activate in items.iter_mut().filter_map(Option::take) {
        let string = |key: &GUID| -> String {
            let mut buffer = PWSTR::null();
            let mut length = 0u32;
            // SAFETY: valid attributes; the buffer is freed after copying.
            unsafe {
                if activate
                    .GetAllocatedString(key, &mut buffer, &mut length)
                    .is_err()
                {
                    return "?".into();
                }
                let text = buffer.to_string().unwrap_or_default();
                CoTaskMemFree(Some(buffer.0 as *const _));
                text
            }
        };
        println!(
            "== {}  vendor={}",
            string(&MFT_FRIENDLY_NAME_Attribute),
            string(&MFT_ENUM_HARDWARE_VENDOR_ID_Attribute)
        );
        // SAFETY: plain COM calls on the activated transform.
        let api: ICodecAPI = unsafe { activate.ActivateObject::<IMFTransform>()?.cast()? };
        for (name, key) in properties {
            // SAFETY: valid ICodecAPI; raw HRESULTs distinguish S_OK/S_FALSE.
            let (supported, modifiable) = unsafe {
                let vt = Interface::vtable(&api);
                (
                    (vt.IsSupported)(Interface::as_raw(&api), key).0 == 0,
                    (vt.IsModifiable)(Interface::as_raw(&api), key).0 == 0,
                )
            };
            if !supported {
                println!("  {name:20} unsupported");
                continue;
            }
            // SAFETY: valid ICodecAPI and key; outputs are owned VARIANTs.
            let (value, range, values) = unsafe {
                let value = api
                    .GetValue(key)
                    .map(|v| text(&v))
                    .unwrap_or_else(|_| "-".into());
                let (mut min, mut max, mut step) =
                    (VARIANT::default(), VARIANT::default(), VARIANT::default());
                let range = api
                    .GetParameterRange(key, &mut min, &mut max, &mut step)
                    .map(|()| format!("{}..{} step {}", text(&min), text(&max), text(&step)))
                    .unwrap_or_default();
                let mut list: *mut VARIANT = std::ptr::null_mut();
                let mut n = 0u32;
                let values =
                    if api.GetParameterValues(key, &mut list, &mut n).is_ok() && !list.is_null() {
                        let slice = std::slice::from_raw_parts(list, n as usize);
                        let joined = slice.iter().map(text).collect::<Vec<_>>().join(",");
                        CoTaskMemFree(Some(list as *const _));
                        joined
                    } else {
                        String::new()
                    };
                (value, range, values)
            };
            println!(
                "  {name:20} supported modifiable={modifiable} value={value} range=[{range}] values=[{values}]"
            );
        }
    }
    // SAFETY: array allocated by CoTaskMemAlloc; all elements were taken.
    unsafe { CoTaskMemFree(Some(activates as *const _)) };
    Ok(())
}

#[cfg(not(windows))]
fn main() {
    eprintln!("codec_caps needs Windows");
}
