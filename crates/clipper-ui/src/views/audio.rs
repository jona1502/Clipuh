//! Audio device controls. Enumeration and persistence run off the UI thread.
use super::ClipperWindow;
use crate::components::{chip, panel, property_row, section_header};
use crate::theme::ActiveTheme;
use clipper_audio::DeviceFlow;
use clipper_core::recording::{RecorderCommand, RecordingAudioOptions};
use gpui::prelude::*;
use gpui::{Context, Div, div, px};

impl ClipperWindow {
    pub(super) fn refresh_audio_devices(&mut self, cx: &mut Context<Self>) {
        if self.audio_loading {
            return;
        }
        self.audio_loading = true;
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async {
                    let mut devices = clipper_audio::windows::list_devices(DeviceFlow::Capture)
                        .map_err(|e| e.to_string())?;
                    devices.extend(
                        clipper_audio::windows::list_devices(DeviceFlow::Render)
                            .map_err(|e| e.to_string())?,
                    );
                    Ok::<_, String>(devices)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.audio_loading = false;
                match result {
                    Ok(devices) => this.audio_devices = devices,
                    Err(message) => this.recording.fail(message),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn start_audio_test(&mut self, microphone: bool, cx: &mut Context<Self>) {
        let options = match self.config.recording_options() {
            Ok(options) => options,
            Err(error) => {
                self.recording.fail(error.to_string());
                cx.notify();
                return;
            }
        };
        let audio = RecordingAudioOptions {
            desktop: !microphone,
            microphone,
            separate_tracks: false,
            ..options.audio
        };
        if self.send(RecorderCommand::StartAudioTest {
            options: audio,
            output_directory: options.output_directory,
        }) {
            self.audio_testing = true;
            self.audio_test_path = None;
            cx.notify();
        }
    }
}

fn device_picker(window: &ClipperWindow, flow: DeviceFlow, cx: &mut Context<ClipperWindow>) -> Div {
    let t = *cx.theme();
    let selected = match flow {
        DeviceFlow::Capture => &window.config.audio.input_device,
        DeviceFlow::Render => &window.config.audio.output_device,
    };
    let name = selected
        .as_ref()
        .map_or("Windows default".to_owned(), |id| {
            window
                .audio_devices
                .iter()
                .find(|d| &d.id == id && d.flow == flow)
                .map_or("Unavailable device — select again".to_owned(), |d| {
                    d.name.clone()
                })
        });
    let enabled = window.recording.settings_enabled() && !window.audio_testing;
    let mut root = div()
        .flex()
        .flex_col()
        .gap_1()
        .min_w(px(260.))
        .max_w(px(520.))
        .child(
            chip(
                &t,
                if flow == DeviceFlow::Capture {
                    "input-picker"
                } else {
                    "output-picker"
                },
                format!("{name} ▾"),
                false,
                enabled,
            )
            .when(enabled, |item| {
                item.on_click(cx.listener(move |this, _, _, cx| {
                    this.audio_menu = if this.audio_menu == Some(flow) {
                        None
                    } else {
                        Some(flow)
                    };
                    this.refresh_audio_devices(cx);
                    cx.notify();
                }))
            }),
        );
    if window.audio_menu == Some(flow) && enabled {
        let choices = std::iter::once(None).chain(
            window
                .audio_devices
                .iter()
                .filter(|d| d.flow == flow)
                .map(Some),
        );
        for (index, device) in choices.enumerate() {
            let id = device.map(|d| d.id.clone());
            let label = device.map_or("Windows default".to_owned(), |d| d.name.clone());
            root = root.child(
                chip(
                    &t,
                    (
                        if flow == DeviceFlow::Capture {
                            "input-option"
                        } else {
                            "output-option"
                        },
                        index,
                    ),
                    label,
                    selected == &id,
                    true,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    match flow {
                        DeviceFlow::Capture => this.config.audio.input_device = id.clone(),
                        DeviceFlow::Render => this.config.audio.output_device = id.clone(),
                    }
                    this.audio_menu = None;
                    this.persist_config(cx);
                })),
            );
        }
    }
    root
}

pub(super) fn content(window: &ClipperWindow, cx: &mut Context<ClipperWindow>) -> Div {
    let t = *cx.theme();
    let enabled = window.recording.settings_enabled() && !window.audio_testing;
    let audio = &window.config.audio;
    let mut body = panel(&t)
        .child(property_row(
            &t,
            "Microphone input",
            Some("A capture device, never the playback endpoint.".into()),
            device_picker(window, DeviceFlow::Capture, cx),
            true,
        ))
        .child(property_row(
            &t,
            "Desktop output",
            Some("WASAPI loopback of this playback device.".into()),
            device_picker(window, DeviceFlow::Render, cx),
            false,
        ));
    for (index, (label, on)) in [
        ("Record microphone", audio.microphone),
        ("Record desktop audio", audio.desktop),
        ("Isolated editing tracks", audio.separate_tracks),
    ]
    .into_iter()
    .enumerate()
    {
        body = body.child(property_row(
            &t,
            label,
            None,
            chip(
                &t,
                ("audio-toggle", index),
                if on { "On" } else { "Off" },
                on,
                enabled,
            )
            .when(enabled, |item| {
                item.on_click(cx.listener(move |this, _, _, cx| {
                    match index {
                        0 => this.config.audio.microphone = !this.config.audio.microphone,
                        1 => this.config.audio.desktop = !this.config.audio.desktop,
                        _ => this.config.audio.separate_tracks = !this.config.audio.separate_tracks,
                    }
                    this.persist_config(cx);
                }))
            }),
            false,
        ));
    }
    for (index, (label, gain)) in [
        ("Microphone gain", audio.microphone_gain_percent),
        ("Desktop gain", audio.desktop_gain_percent),
    ]
    .into_iter()
    .enumerate()
    {
        body = body.child(property_row(
            &t,
            label,
            None,
            div()
                .flex()
                .gap_1()
                .children([0_u16, 50, 100, 150, 200].into_iter().map(|value| {
                    chip(
                        &t,
                        (
                            if index == 0 {
                                "mic-gain"
                            } else {
                                "desktop-gain"
                            },
                            value as usize,
                        ),
                        format!("{value}%"),
                        gain == value,
                        enabled,
                    )
                    .when(enabled, |item| {
                        item.on_click(cx.listener(move |this, _, _, cx| {
                            if index == 0 {
                                this.config.audio.microphone_gain_percent = value;
                            } else {
                                this.config.audio.desktop_gain_percent = value;
                            }
                            this.persist_config(cx);
                        }))
                    })
                })),
            false,
        ));
    }
    for (index, (label, peak)) in [
        ("Microphone peak", window.audio_levels.1),
        ("Desktop peak", window.audio_levels.0),
    ]
    .into_iter()
    .enumerate()
    {
        body = body.child(property_row(
            &t,
            label,
            Some("Live during recording or the selected source test.".into()),
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div().w(px(120.)).h(px(5.)).bg(t.colors.border).child(
                        div()
                            .h_full()
                            .w(px(120. * peak.clamp(0.0, 1.0)))
                            .bg(t.colors.accent),
                    ),
                )
                .child(
                    chip(&t, ("audio-test", index), "Test 30 s", false, enabled).when(
                        enabled,
                        |item| {
                            item.on_click(cx.listener(move |this, _, _, cx| {
                                this.start_audio_test(index == 0, cx)
                            }))
                        },
                    ),
                ),
            false,
        ));
    }
    let mut root = div().flex().flex_col().gap_2().child(section_header(&t, "Audio", None)).child(body)
        .child(div().text_xs().text_color(t.colors.text_muted).child("Primary AAC track: desktop + microphone mix. Optional extra tracks are isolated alternatives for editing. Game/Discord are included once in desktop playback; separate process routing and Instant Replay are not integrated. Settings apply to the next recording."))
        .child(chip(&t, "refresh-audio", if window.audio_loading { "Refreshing…" } else { "Refresh devices" }, false, !window.audio_loading).on_click(cx.listener(|this, _, _, cx| this.refresh_audio_devices(cx))));
    if window.audio_testing {
        root = root.child(
            chip(&t, "stop-audio-test", "Stop audio test", true, true).on_click(cx.listener(
                |this, _, _, cx| {
                    this.send(RecorderCommand::StopAudioTest);
                    cx.notify();
                },
            )),
        );
    }
    if let Some(path) = &window.audio_test_path {
        root = root.child(div().text_xs().child(format!("Test WAV saved: {path}")));
    }
    root
}
