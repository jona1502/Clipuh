//! Public API integration from persisted TOML through the actual encoder targets.
use clipper_storage::AppConfig;

#[test]
fn persisted_settings_project_quality_mixed_audio_and_hybrid_capacity() {
    let config: AppConfig = toml::from_str(
        r#"
[video]
width = 1600
height = 900
fps = 120
bitrate_mbps = 16
[recording]
quality = "quality"
[audio]
separate_tracks = false
[replay]
duration_seconds = 120
memory_limit_megabytes = 64
"#,
    )
    .expect("deserialize settings");

    let projection = config.storage_projection().expect("validated projection");
    assert_eq!(projection.video_bps, 24_000_000);
    assert_eq!(projection.audio_tracks, 1);
    assert_eq!(projection.audio_bps, 192_000);
    assert!(projection.variable_bitrate);
    assert_eq!(
        projection.sizes,
        [90_720_000, 181_440_000, 362_880_000, 907_200_000]
    );
    assert_eq!(projection.replay_duration_seconds, 120);
    assert_eq!(projection.replay.total_bytes, 362_880_000);
    assert_eq!(projection.replay.ram_budget_bytes, 67_108_864);
    assert_eq!(projection.replay.estimated_ssd_bytes, 295_771_136);
}
