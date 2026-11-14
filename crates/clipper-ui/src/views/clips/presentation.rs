use super::*;

#[allow(clippy::too_many_arguments)] // Matches the shared button API plus focus and activation.
fn control(
    t: &Theme,
    index: usize,
    label: impl Into<SharedString>,
    enabled: bool,
    variant: ButtonVariant,
    focus: &FocusHandle,
    cx: &mut Context<ClipLibrary>,
    activate: impl Fn(&mut ClipLibrary, &mut Window, &mut Context<ClipLibrary>) + 'static,
) -> Stateful<Div> {
    let activate = std::rc::Rc::new(activate);
    let clicked = activate.clone();
    button(t, ("library-control", index), label, variant, None, enabled)
        .when(enabled, |d| {
            d.track_focus(focus).key_context("LibraryButton")
        })
        .focus(|s| s.border_color(t.colors.focus_ring))
        .on_click(cx.listener(move |this, _, window, cx| {
            cx.stop_propagation();
            this.context_position = None;
            if enabled {
                clicked(this, window, cx);
            }
        }))
        .on_action(
            cx.listener(move |this, _: &crate::components::Activate, window, cx| {
                cx.stop_propagation();
                this.context_position = None;
                if enabled {
                    activate(this, window, cx);
                }
            }),
        )
}

impl Render for ClipLibrary {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = *cx.theme();
        if let Some((kind, _)) = self.menu {
            let slot = if kind == Menu::Source { 0 } else { 2 };
            if !self.controls[slot].is_focused(window) {
                self.menu = None;
            }
        }
        let available: f32 = window.viewport_size().width.into();
        self.columns = (((available - 300.0) / 215.0).floor() as usize).clamp(1, 4);
        let list = self.settings.view == LibraryView::List;
        let selected = self.selection.ids.len();
        let enabled = !self.busy;
        let all_favorite = selected > 0
            && self
                .selection
                .ids
                .iter()
                .all(|id| self.settings.favorites.contains(id));
        let toolbar = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_2()
            .child(div().min_w(px(180.)).flex_1().child(self.search.clone()))
            .child(self.menu_control(&t, Menu::Source, window, cx))
            .child(control(
                &t,
                1,
                if self.favorites_only {
                    "★ Favorites"
                } else {
                    "All clips"
                },
                enabled,
                ButtonVariant::Secondary,
                &self.controls[1],
                cx,
                |this, window, cx| {
                    this.favorites_only = !this.favorites_only;
                    this.page = 0;
                    this.refilter(window, cx);
                },
            ))
            .child(self.menu_control(&t, Menu::Sort, window, cx))
            .child(control(
                &t,
                3,
                if list { "List ▾" } else { "Grid ▾" },
                enabled,
                ButtonVariant::Secondary,
                &self.controls[3],
                cx,
                |this, _, cx| {
                    this.settings.view = if this.settings.view == LibraryView::Grid {
                        LibraryView::List
                    } else {
                        LibraryView::Grid
                    };
                    this.changed_settings(cx);
                    cx.notify();
                },
            ))
            .child(control(
                &t,
                4,
                "Refresh",
                enabled,
                ButtonVariant::Ghost,
                &self.controls[4],
                cx,
                |this, _, cx| this.refresh(cx),
            ));
        let actions = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_2()
            .child(
                div()
                    .text_xs()
                    .text_color(t.colors.text_secondary)
                    .child(format!("Selected: {selected}")),
            )
            .child(control(
                &t,
                5,
                "Play",
                enabled && selected == 1,
                ButtonVariant::Secondary,
                &self.controls[5],
                cx,
                |this, _, cx| this.open(false, cx),
            ))
            .child(control(
                &t,
                6,
                "Edit",
                enabled && selected == 1,
                ButtonVariant::Primary,
                &self.controls[6],
                cx,
                |this, _, cx| this.open(true, cx),
            ))
            .child(control(
                &t,
                7,
                if all_favorite {
                    "Unfavorite"
                } else {
                    "Favorite"
                },
                enabled && selected > 0,
                ButtonVariant::Secondary,
                &self.controls[7],
                cx,
                |this, window, cx| this.favorite(window, cx),
            ))
            .child(control(
                &t,
                8,
                "Open Folder",
                enabled && selected == 1,
                ButtonVariant::Ghost,
                &self.controls[8],
                cx,
                |this, _, cx| this.folder(cx),
            ))
            .child(control(
                &t,
                9,
                "Rename",
                enabled && selected == 1,
                ButtonVariant::Ghost,
                &self.controls[9],
                cx,
                |this, window, cx| this.begin_rename(window, cx),
            ))
            .child(control(
                &t,
                10,
                "Delete",
                enabled && selected > 0,
                ButtonVariant::Danger,
                &self.controls[10],
                cx,
                |this, _, cx| this.confirm_delete(cx),
            ))
            .child(control(
                &t,
                11,
                "Select all",
                enabled,
                ButtonVariant::Ghost,
                &self.controls[11],
                cx,
                |this, _, cx| {
                    this.selection.all(&this.ordered());
                    cx.notify();
                },
            ))
            .child(control(
                &t,
                12,
                "Clear",
                enabled && selected > 0,
                ButtonVariant::Ghost,
                &self.controls[12],
                cx,
                |this, _, cx| {
                    this.selection.clear();
                    this.delete_targets.clear();
                    this.rename_target = None;
                    cx.notify();
                },
            ));
        let mut content = div()
            .on_action(cx.listener(|this, _: &Clear, _, cx| {
                this.delete_targets.clear();
                this.rename_target = None;
                this.context_position = None;
                this.selection.clear();
                cx.notify();
            }))
            .h_full()
            .min_h_0()
            .on_action(cx.listener(|this, _: &super::super::SelectNext, _, cx| {
                this.step_menu(1, cx);
            }))
            .on_action(
                cx.listener(|this, _: &super::super::SelectPrevious, _, cx| {
                    this.step_menu(-1, cx);
                }),
            )
            .on_action(cx.listener(|this, _: &super::super::Confirm, window, cx| {
                if let Some((menu, index)) = this.menu {
                    this.choose_menu(menu, index, window, cx);
                } else {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this, _: &super::super::Dismiss, _, cx| {
                this.menu = None;
                cx.notify();
            }))
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .text_2xl()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child("Your Clips"),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(t.colors.text_muted)
                    .child("Browse and manage your gaming moments"),
            )
            .child(toolbar);
        if let Some(message) = &self.notice {
            content = content.child(
                panel(&t)
                    .id("library-notice")
                    .max_h(px(88.))
                    .overflow_y_scroll()
                    .p_3()
                    .text_sm()
                    .child(message.clone()),
            );
        }
        if !self.delete_targets.is_empty() {
            content = content.child(panel(&t).p_3().flex().flex_col().gap_2()
                .child(format!("Move {} selected clip(s) to Recycle Bin?",self.delete_targets.len()))
                .child(div().text_xs().text_color(t.colors.text_muted).child("Original files will leave the clips folder. No permanent-delete fallback is used."))
                .child(div().flex().gap_2()
                    .child(control(&t,13,"Confirm deletion",enabled,ButtonVariant::Danger,&self.controls[13],cx,|this,_,cx| this.delete(cx)))
                    .child(control(&t,14,"Cancel",enabled,ButtonVariant::Secondary,&self.controls[14],cx,|this,_,cx| { this.delete_targets.clear(); cx.notify(); }))));
        }
        if self.rename_target.is_some() {
            content = content.child(
                panel(&t)
                    .p_3()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .child(div().w(px(260.)).child(self.rename_input.clone()))
                    .child(control(
                        &t,
                        15,
                        "Save name",
                        enabled,
                        ButtonVariant::Primary,
                        &self.controls[15],
                        cx,
                        |this, window, cx| {
                            let name = this.rename_input.read(cx).text().to_owned();
                            this.rename(name, window, cx);
                        },
                    ))
                    .child(control(
                        &t,
                        16,
                        "Cancel rename",
                        enabled,
                        ButtonVariant::Secondary,
                        &self.controls[16],
                        cx,
                        |this, _, cx| {
                            this.rename_target = None;
                            cx.notify();
                        },
                    )),
            );
        }
        let ids = self
            .filtered
            .iter()
            .skip(self.page * PAGE_SIZE)
            .take(if self.visible { PAGE_SIZE } else { 0 })
            .copied()
            .collect::<Vec<_>>();
        let mut grid = div()
            .id("library-clips")
            .track_scroll(&self.scroll)
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .flex()
            .flex_col()
            .gap_3()
            .on_action(cx.listener(|this, _: &Play, _, cx| this.open(false, cx)))
            .on_action(cx.listener(|this, _: &Edit, _, cx| this.open(true, cx)))
            .on_action(cx.listener(|this, _: &Rename, window, cx| this.begin_rename(window, cx)))
            .on_action(cx.listener(|this, _: &Delete, _, cx| this.confirm_delete(cx)))
            .on_action(cx.listener(|this, _: &SelectAll, _, cx| {
                this.selection.all(&this.ordered());
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &Clear, _, cx| {
                this.selection.clear();
                this.context_position = None;
                this.delete_targets.clear();
                this.rename_target = None;
                cx.notify();
            }))
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                let delta = match event.keystroke.key.as_str() {
                    "left" => -1,
                    "right" => 1,
                    "up" => {
                        -((if this.settings.view == LibraryView::List {
                            1
                        } else {
                            this.columns
                        }) as isize)
                    }
                    "down" => {
                        (if this.settings.view == LibraryView::List {
                            1
                        } else {
                            this.columns
                        }) as isize
                    }
                    _ => return,
                };
                cx.stop_propagation();
                window.prevent_default();
                this.move_selection(delta, event.keystroke.modifiers.shift, window, cx);
            }));
        if self.loading || self.duration_pending {
            grid = grid.child(empty_state(
                &t,
                "▤",
                "Loading clips…",
                "Reading the configured recordings folder.",
            ));
        } else if ids.is_empty() {
            grid = grid.child(empty_state(
                &t,
                "▤",
                "No clips found",
                if self.clips.is_empty() {
                    "Save a replay to start your library."
                } else {
                    "Try another search or filter."
                },
            ));
        } else if list {
            for (slot, index) in ids.iter().enumerate() {
                grid = grid.child(self.clip(&t, *index, slot, true, window, cx));
            }
        } else {
            for chunk in ids.chunks(self.columns) {
                let mut row = div().flex().gap_3();
                for (slot, index) in chunk.iter().enumerate() {
                    row = row.child(self.clip(&t, *index, slot, false, window, cx));
                }
                for _ in chunk.len()..self.columns {
                    row = row.child(div().flex_1().min_w_0());
                }
                grid = grid.child(row);
            }
        }
        content = content.child(grid).child(actions);
        if let Some(position) = self.context_position {
            let popup = panel(&t)
                .id("library-context")
                .p_2()
                .w(px(180.))
                .flex()
                .flex_col()
                .gap_1()
                .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                    this.context_position = None;
                    cx.notify();
                }))
                .child(control(
                    &t,
                    30,
                    "Play",
                    enabled,
                    ButtonVariant::Ghost,
                    &self.controls[30],
                    cx,
                    |this, _, cx| this.open(false, cx),
                ))
                .child(control(
                    &t,
                    31,
                    "Edit",
                    enabled,
                    ButtonVariant::Ghost,
                    &self.controls[31],
                    cx,
                    |this, _, cx| this.open(true, cx),
                ))
                .child(control(
                    &t,
                    32,
                    if all_favorite {
                        "Unfavorite"
                    } else {
                        "Favorite"
                    },
                    enabled,
                    ButtonVariant::Ghost,
                    &self.controls[32],
                    cx,
                    |this, window, cx| this.favorite(window, cx),
                ))
                .child(control(
                    &t,
                    33,
                    "Rename",
                    enabled,
                    ButtonVariant::Ghost,
                    &self.controls[33],
                    cx,
                    |this, window, cx| this.begin_rename(window, cx),
                ))
                .child(control(
                    &t,
                    34,
                    "Delete",
                    enabled,
                    ButtonVariant::Danger,
                    &self.controls[34],
                    cx,
                    |this, _, cx| this.confirm_delete(cx),
                ));
            content = content.child(gpui::deferred(
                gpui::anchored().position(position).child(popup),
            ));
        }
        let pages = self.filtered.len().div_ceil(PAGE_SIZE).max(1);
        content.child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_2()
                .child(control(
                    &t,
                    17,
                    "Previous",
                    self.page > 0 && enabled,
                    ButtonVariant::Secondary,
                    &self.controls[17],
                    cx,
                    |this, window, cx| {
                        this.page = this.page.saturating_sub(1);
                        this.scroll.scroll_to_top_of_item(0);
                        this.previews(window, cx);
                        cx.notify();
                    },
                ))
                .child(
                    div()
                        .text_xs()
                        .text_color(t.colors.text_muted)
                        .child(format!(
                            "Page {} / {pages} · {} clips",
                            self.page + 1,
                            self.filtered.len()
                        )),
                )
                .child(control(
                    &t,
                    18,
                    "Next",
                    (self.page + 1) < pages && enabled,
                    ButtonVariant::Secondary,
                    &self.controls[18],
                    cx,
                    |this, window, cx| {
                        this.page += 1;
                        this.scroll.scroll_to_top_of_item(0);
                        this.previews(window, cx);
                        cx.notify();
                    },
                ))
                .child(div().text_xs().text_color(t.colors.text_muted).child(
                    "Click to select · Ctrl/Shift for multiple · Enter to play · F2 to rename",
                )),
        )
    }
}

impl ClipLibrary {
    fn clip(
        &self,
        t: &Theme,
        index: usize,
        _slot: usize,
        list: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let clip = &self.clips[index];
        let id = clip.id.clone();
        let click = id.clone();
        let context_id = id.clone();
        let fav_id = id.clone();
        let selected = self.selection.ids.contains(&id);
        let favorite = self.settings.favorites.contains(&id);
        let available: f32 = window.viewport_size().width.into();
        let poster_height = (((available - 300. - 12. * (self.columns.saturating_sub(1) as f32))
            / (self.columns as f32)
            - 18.)
            * 9.
            / 16.)
            .max(80.);
        let thumb = div()
            .relative()
            .overflow_hidden()
            .rounded_lg()
            .bg(t.colors.surface_raised)
            .when(list, |d| d.w(px(96.)).h(px(54.)).flex_none())
            .when(!list, |d| d.w_full().h(px(poster_height)))
            .map(|d| match self.images.get(&id) {
                Some(image) => d.child(img(image.clone()).size_full().object_fit(ObjectFit::Cover)),
                None => d
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(t.colors.text_muted)
                    .child("Video preview unavailable"),
            });
        let duration = clip
            .duration
            .map(|d| {
                let s = d.as_secs();
                format!("{}:{:02}", s / 60, s % 60)
            })
            .unwrap_or_else(|| "Duration unavailable".into());
        let date = relative_time(SystemTime::now(), clip.created.unwrap_or(clip.modified));
        let details = div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .child(clip.name.clone()),
                    )
                    .child(
                        button(
                            t,
                            SharedString::from(format!("favorite-{id}")),
                            if favorite { "★" } else { "☆" },
                            ButtonVariant::Ghost,
                            None,
                            true,
                        )
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                cx.stop_propagation();
                                if this.settings.favorites.contains(&fav_id) {
                                    this.settings.favorites.remove(&fav_id);
                                } else {
                                    this.settings.favorites.insert(fav_id.clone());
                                }
                                this.changed_settings(cx);
                                this.refilter(window, cx);
                            },
                        )),
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(t.colors.text_muted)
                    .truncate()
                    .child(
                        clip.source
                            .clone()
                            .unwrap_or_else(|| "Source unavailable".into()),
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(t.colors.text_secondary)
                    .truncate()
                    .child(format!("{duration} · {}", size_label(clip.size_bytes))),
            )
            .child(div().text_xs().text_color(t.colors.text_muted).child(date));
        panel(t)
            .id(SharedString::from(format!("library-clip-{id}")))
            .p_2()
            .flex()
            .gap_2()
            .min_w_0()
            .when(list, |d| d.flex_row().items_center())
            .when(!list, |d| d.flex_col().flex_1())
            .border_color(if selected {
                t.colors.accent
            } else {
                t.colors.border
            })
            .when(selected && self.focus.is_focused(window), |d| d.border_2())
            .hover(|s| s.bg(t.colors.surface_hover))
            .cursor_pointer()
            .on_mouse_down(
                gpui::MouseButton::Right,
                cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
                    this.select(&context_id, false, false, window, cx);
                    this.context_position = Some(event.position);
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            .on_click(
                cx.listener(move |this, event: &gpui::ClickEvent, window, cx| {
                    this.select(
                        &click,
                        event.modifiers().control,
                        event.modifiers().shift,
                        window,
                        cx,
                    );
                    if event.click_count() == 2 {
                        this.open(false, cx);
                    }
                }),
            )
            .child(thumb)
            .child(details)
    }
}
