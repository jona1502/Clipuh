//! Audio track controls, real cached waveform rendering and inspector state.
use super::*;
use crate::components::slider;

fn row_height(theme: &Theme) -> f32 {
    f32::from(theme.metrics.control_height_small) * 2. + 16.
}

impl ClipEditor {
    pub(super) fn sync_audio(&mut self, cx: &mut Context<Self>) {
        let Some(edits) = &self.edits else { return };
        let mut tracks = edits.preview_audio_tracks();
        // Segment selection is UI state, not a reason to flush the audio device.
        for track in &mut tracks {
            track.selected_segment = None;
        }
        if self.last_audio_mix.as_ref() != Some(&tracks)
            && let Some(player) = &self.player
        {
            player.set_audio_tracks(tracks.clone());
            self.last_audio_mix = Some(tracks);
        }
        let segment = edits
            .selected_audio()
            .and_then(|i| edits.audio_tracks().get(i))
            .and_then(|track| track.selected_segment.and_then(|i| track.segments.get(i)))
            .copied();
        let (a, b) = segment.map_or((String::new(), String::new()), |s| {
            (format_timestamp(s.start), format_timestamp(s.end))
        });
        self.audio_start_input
            .update(cx, |input, cx| input.set_text(a, cx));
        self.audio_end_input
            .update(cx, |input, cx| input.set_text(b, cx));
        cx.notify();
    }
    fn audio_toggle(&mut self, index: usize, control: usize, cx: &mut Context<Self>) {
        if let Some(edits) = &mut self.edits {
            edits.finish_audio_gesture();
            if let Some(track) = edits.audio_tracks().get(index) {
                let (enabled, muted, solo) = (track.enabled, track.muted, track.solo);
                let result = match control {
                    0 => edits.set_track_enabled(index, !enabled),
                    1 => edits.set_track_mute(index, !muted),
                    _ => edits.set_track_solo(index, !solo),
                };
                self.notice = result.err().map(Into::into);
            }
        }
        self.sync_audio(cx);
    }
    fn audio_gain(&mut self, index: usize, value: f32, cx: &mut Context<Self>) {
        if let Some(edits) = &mut self.edits {
            edits.begin_audio_gesture();
            self.notice = edits
                .set_track_gain(index, (value.clamp(0., 1.) * 200.).round() as u16)
                .err()
                .map(Into::into);
        }
        self.sync_audio(cx);
    }
    fn finish_audio_gain(&mut self, cx: &mut Context<Self>) {
        if let Some(edits) = &mut self.edits {
            edits.finish_audio_gesture();
        }
        cx.notify();
    }
    fn select_audio_track(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(edits) = &mut self.edits {
            edits.finish_audio_gesture();
            let _ = edits.select_audio(Some(index));
        }
        self.tab = 2;
        self.sync_audio(cx);
    }
    fn audio_controls(
        &self,
        index: usize,
        inspector: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let t = *cx.theme();
        let Some(track) = self
            .edits
            .as_ref()
            .and_then(|e| e.audio_tracks().get(index))
        else {
            return div();
        };
        let selected = track.clone();
        let mut controls = div().flex().items_center().gap_1();
        for (control, label, active) in [
            (
                0,
                if selected.enabled { "On" } else { "Off" },
                selected.enabled,
            ),
            (1, if inspector { "Mute" } else { "M" }, selected.muted),
            (2, if inspector { "Solo" } else { "S" }, selected.solo),
        ] {
            let prefix = match (inspector, control) {
                (true, 0) => "inspector-enable",
                (true, 1) => "inspector-mute",
                (true, _) => "inspector-solo",
                (false, 0) => "track-enable",
                (false, 1) => "track-mute",
                _ => "track-solo",
            };
            controls = controls.child(
                button(
                    &t,
                    (prefix, index),
                    label,
                    if active {
                        ButtonVariant::Primary
                    } else {
                        ButtonVariant::Secondary
                    },
                    None,
                    true,
                )
                .px_1()
                .h(t.metrics.control_height_small)
                .text_size(px(10.))
                .on_click(cx.listener(move |this, _, _, cx| this.audio_toggle(index, control, cx))),
            );
        }
        if let Some(focus) = self
            .audio_slider_focus
            .get(index)
            .map(|pair| if inspector { &pair.1 } else { &pair.0 })
        {
            let weak = cx.entity().downgrade();
            controls = controls.child(div().flex_1().min_w(px(46.)).child(slider(
                &t,
                (
                    if inspector {
                        "inspector-gain"
                    } else {
                        "track-gain"
                    },
                    index,
                ),
                f32::from(track.gain_percent) / 200.,
                track.enabled,
                focus,
                focus.is_focused(window),
                move |value, _, app| {
                    let _ = weak.update(app, |this, cx| this.audio_gain(index, value, cx));
                },
            )));
        }
        controls
            .child(
                muted(&t)
                    .w(px(35.))
                    .child(format!("{}%", track.gain_percent)),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.finish_audio_gain(cx)),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.finish_audio_gain(cx)),
            )
            .on_key_up(cx.listener(|this, _, _, cx| this.finish_audio_gain(cx)))
    }
    pub(super) fn audio_header(
        &self,
        index: usize,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let t = *cx.theme();
        let row_height = row_height(&t);
        let Some(edits) = &self.edits else {
            return div().h(px(row_height));
        };
        let Some(track) = edits.audio_tracks().get(index) else {
            return div().h(px(row_height));
        };
        let selected = edits.selected_audio() == Some(index);
        div()
            .h(px(row_height))
            .p_1()
            .rounded_md()
            .border_1()
            .border_color(if selected {
                t.colors.accent
            } else {
                t.colors.border
            })
            .bg(t.colors.surface_raised)
            .flex()
            .flex_col()
            .gap_1()
            .child(
                button(
                    &t,
                    ("audio-select", index),
                    track.label.clone(),
                    if selected {
                        ButtonVariant::Ghost
                    } else {
                        ButtonVariant::Secondary
                    },
                    None,
                    true,
                )
                .h(t.metrics.control_height_small)
                .px_1()
                .min_w_0()
                .overflow_hidden()
                .on_click(cx.listener(move |this, _, window, cx| {
                    window.focus(&this.focus);
                    this.select_audio_track(index, cx);
                })),
            )
            .child(self.audio_controls(index, false, window, cx))
    }
    pub(super) fn audio_inspector(&self, window: &Window, cx: &mut Context<Self>) -> Div {
        let t = *cx.theme();
        let Some((index, track)) = self.edits.as_ref().and_then(|e| {
            e.selected_audio()
                .and_then(|i| e.audio_tracks().get(i).map(|track| (i, track)))
        }) else {
            return muted(&t).child(if self.audio_metadata.is_some() {
                "No audio streams in this clip."
            } else {
                "Reading audio streams…"
            });
        };
        let editable = track.selected_segment.is_some();
        let split = button(
            &t,
            "audio-split",
            "Split",
            ButtonVariant::Secondary,
            None,
            editable,
        )
        .when(editable, |el| {
            el.on_click(cx.listener(|this, _, _, cx| {
                if let Some(e) = &mut this.edits {
                    this.notice = e.split_audio(this.position).err().map(Into::into);
                }
                this.sync_audio(cx);
            }))
        });
        let delete = button(
            &t,
            "audio-delete",
            "Delete segment",
            ButtonVariant::Secondary,
            None,
            editable,
        )
        .when(editable, |el| {
            el.on_click(cx.listener(|this, _, _, cx| {
                if let Some(e) = &mut this.edits {
                    this.notice = e.delete_audio().err().map(Into::into);
                }
                this.sync_audio(cx);
            }))
        });
        div().flex().flex_col().gap_3()
            .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(track.label.clone()))
            .child(muted(&t).child(format!("Source stream {} · {} channels · {} Hz",track.stream_index,track.channels,track.sample_rate)))
            .child(self.audio_controls(index,true,window,cx))
            .child(super::presentation::timestamp_row(&t,"Start",self.audio_start_input.clone()))
            .child(super::presentation::timestamp_row(&t,"End",self.audio_end_input.clone()))
            .child(muted(&t).child("Source-time cuts. Video ranges remain linked; audio cuts do not move other tracks."))
            .child(div().flex().flex_wrap().gap_2().child(split).child(delete))
            .child(muted(&t).child("Solo includes enabled, unmuted solo tracks. Mute wins. Gain 0–200%; final mix is limited to prevent overload."))
    }
    pub(super) fn audio_waveform(
        &self,
        index: usize,
        duration: Duration,
        cx: &mut Context<Self>,
    ) -> Div {
        let t = *cx.theme();
        let row_height = row_height(&t);
        let Some(edits) = &self.edits else {
            return div().h(px(row_height));
        };
        let Some(track) = edits.audio_tracks().get(index).cloned() else {
            return div().h(px(row_height));
        };
        let selected = edits.selected_audio() == Some(index);
        let gain = clipper_editor::effective_gain(edits.audio_tracks(), track.stream_index);
        let waveform = self.waveforms.clone();
        let source_ranges = edits.segments().to_vec();
        let draw_track = track.clone();
        let mut row = div()
            .id(("audio-waveform", index))
            .relative()
            .h(px(row_height))
            .w_full()
            .rounded_md()
            .overflow_hidden()
            .bg(t.colors.accent_subtle)
            .border_1()
            .border_color(if selected {
                t.colors.accent
            } else {
                t.colors.border
            })
            .child(
                canvas(
                    move |bounds, _, _| bounds,
                    move |bounds, _, window, _| {
                        let Some(waveforms) = &waveform else { return };
                        let Some(data) = waveforms
                            .tracks
                            .iter()
                            .find(|w| w.stream_index == draw_track.stream_index)
                        else {
                            return;
                        };
                        if data.buckets.is_empty() || waveforms.duration.is_zero() {
                            return;
                        }
                        let width = f32::from(bounds.size.width).max(1.);
                        let columns = (width.ceil() as usize).min(2048);
                        for x in 0..columns {
                            let at = duration.mul_f64((x as f64 + 0.5) / columns as f64);
                            if !draw_track.contains(at)
                                || !source_ranges.iter().any(|s| at >= s.start && at < s.end)
                            {
                                continue;
                            }
                            let from = ((duration.as_secs_f64() * x as f64
                                / columns as f64
                                / waveforms.duration.as_secs_f64())
                                * data.buckets.len() as f64)
                                as usize;
                            let to = ((duration.as_secs_f64() * (x + 1) as f64
                                / columns as f64
                                / waveforms.duration.as_secs_f64())
                                * data.buckets.len() as f64)
                                .ceil() as usize;
                            let peak = data
                                .buckets
                                .get(
                                    from.min(data.buckets.len())
                                        ..to.min(data.buckets.len())
                                            .max(from.min(data.buckets.len())),
                                )
                                .map_or(0., |b| b.iter().map(|b| b.peak).fold(0f32, f32::max));
                            let amplitude =
                                (peak * if gain > 0. { gain } else { 1. }).clamp(0., 1.);
                            if amplitude == 0. {
                                continue;
                            }
                            let height = (amplitude * (row_height - 12.)).max(1.);
                            let bar = Bounds::new(
                                gpui::point(
                                    bounds.origin.x + px(x as f32 * width / columns as f32),
                                    bounds.origin.y + px((row_height - height) / 2.),
                                ),
                                gpui::size(px((width / columns as f32).max(1.)), px(height)),
                            );
                            window.paint_quad(gpui::fill(
                                bar,
                                if gain > 0. {
                                    t.colors.accent
                                } else {
                                    t.colors.text_disabled
                                },
                            ));
                        }
                    },
                )
                .absolute()
                .size_full(),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
                    window.focus(&this.focus);
                    this.select_audio_track(index, cx);
                    let at = this
                        .duration
                        .unwrap_or_default()
                        .mul_f32(position_fraction(event.position.x, this.timeline.get()));
                    if let Some(e) = &mut this.edits {
                        let segment = e.audio_tracks().get(index).and_then(|t| {
                            t.segments.iter().position(|s| at >= s.start && at < s.end)
                        });
                        if let Some(segment) = segment {
                            let _ = e.select_audio_segment(segment);
                        }
                    }
                    this.sync_audio(cx);
                    this.scrubbing = true;
                    this.seek_to_x(event.position.x, cx);
                    cx.stop_propagation();
                }),
            );
        for (i, segment) in track.segments.iter().enumerate() {
            row = row.child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(relative(fraction(segment.start, duration)))
                    .w(relative(fraction(segment.end - segment.start, duration)))
                    .border_x_1()
                    .border_color(if selected && track.selected_segment == Some(i) {
                        t.colors.accent
                    } else {
                        t.colors.border_strong
                    }),
            );
        }
        if self.waveforms.is_none() {
            row = row.child(muted(&t).p_2().child(if self.waveform_error.is_some() {
                "Waveform unavailable"
            } else {
                "Generating waveform…"
            }));
        }
        div().h(px(row_height)).child(row)
    }
}
