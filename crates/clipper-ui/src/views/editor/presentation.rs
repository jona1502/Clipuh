//! Editor workspace presentation and timeline interactions.
use super::*;

impl Render for ClipEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = *cx.theme();
        let loaded = self.ready && self.edits.is_some();
        let exporting = matches!(self.export, Export::Running { .. });
        let duration = self.duration.unwrap_or_default();
        let has_segments = self
            .edits
            .as_ref()
            .is_some_and(|e| !e.segments().is_empty());
        let header = panel(&t)
            .px_3()
            .py_2()
            .flex()
            .items_center()
            .gap_3()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_lg()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(file_name(&self.path)),
            )
            .child(muted(&t).child("Non-destructive edits"))
            .child(
                button(
                    &t,
                    "editor-export",
                    "Export",
                    ButtonVariant::Primary,
                    None,
                    loaded && has_segments && !exporting,
                )
                .when(loaded && has_segments && !exporting, |el| {
                    el.on_click(cx.listener(|this, _, _, cx| this.choose_export(cx)))
                }),
            );
        let preview_height = (f32::from(window.viewport_size().height) * 0.43).clamp(180., 440.);
        let preview = div()
            .id("editor-preview")
            .relative()
            .w_full()
            .h(px(preview_height))
            .rounded_lg()
            .overflow_hidden()
            .bg(t.colors.media_background)
            .flex()
            .items_center()
            .justify_center()
            .when(loaded, |el| {
                el.cursor_pointer()
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_playback(cx)))
            })
            .map(|el| match (&self.error, &self.frame) {
                (Some(error), _) => el.child(
                    div()
                        .max_w(px(420.))
                        .text_sm()
                        .text_center()
                        .text_color(t.colors.danger)
                        .child(error.clone()),
                ),
                (None, Some(frame)) => el.child(
                    img(frame.clone())
                        .size_full()
                        .object_fit(ObjectFit::Contain),
                ),
                _ => el.child(muted(&t).child("Loading clip…")),
            });
        let controls = div()
            .flex()
            .flex_wrap()
            .items_center()
            .justify_between()
            .gap_2()
            .child(muted(&t).child(format!(
                "{} / {}",
                format_timestamp(self.position),
                format_timestamp(duration)
            )))
            .child(
                button(
                    &t,
                    "editor-back",
                    "−10s",
                    ButtonVariant::Ghost,
                    None,
                    loaded,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.seek(this.position.saturating_sub(Duration::from_secs(10)), cx)
                })),
            )
            .child(
                button(
                    &t,
                    "editor-play",
                    if self.playing { "Pause" } else { "Play" },
                    ButtonVariant::Primary,
                    Some(&["Space"]),
                    loaded && has_segments,
                )
                .when(loaded && has_segments, |el| {
                    el.on_click(cx.listener(|this, _, _, cx| this.toggle_playback(cx)))
                }),
            )
            .child(
                button(
                    &t,
                    "editor-forward",
                    "+10s",
                    ButtonVariant::Ghost,
                    None,
                    loaded,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.seek(this.position + Duration::from_secs(10), cx)
                })),
            );
        let controls = controls.child(
            button(
                &t,
                "editor-fullscreen",
                if self.preview_fullscreen {
                    "Exit fullscreen"
                } else {
                    "Fullscreen"
                },
                ButtonVariant::Ghost,
                None,
                loaded,
            )
            .when(loaded, |el| {
                el.on_click(cx.listener(|this, _, window, cx| this.toggle_preview(window, cx)))
            }),
        );
        if self.preview_fullscreen {
            return div()
                .id("clip-editor-fullscreen")
                .key_context(CONTEXT)
                .track_focus(&self.focus)
                .on_action(cx.listener(|this, _: &TogglePlayback, _, cx| this.toggle_playback(cx)))
                .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                    if event.keystroke.key == "escape" {
                        this.toggle_preview(window, cx);
                        cx.stop_propagation();
                    }
                }))
                .size_full()
                .bg(t.colors.media_background)
                .flex()
                .flex_col()
                .child(preview.h(px(0.)).flex_1().min_h_0())
                .child(div().p_3().child(controls));
        }
        let tabs = div()
            .flex()
            .w_full()
            .gap_1()
            .p_1()
            .rounded_lg()
            .bg(t.colors.surface_raised)
            .children(
                ["Trim", "Effects", "Audio", "Text", "Export"]
                    .into_iter()
                    .enumerate()
                    .map(|(i, label)| {
                        button(
                            &t,
                            ("editor-tab", i),
                            label,
                            if self.tab == i {
                                ButtonVariant::Primary
                            } else {
                                ButtonVariant::Ghost
                            },
                            None,
                            true,
                        )
                        .flex_1()
                        .min_w_0()
                        .px_1()
                        .justify_center()
                        .text_size(px(11.))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.tab = i;
                            cx.notify();
                        }))
                    }),
            );
        let mut inspector = panel(&t)
            .w(relative(0.30))
            .min_w(px(290.))
            .p_3()
            .flex()
            .flex_col()
            .gap_3()
            .child(tabs);
        inspector = match self.tab {
            0 => {
                let start = timestamp_row(&t, "Start", self.start_input.clone());
                let end = timestamp_row(&t, "End", self.end_input.clone());
                let length = self.trim.map_or_else(
                    || "No segment selected".into(),
                    |range| format_timestamp(range.end - range.start),
                );
                let split = button(
                    &t,
                    "editor-split",
                    "Split",
                    ButtonVariant::Secondary,
                    None,
                    loaded && has_segments,
                )
                .when(loaded && has_segments, |el| {
                    el.on_click(cx.listener(|this, _, _, cx| {
                        this.notice = this
                            .edits
                            .as_mut()
                            .and_then(|edits| edits.split(this.position).err())
                            .map(Into::into);
                        this.sync_trim(cx);
                    }))
                });
                let delete = button(
                    &t,
                    "editor-delete",
                    "Delete",
                    ButtonVariant::Secondary,
                    None,
                    loaded && self.trim.is_some(),
                )
                .when(loaded && self.trim.is_some(), |el| {
                    el.on_click(cx.listener(|this, _, _, cx| {
                        if let Some(edits) = &mut this.edits {
                            edits.delete_selected();
                        }
                        this.sync_trim(cx);
                    }))
                });
                let reset_enabled =
                    loaded && self.trim.is_some_and(|range| !range.is_full(duration));
                let reset = button(
                    &t,
                    "editor-reset",
                    "Reset selected trim",
                    ButtonVariant::Ghost,
                    None,
                    reset_enabled,
                )
                .when(reset_enabled, |el| {
                    el.on_click(cx.listener(|this, _, _, cx| this.reset_trim(cx)))
                });
                inspector
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .child("Clip"),
                    )
                    .child(start)
                    .child(end)
                    .child(muted(&t).child(format!("Length   {length}")))
                    .child(muted(&t).child("Enter timestamps to apply · I / O: set at playhead"))
                    .child(div().flex().gap_2().child(split).child(delete))
                    .child(reset)
            }
            4 => {
                let enabled = loaded && has_segments && !exporting;
                let save = button(
                    &t,
                    "editor-save-as",
                    "Choose output…",
                    ButtonVariant::Primary,
                    None,
                    enabled,
                )
                .when(enabled, |el| {
                    el.on_click(cx.listener(|this, _, _, cx| this.choose_export(cx)))
                });
                let length = format_timestamp(
                    self.edits
                        .as_ref()
                        .map_or(Duration::ZERO, |edits| edits.duration()),
                );
                inspector.child(div().text_sm().child("MP4 · H.264 video · Stereo AAC mix"))
                    .child(muted(&t).child("Selected video ranges with the enabled audio mix. Mute, Solo, gain and source audio cuts are included. Original unchanged."))
                    .child(muted(&t).child("AAC container duration may include encoder padding; cuts never snap to earlier keyframes."))
                    .child(muted(&t).child(format!("Output length: {length}"))).child(save)
            }
            2 => inspector.child(self.audio_inspector(window, cx)),
            _ => inspector
                .child(muted(&t).child("Coming later. This feature is not available in Phase 6.")),
        };
        let can_undo = self.edits.as_ref().is_some_and(|e| e.can_undo());
        let can_redo = self.edits.as_ref().is_some_and(|e| e.can_redo());
        let toolbar = div()
            .flex()
            .items_center()
            .gap_2()
            .child(
                button(
                    &t,
                    "editor-undo",
                    "Undo",
                    ButtonVariant::Ghost,
                    Some(&["Ctrl", "Z"]),
                    can_undo,
                )
                .when(can_undo, |el| {
                    el.on_click(cx.listener(|this, _, _, cx| {
                        if let Some(e) = &mut this.edits {
                            e.undo();
                        }
                        this.sync_trim(cx);
                    }))
                }),
            )
            .child(
                button(
                    &t,
                    "editor-redo",
                    "Redo",
                    ButtonVariant::Ghost,
                    Some(&["Ctrl", "Y"]),
                    can_redo,
                )
                .when(can_redo, |el| {
                    el.on_click(cx.listener(|this, _, _, cx| {
                        if let Some(e) = &mut this.edits {
                            e.redo();
                        }
                        this.sync_trim(cx);
                    }))
                }),
            )
            .child(
                muted(&t)
                    .flex_1()
                    .child("Source timeline · select segment · drag edges"),
            )
            .child(self.zoom_controls(&t, loaded, window, cx));
        let timeline = self.timeline(duration, loaded, window, cx);
        div()
            .id("clip-editor")
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &TogglePlayback, _, cx| this.toggle_playback(cx)))
            .on_action(cx.listener(|this, _: &SetIn, _, cx| this.set_in(cx)))
            .on_action(cx.listener(|this, _: &SetOut, _, cx| this.set_out(cx)))
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                if event.keystroke.modifiers.control {
                    let changed = match event.keystroke.key.as_str() {
                        "z" => this.edits.as_mut().is_some_and(|e| e.undo()),
                        "y" => this.edits.as_mut().is_some_and(|e| e.redo()),
                        _ => false,
                    };
                    if changed {
                        this.sync_trim(cx);
                        cx.stop_propagation();
                    }
                }
            }))
            .flex()
            .flex_col()
            .gap_3()
            .min_w_0()
            .child(header)
            .child(
                div()
                    .flex()
                    .gap_3()
                    .min_w_0()
                    .child(
                        panel(&t)
                            .flex_1()
                            .min_w_0()
                            .p_2()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(preview)
                            .child(controls),
                    )
                    .child(inspector),
            )
            .child(
                panel(&t)
                    .p_3()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(toolbar)
                    .child(timeline),
            )
            .children(
                self.notice
                    .clone()
                    .map(|notice| muted(&t).text_color(t.colors.warning).child(notice)),
            )
            .children(self.export_status(cx))
    }
}

impl ClipEditor {
    fn toggle_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.preview_fullscreen {
            self.fullscreen_before_preview = window.is_fullscreen();
            if !self.fullscreen_before_preview {
                window.toggle_fullscreen();
            }
        } else if !self.fullscreen_before_preview && window.is_fullscreen() {
            window.toggle_fullscreen();
        }
        self.preview_fullscreen = !self.preview_fullscreen;
        window.focus(&self.focus);
        cx.emit(EditorEvent::PreviewChanged);
        cx.notify();
    }

    fn timeline(
        &self,
        duration: Duration,
        loaded: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let t = *cx.theme();
        let bounds_out = self.timeline.clone();
        let mut ruler = div().relative().h(px(26.)).w_full();
        // Before the first layout the viewport is unknown: label the whole
        // axis at a nominal width instead of leaving the ruler empty.
        let measured = f32::from(self.timeline_viewport.get().size.width);
        let (axis, scroll, viewport) = if measured > super::TIMELINE_END_PAD {
            (
                self.axis_width(self.zoom),
                -f32::from(self.timeline_scroll.offset().x),
                measured,
            )
        } else {
            (1000. * self.zoom, 0., 1000. * self.zoom)
        };
        let ticks = clipper_editor::timeline::visible_ticks(
            duration,
            axis,
            scroll,
            scroll + viewport,
            RULER_LABEL_GAP,
        );
        let step = ticks
            .get(1)
            .zip(ticks.first())
            .map_or(Duration::from_secs(1), |(b, a)| *b - *a);
        for at in ticks {
            ruler = ruler.child(
                div()
                    .absolute()
                    .left(relative(fraction(at, duration)))
                    .text_size(px(10.))
                    .text_color(t.colors.text_muted)
                    .child(ruler_label(at, step)),
            );
        }
        let video_height = self.track_height(TrackId::Video, HeightLimits::VIDEO);
        let mut video = div()
            .id("editor-video-track")
            .relative()
            .w_full()
            .h(px(video_height))
            .bg(t.colors.media_background)
            .rounded_md()
            .overflow_hidden()
            .child(
                canvas(move |bounds, _, _| bounds_out.set(bounds), |_, _, _, _| {})
                    .absolute()
                    .size_full(),
            );
        if !self.thumbnails.is_empty() {
            video = video.child(div().absolute().size_full().flex().children(
                self.thumbnails.iter().map(|image| {
                    div()
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .overflow_hidden()
                        .child(img(image.clone()).size_full().object_fit(ObjectFit::Cover))
                }),
            ));
        }
        if let Some(edits) = &self.edits {
            for (i, segment) in edits.segments().iter().enumerate() {
                let selected = edits.selected() == Some(i);
                let segment = if selected {
                    self.trim.map_or(*segment, |trim| clipper_editor::Segment {
                        start: trim.start,
                        end: trim.end,
                    })
                } else {
                    *segment
                };
                video = video.child(
                    div()
                        .id(("editor-segment", i))
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .left(relative(fraction(segment.start, duration)))
                        .w(relative(fraction(segment.end - segment.start, duration)))
                        .border_2()
                        .border_color(if selected {
                            t.colors.accent
                        } else {
                            t.colors.border_strong
                        })
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
                                window.focus(&this.focus);
                                if let Some(e) = &mut this.edits {
                                    e.select(i);
                                }
                                this.sync_trim(cx);
                                this.scrubbing = true;
                                this.seek_to_x(event.position.x, cx);
                                cx.stop_propagation();
                            }),
                        ),
                );
            }
        }
        if let Some(trim) = self.trim {
            for (start, at) in [(true, trim.start), (false, trim.end)] {
                video = video.child(
                    div()
                        .id(if start {
                            "editor-trim-start"
                        } else {
                            "editor-trim-end"
                        })
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .when(start, |el| el.left(relative(fraction(at, duration))))
                        .when(!start, |el| el.right(relative(1. - fraction(at, duration))))
                        .w(px(9.))
                        .bg(t.colors.accent)
                        .cursor_pointer()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, event: &gpui::MouseDownEvent, _, cx| {
                                this.dragging = Some(start);
                                this.drag_origin = this.trim.map(|trim| (event.position.x, trim));
                                cx.stop_propagation();
                                cx.notify();
                            }),
                        ),
                );
            }
        }
        video = video.when(loaded, |el| {
            el.on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &gpui::MouseDownEvent, window, cx| {
                    window.focus(&this.focus);
                    this.scrubbing = true;
                    this.seek_to_x(event.position.x, cx);
                }),
            )
        });
        let mut tracks = div()
            .relative()
            .flex()
            .flex_col()
            .gap_2()
            .child(ruler)
            .child(video);
        // Combined/Master is first (see `arrange_audio_tracks`); every row
        // shares the one time axis above.
        for i in 0..self.audio_tracks {
            tracks = tracks.child(self.audio_waveform(i, duration, cx));
        }
        // Vsync-paced only while playing; idle and paused editors do not
        // redraw. GPUI does not draw minimized windows.
        self.queue_playhead_frame(window, cx);
        tracks = tracks.child(
            div()
                .absolute()
                .top(px(20.))
                .bottom_0()
                .left(relative(fraction(self.playhead_position(), duration)))
                .w(px(2.))
                .bg(t.colors.accent),
        );
        let mut labels = div()
            .w(px(300.))
            .flex_shrink_0()
            .flex()
            .flex_col()
            .gap_2()
            .pt(px(34.))
            .child(
                muted(&t)
                    .relative()
                    .h(px(video_height))
                    .child("Video")
                    .child(self.resize_handle(
                        &t,
                        TrackId::Video,
                        video_height,
                        HeightLimits::VIDEO,
                        cx,
                    )),
            );
        for i in 0..self.audio_tracks {
            labels = labels.child(self.audio_header(i, window, cx));
        }
        if self.audio_tracks == 0 {
            labels = labels.child(muted(&t).child(if self.media_duration.is_some() {
                "No audio tracks"
            } else {
                "Reading tracks…"
            }));
        }
        let viewport_out = self.timeline_viewport.clone();
        let editor = cx.entity().downgrade();
        let scroller = div()
            .relative()
            .flex_1()
            .min_w_0()
            .child(
                div()
                    .id("editor-timeline-scroll")
                    .w_full()
                    .overflow_x_scroll()
                    .track_scroll(&self.timeline_scroll)
                    .pb_2()
                    .child(
                        div()
                            .w(relative(self.zoom))
                            .flex_shrink_0()
                            .pr(px(super::TIMELINE_END_PAD))
                            .child(tracks),
                    ),
            )
            // Measures the viewport and zooms on Ctrl+wheel before the
            // scroll area sees the wheel (capture phase), around the pointer.
            .child(
                canvas(
                    move |bounds, _, _| viewport_out.set(bounds),
                    move |bounds, _, window, _| {
                        let editor = editor.clone();
                        window.on_mouse_event(
                            move |event: &gpui::ScrollWheelEvent, phase, window, cx| {
                                if phase != gpui::DispatchPhase::Capture
                                    || !event.modifiers.control
                                    || !bounds.contains(&event.position)
                                {
                                    return;
                                }
                                let notches =
                                    f32::from(event.delta.pixel_delta(window.line_height()).y)
                                        / f32::from(window.line_height())
                                        / 3.;
                                if notches == 0. || !notches.is_finite() {
                                    return;
                                }
                                cx.stop_propagation();
                                let factor = clipper_editor::timeline::ZOOM_STEP
                                    .powf(notches.clamp(-3., 3.));
                                let anchor = f32::from(event.position.x - bounds.origin.x);
                                editor
                                    .update(cx, |this, cx| this.zoom_by(factor, Some(anchor), cx))
                                    .ok();
                            },
                        );
                    },
                )
                .absolute()
                .size_full(),
            );
        div()
            .flex()
            .gap_2()
            .min_w_0()
            .child(labels)
            .child(scroller)
            .on_mouse_move(cx.listener(|this, event: &gpui::MouseMoveEvent, _, cx| {
                if event.pressed_button != Some(MouseButton::Left) {
                    this.finish_drag(cx);
                    return;
                }
                if let Some((id, _, _)) = this.resizing {
                    let limits = match id {
                        TrackId::Video => HeightLimits::VIDEO,
                        TrackId::Audio(_) => Self::audio_limits(cx.theme()),
                    };
                    this.drag_resize(event.position.y, limits, cx);
                    return;
                }
                if this.audio_dragging.is_some() {
                    this.drag_audio(event.position.x, cx);
                    return;
                }
                if let Some(start) = this.dragging {
                    if let (Some(duration), Some(mut trim)) = (this.duration, this.trim) {
                        let bounds = this.timeline.get();
                        let at = if let Some((pointer, original)) = this.drag_origin {
                            let width = f32::from(bounds.size.width);
                            let origin = if start { original.start } else { original.end };
                            clipper_editor::pixel_to_time(
                                clipper_editor::time_to_pixel(origin, duration, width)
                                    + f32::from(event.position.x - pointer),
                                duration,
                                width,
                            )
                        } else {
                            duration.mul_f32(position_fraction(event.position.x, bounds))
                        };
                        let result = if start {
                            trim.set_in(at)
                        } else {
                            trim.set_out(at, duration)
                        };
                        if result.is_ok() {
                            this.trim = Some(trim);
                            cx.notify();
                        }
                    }
                } else if this.scrubbing {
                    this.seek_to_x(event.position.x, cx);
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.finish_drag(cx)),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.finish_drag(cx)),
            )
    }
    pub(super) fn finish_drag(&mut self, cx: &mut Context<Self>) {
        if self.resizing.take().is_some() {
            cx.notify();
        }
        self.finish_audio_drag(cx);
        if self.dragging.take().is_some() {
            if let (Some(edits), Some(trim)) = (&mut self.edits, self.trim) {
                self.notice = edits.trim(trim.start, trim.end).err().map(Into::into);
            }
            self.sync_trim(cx);
        }
        self.scrubbing = false;
        self.drag_origin = None;
    }
    pub(super) fn sync_trim(&mut self, cx: &mut Context<Self>) {
        if let Some(edits) = &mut self.edits
            && edits.audio_tracks().is_empty()
            && let Some(metadata) = &self.audio_metadata
        {
            let duration = edits.source_duration();
            let mut tracks = metadata.clone();
            for track in &mut tracks {
                for segment in &mut track.segments {
                    segment.end = segment.end.min(duration);
                }
            }
            edits.set_audio_tracks(clipper_editor::arrange_audio_tracks(tracks));
        }
        self.sync_audio(cx);
        self.trim = self
            .edits
            .as_ref()
            .and_then(|e| e.selected_segment())
            .map(|s| Trim {
                start: s.start,
                end: s.end,
            });
        let (a, b) = self.trim.map_or((String::new(), String::new()), |r| {
            (format_timestamp(r.start), format_timestamp(r.end))
        });
        self.start_input
            .update(cx, |input, cx| input.set_text(a, cx));
        self.end_input.update(cx, |input, cx| input.set_text(b, cx));
        cx.notify();
    }
    fn choose_export(&mut self, cx: &mut Context<Self>) {
        if matches!(self.export, Export::Running { .. }) {
            return;
        }
        let suggested = trim::export_path(
            &self.path,
            Duration::ZERO,
            self.duration.unwrap_or_default(),
        );
        let name = file_name(&suggested);
        let picker =
            cx.prompt_for_new_path(self.path.parent().unwrap_or(Path::new(".")), Some(&name));
        cx.spawn(async move |this, cx| match picker.await {
            Ok(Ok(Some(path))) => {
                let _ = this.update(cx, |this, cx| this.export_to(path, cx));
            }
            Ok(Ok(None)) => {}
            _ => {
                let _ = this.update(cx, |this, cx| {
                    this.notice = Some("Could not open the output file picker.".into());
                    cx.notify();
                });
            }
        })
        .detach();
    }
}

/// Minimum spacing of ruler labels, in pixels.
const RULER_LABEL_GAP: f32 = 72.;

/// Ruler text for `at` with labels `step` apart: `M:SS`, with tenths or
/// hundredths when the step is below a second, `H:MM:SS` past an hour.
fn ruler_label(at: Duration, step: Duration) -> String {
    let total = at.as_secs();
    let base = if total >= 3600 {
        format!("{}:{:02}:{:02}", total / 3600, total / 60 % 60, total % 60)
    } else {
        format!("{}:{:02}", total / 60, total % 60)
    };
    let millis = at.subsec_millis();
    if step >= Duration::from_secs(1) {
        base
    } else if step.as_millis().is_multiple_of(100) {
        format!("{base}.{}", millis / 100)
    } else {
        format!("{base}.{:02}", millis / 10)
    }
}

impl ClipEditor {
    /// Drag strip on the bottom edge of a track header: resizes that row only.
    pub(super) fn resize_handle(
        &self,
        t: &Theme,
        id: TrackId,
        height: f32,
        limits: HeightLimits,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<Div> {
        let active = self.resizing.is_some_and(|(row, _, _)| row == id);
        let key = match id {
            TrackId::Video => 0,
            TrackId::Audio(stream) => stream as usize + 1,
        };
        div()
            .id(("track-resize", key))
            .absolute()
            .left_0()
            .right_0()
            .bottom(px(-6.))
            .h(px(8.))
            .cursor_row_resize()
            .flex()
            .items_center()
            .child(
                div()
                    .w_full()
                    .h(px(2.))
                    .rounded_full()
                    .when(active, |line| line.bg(t.colors.accent)),
            )
            .hover(|style| style.bg(t.colors.accent.opacity(0.25)))
            .tooltip(crate::components::tooltip("Drag to resize this track"))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &gpui::MouseDownEvent, _, cx| {
                    this.resizing = Some((id, event.position.y, limits.clamp(height)));
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
    }

    /// Zoom out, slider, zoom in and Fit, plus the current level.
    fn zoom_controls(
        &self,
        t: &Theme,
        loaded: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Div {
        use clipper_editor::timeline::{MIN_ZOOM, ZOOM_STEP, slider_to_zoom, zoom_to_slider};
        let max = self.max_zoom();
        let editor = cx.entity().downgrade();
        let slider = crate::components::slider(
            t,
            "editor-zoom-slider",
            zoom_to_slider(self.zoom, max),
            loaded,
            &self.zoom_focus,
            self.zoom_focus.is_focused(window),
            move |value, _, cx| {
                editor
                    .update(cx, |this, cx| {
                        let max = this.max_zoom();
                        this.set_zoom(slider_to_zoom(value, max), None, cx);
                    })
                    .ok();
            },
        );
        div()
            .flex()
            .items_center()
            .gap_1()
            .child(
                button(
                    t,
                    "editor-zoom-out",
                    "−",
                    ButtonVariant::Ghost,
                    None,
                    loaded && self.zoom > MIN_ZOOM,
                )
                .tooltip(crate::components::tooltip("Zoom out (Ctrl+wheel)"))
                .on_click(cx.listener(|this, _, _, cx| this.zoom_by(1. / ZOOM_STEP, None, cx))),
            )
            .child(div().w(px(120.)).child(slider))
            .child(
                button(
                    t,
                    "editor-zoom-in",
                    "+",
                    ButtonVariant::Ghost,
                    None,
                    loaded && self.zoom < max,
                )
                .tooltip(crate::components::tooltip("Zoom in (Ctrl+wheel)"))
                .on_click(cx.listener(|this, _, _, cx| this.zoom_by(ZOOM_STEP, None, cx))),
            )
            .child(
                muted(t)
                    .w(px(52.))
                    .child(format!("{:.0}%", self.zoom * 100.)),
            )
            .child(
                button(
                    t,
                    "editor-zoom-fit",
                    "Fit",
                    ButtonVariant::Ghost,
                    None,
                    loaded && self.zoom > MIN_ZOOM,
                )
                .tooltip(crate::components::tooltip("Show the whole clip"))
                .on_click(cx.listener(|this, _, _, cx| this.set_zoom(MIN_ZOOM, None, cx))),
            )
    }
}

pub(super) fn timestamp_row(theme: &Theme, label: &'static str, input: Entity<TextInput>) -> Div {
    div()
        .flex()
        .items_center()
        .gap_3()
        .child(muted(theme).w(px(42.)).child(label))
        .child(div().flex_1().child(input))
}

#[cfg(test)]
mod ruler_tests {
    use super::*;

    fn ms(v: u64) -> Duration {
        Duration::from_millis(v)
    }

    #[test]
    fn ruler_labels_match_the_tick_step() {
        assert_eq!(ruler_label(ms(65_000), ms(5_000)), "1:05");
        assert_eq!(ruler_label(ms(5_500), ms(500)), "0:05.5");
        assert_eq!(ruler_label(ms(5_250), ms(250)), "0:05.25");
        assert_eq!(ruler_label(ms(3_725_000), ms(60_000)), "1:02:05");
    }
}
