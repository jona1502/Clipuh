//! Audio track controls, real cached waveform rendering and inspector state.
use super::*;
use crate::components::slider;

/// Peak of every drawn waveform column, cached per track and layout so a
/// redraw (the playhead moves every display refresh while playing) does not
/// re-scan all buckets. Bounded: old layouts (zoom levels, widths) are
/// evicted first.
#[derive(Default)]
pub(super) struct PeakCache {
    entries: Vec<(PeakKey, Arc<[f32]>)>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct PeakKey {
    stream_index: u32,
    columns: usize,
    duration: Duration,
    /// Visible slice of the time axis, in thousandths of its width (the
    /// waveform is computed for what is on screen only).
    from_milli: u32,
    to_milli: u32,
    /// Identity of the decoded waveform set (replaced when re-analysed).
    source: usize,
}

const PEAK_CACHE_ENTRIES: usize = 16;

impl PeakCache {
    fn get(&mut self, key: PeakKey, compute: impl FnOnce() -> Vec<f32>) -> Arc<[f32]> {
        if let Some((_, peaks)) = self.entries.iter().find(|(k, _)| *k == key) {
            return peaks.clone();
        }
        let peaks: Arc<[f32]> = compute().into();
        if self.entries.len() >= PEAK_CACHE_ENTRIES {
            self.entries.remove(0);
        }
        self.entries.push((key, peaks.clone()));
        peaks
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.len()
    }
}

/// Largest bucket peak under each of `columns` equal slices of `duration`
/// (the timeline), for buckets spanning `source_duration`.
#[cfg(test)]
fn column_peaks(
    buckets: &[clipper_media::audio::WaveformBucket],
    duration: Duration,
    source_duration: Duration,
    columns: usize,
) -> Vec<f32> {
    column_peaks_range(buckets, duration, source_duration, 0., 1., columns)
}

/// Like `column_peaks` for the slice `from..to` (fractions of the time
/// axis): column `x` covers `duration * (from + (to - from) * x / columns)`.
fn column_peaks_range(
    buckets: &[clipper_media::audio::WaveformBucket],
    duration: Duration,
    source_duration: Duration,
    from: f64,
    to: f64,
    columns: usize,
) -> Vec<f32> {
    let len = buckets.len();
    if columns == 0 || source_duration.is_zero() {
        return Vec::new();
    }
    let bucket_at = |fraction: f64| {
        duration.as_secs_f64() * fraction / source_duration.as_secs_f64() * len as f64
    };
    let span = (to - from).max(0.);
    (0..columns)
        .map(|x| {
            let a = bucket_at(from + span * x as f64 / columns as f64) as usize;
            let b = bucket_at(from + span * (x + 1) as f64 / columns as f64).ceil() as usize;
            buckets
                .get(a.min(len)..b.min(len).max(a.min(len)))
                .map_or(0., |b| b.iter().map(|b| b.peak).fold(0f32, f32::max))
        })
        .collect()
}

fn row_height(theme: &Theme) -> f32 {
    f32::from(theme.metrics.control_height_small) * 2. + 16.
}

/// Waveform display floor: quieter peaks draw nothing.
const WAVEFORM_FLOOR_DB: f32 = -48.;

/// Bar height fraction for a linear peak on a dB scale (-48..0 dBFS), so quiet
/// game or voice tracks stay readable. Data stays real: silence draws nothing
/// and louder always draws taller.
fn display_level(peak: f32) -> f32 {
    if peak <= 0. || !peak.is_finite() {
        return 0.;
    }
    ((20. * peak.log10() - WAVEFORM_FLOOR_DB) / -WAVEFORM_FLOOR_DB).clamp(0., 1.)
}

/// Per-track waveform color derived from the user's accent: the first track
/// uses the accent itself, later tracks step around the hue wheel (purple,
/// blue, green... with the default accent), so custom accents still apply.
fn track_color(accent: gpui::Hsla, index: usize) -> gpui::Hsla {
    gpui::Hsla {
        h: (accent.h - 0.18 * index as f32).rem_euclid(1.),
        ..accent
    }
}

impl ClipEditor {
    pub(super) fn drag_audio(&mut self, x: Pixels, cx: &mut Context<Self>) {
        let Some((start, pointer, original)) = self.audio_dragging else {
            return;
        };
        let Some(duration) = self.duration else {
            return;
        };
        let width = f32::from(self.timeline.get().size.width);
        let origin = if start { original.start } else { original.end };
        let at = clipper_editor::pixel_to_time(
            clipper_editor::time_to_pixel(origin, duration, width) + f32::from(x - pointer),
            duration,
            width,
        );
        if let Some(edits) = &mut self.edits {
            let result = edits.trim_audio(
                if start { at } else { original.start },
                if start { original.end } else { at },
            );
            self.notice = result.err().map(Into::into);
        }
        self.sync_audio(cx);
    }
    pub(super) fn finish_audio_drag(&mut self, cx: &mut Context<Self>) {
        if self.audio_dragging.take().is_some() {
            if let Some(edits) = &mut self.edits {
                edits.finish_audio_gesture();
            }
            cx.notify();
        }
    }
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
    /// Height limits of audio rows (the header controls set the minimum).
    pub(super) fn audio_limits(t: &Theme) -> clipper_editor::timeline::HeightLimits {
        clipper_editor::timeline::HeightLimits::audio(row_height(t))
    }

    /// Height of audio row `index` (default until the user resizes it).
    pub(super) fn audio_row_height(&self, index: usize, t: &Theme) -> f32 {
        let limits = Self::audio_limits(t);
        self.edits
            .as_ref()
            .and_then(|e| e.audio_tracks().get(index))
            .map_or(limits.default, |track| {
                self.track_height(
                    clipper_editor::timeline::TrackId::Audio(track.stream_index),
                    limits,
                )
            })
    }

    pub(super) fn audio_header(
        &self,
        index: usize,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let t = *cx.theme();
        let row_height = self.audio_row_height(index, &t);
        let Some(edits) = &self.edits else {
            return div().h(px(row_height));
        };
        let Some(track) = edits.audio_tracks().get(index) else {
            return div().h(px(row_height));
        };
        let selected = edits.selected_audio() == Some(index);
        let resize = self.resize_handle(
            &t,
            clipper_editor::timeline::TrackId::Audio(track.stream_index),
            row_height,
            Self::audio_limits(&t),
            cx,
        );
        div()
            .relative()
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
                    clipper_editor::display_label(track).to_owned(),
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
            .child(resize)
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
            .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(clipper_editor::display_label(track).to_owned()))
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
        let row_height = self.audio_row_height(index, &t);
        let Some(edits) = &self.edits else {
            return div().h(px(row_height));
        };
        let Some(track) = edits.audio_tracks().get(index).cloned() else {
            return div().h(px(row_height));
        };
        let selected = edits.selected_audio() == Some(index);
        let gain = clipper_editor::effective_gain(edits.audio_tracks(), track.stream_index);
        let waveform = self.waveforms.clone();
        let peak_cache = self.waveform_peaks.clone();
        let source_ranges = edits.segments().to_vec();
        let draw_track = track.clone();
        let color = track_color(t.colors.accent, index);
        let mut row = div()
            .id(("audio-waveform", index))
            .relative()
            .h(px(row_height))
            .w_full()
            .rounded_md()
            .overflow_hidden()
            .bg(t.colors.surface_raised)
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
                        // Only the part inside the scrolled viewport is drawn,
                        // about one column per pixel: sharp at any zoom, with
                        // a bounded cost however long the timeline gets.
                        let width = f32::from(bounds.size.width).max(1.);
                        let visible = window.content_mask().bounds.intersect(&bounds);
                        if visible.size.width <= px(0.) || visible.size.height <= px(0.) {
                            return;
                        }
                        let from =
                            (f32::from(visible.origin.x - bounds.origin.x) / width).clamp(0., 1.);
                        let to =
                            (f32::from(visible.right() - bounds.origin.x) / width).clamp(from, 1.);
                        let visible_width = f32::from(visible.size.width).max(1.);
                        let columns = (visible_width.ceil() as usize).clamp(1, 2048);
                        let key = PeakKey {
                            stream_index: data.stream_index,
                            columns,
                            duration,
                            from_milli: (from * 1000.).round() as u32,
                            to_milli: (to * 1000.).round() as u32,
                            source: Arc::as_ptr(waveforms) as usize,
                        };
                        let (from, to) = (
                            f64::from(key.from_milli) / 1000.,
                            f64::from(key.to_milli) / 1000.,
                        );
                        let peaks = peak_cache.borrow_mut().get(key, || {
                            column_peaks_range(
                                &data.buckets,
                                duration,
                                waveforms.duration,
                                from,
                                to,
                                columns,
                            )
                        });
                        let start_x = bounds.origin.x + px(width * from as f32);
                        let span_width = width * (to - from) as f32;
                        for (x, peak) in peaks.iter().copied().enumerate() {
                            let at = duration
                                .mul_f64(from + (to - from) * (x as f64 + 0.5) / columns as f64);
                            if !draw_track.contains(at)
                                || !source_ranges.iter().any(|s| at >= s.start && at < s.end)
                            {
                                continue;
                            }
                            let amplitude = display_level(peak * if gain > 0. { gain } else { 1. });
                            if amplitude == 0. {
                                continue;
                            }
                            let height = (amplitude * (row_height - 12.)).max(1.);
                            let bar = Bounds::new(
                                gpui::point(
                                    start_x + px(x as f32 * span_width / columns as f32),
                                    bounds.origin.y + px((row_height - height) / 2.),
                                ),
                                gpui::size(px((span_width / columns as f32).max(1.)), px(height)),
                            );
                            window.paint_quad(gpui::fill(
                                bar,
                                if gain > 0. {
                                    color
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
        if selected
            && let Some(segment) = track
                .selected_segment
                .and_then(|i| track.segments.get(i))
                .copied()
        {
            for (start, at) in [(true, segment.start), (false, segment.end)] {
                row = row.child(
                    div()
                        .id((
                            if start {
                                "audio-trim-start"
                            } else {
                                "audio-trim-end"
                            },
                            index,
                        ))
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .w(px(8.))
                        .when(start, |el| el.left(relative(fraction(at, duration))))
                        .when(!start, |el| el.right(relative(1. - fraction(at, duration))))
                        .bg(t.colors.accent)
                        .cursor_pointer()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
                                window.focus(&this.focus);
                                if let Some(edits) = &mut this.edits {
                                    edits.begin_audio_gesture();
                                }
                                this.audio_dragging = Some((start, event.position.x, segment));
                                cx.stop_propagation();
                            }),
                        ),
                );
            }
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

#[cfg(test)]
mod display_tests {
    use super::*;
    use clipper_media::audio::WaveformBucket;

    fn buckets(peaks: &[f32]) -> Vec<WaveformBucket> {
        peaks
            .iter()
            .map(|&peak| WaveformBucket { peak, rms: peak })
            .collect()
    }

    #[test]
    fn column_peaks_take_the_loudest_bucket_of_each_slice() {
        let b = buckets(&[0.1, 0.9, 0.2, 0.3, 0.8, 0.1, 0.0, 0.4]);
        let d = Duration::from_secs(8);
        assert_eq!(column_peaks(&b, d, d, 4), vec![0.9, 0.3, 0.8, 0.4]);
        assert_eq!(column_peaks(&b, d, d, 1), vec![0.9]);
        // Timeline longer than the analysed audio: tail columns are silent.
        assert_eq!(
            column_peaks(&b, Duration::from_secs(16), d, 4),
            vec![0.9, 0.8, 0.0, 0.0]
        );
    }

    #[test]
    fn visible_range_peaks_match_the_same_slice_of_the_full_waveform() {
        let b = buckets(&[0.1, 0.9, 0.2, 0.3, 0.8, 0.1, 0.0, 0.4]);
        let d = Duration::from_secs(8);
        // Second half of the axis at two columns = last two of four.
        assert_eq!(column_peaks_range(&b, d, d, 0.5, 1.0, 2), vec![0.8, 0.4]);
        assert_eq!(column_peaks_range(&b, d, d, 0.25, 0.5, 1), vec![0.3]);
        assert!(column_peaks_range(&b, d, d, 0.5, 1.0, 0).is_empty());
    }

    #[test]
    fn peak_cache_reuses_layouts_and_stays_bounded() {
        let mut cache = PeakCache::default();
        let key = |columns| PeakKey {
            stream_index: 1,
            columns,
            from_milli: 0,
            to_milli: 1000,
            duration: Duration::from_secs(10),
            source: 42,
        };
        let mut computed = 0;
        for _ in 0..3 {
            cache.get(key(100), || {
                computed += 1;
                vec![0.; 100]
            });
        }
        assert_eq!(computed, 1, "same layout is computed once");
        for columns in 0..100 {
            cache.get(key(columns), || vec![0.; columns]);
        }
        assert_eq!(cache.len(), PEAK_CACHE_ENTRIES);
    }

    #[test]
    fn waveform_levels_follow_real_peaks() {
        assert_eq!(display_level(0.), 0.);
        assert_eq!(display_level(f32::NAN), 0.);
        assert_eq!(display_level(1.), 1.);
        assert_eq!(display_level(2.), 1.);
        // Below the -48 dB floor nothing is drawn.
        assert_eq!(display_level(0.003), 0.);
        let quiet = display_level(0.025);
        let loud = display_level(0.155);
        assert!(quiet > 0. && quiet < loud && loud < 1.);
        assert!((display_level(0.5) - (1. - 6.0206 / 48.)).abs() < 1e-3);
    }

    #[test]
    fn track_colors_start_at_accent_and_differ() {
        let accent = gpui::hsla(0.75, 0.9, 0.65, 1.);
        assert_eq!(track_color(accent, 0), accent);
        let colors: Vec<_> = (0..4).map(|i| track_color(accent, i)).collect();
        for (i, a) in colors.iter().enumerate() {
            assert!((0. ..1.).contains(&a.h));
            assert_eq!((a.s, a.l, a.a), (accent.s, accent.l, accent.a));
            for b in &colors[i + 1..] {
                assert!((a.h - b.h).abs() > 0.1);
            }
        }
    }
}
