//! Clip-saved sound settings: defaults, persistence and safe loading.

use clipper_storage::{AppConfig, SoundChoice, is_plain_file_name};

#[test]
fn existing_files_default_to_the_bundled_sound() {
    let config: AppConfig =
        toml::from_str("[notifications]\nsound = true\n").expect("old file parses");
    assert_eq!(config.notifications.sound_choice, SoundChoice::Default);
    assert_eq!(config.notifications.custom_sound, None);
    assert!(config.notifications.sound);
    assert_eq!(config.notifications.selected_custom_sound(), None);
}

#[test]
fn custom_selection_round_trips_through_toml() {
    let mut config = AppConfig::default();
    config.notifications.sound_choice = SoundChoice::Custom;
    config.notifications.custom_sound = Some("Ping Ünïcode-0123456789abcdef.wav".into());
    let text = toml::to_string(&config).expect("serialize");
    let loaded: AppConfig = toml::from_str(&text).expect("parse");
    assert_eq!(loaded.notifications, config.notifications);
    assert_eq!(
        loaded.notifications.selected_custom_sound(),
        Some("Ping Ünïcode-0123456789abcdef.wav")
    );
}

#[test]
fn paths_and_unknown_values_are_dropped_instead_of_failing_the_file() {
    let config: AppConfig = toml::from_str(concat!(
        "[notifications]\n",
        "sound_choice = \"loud\"\n",
        r"custom_sound = '..\..\evil.wav'",
        "\n",
    ))
    .expect("tolerated");
    assert_eq!(config.notifications.sound_choice, SoundChoice::Default);
    assert_eq!(config.notifications.custom_sound, None);
    for bad in ["", ".", "..", "a/b.wav", r"C:\x.wav", "c:x.wav"] {
        assert!(!is_plain_file_name(bad), "{bad:?}");
    }
    assert!(is_plain_file_name("ping-00ff.wav"));
}

#[test]
fn switching_back_to_default_keeps_the_imported_file_name() {
    let mut config = AppConfig::default();
    config.notifications.sound_choice = SoundChoice::Custom;
    config.notifications.custom_sound = Some("ping-1.wav".into());
    // Reset to default: selection only; the import stays available.
    config.notifications.sound_choice = SoundChoice::Default;
    assert_eq!(config.notifications.selected_custom_sound(), None);
    assert_eq!(
        config.notifications.custom_sound.as_deref(),
        Some("ping-1.wav")
    );
}
