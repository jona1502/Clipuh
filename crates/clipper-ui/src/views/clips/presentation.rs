use super::*;

/// Gap between cards, in pixels.
const GAP: f32 = 16.0;
/// Narrowest a grid card gets before a column is dropped.
const MIN_CARD_WIDTH: f32 = 270.0;
/// Sidebar plus page padding, subtracted from the viewport for the grid width.
const CHROME_WIDTH: f32 = 304.0;
/// Thumbnail height / card width, as in the approved Library design.
const THUMB_RATIO: f32 = 0.39;

/// Grid columns for a viewport width: as many as fit at `MIN_CARD_WIDTH`, at most four.
fn columns_for(viewport: f32) -> usize {
    (((viewport - CHROME_WIDTH + GAP) / (MIN_CARD_WIDTH + GAP)).floor() as usize).clamp(1, 4)
}

/// Width of one grid card for `columns` at `viewport`.
fn card_width(viewport: f32, columns: usize) -> f32 {
    ((viewport - CHROME_WIDTH - GAP * columns.saturating_sub(1) as f32) / columns as f32)
        .max(MIN_CARD_WIDTH.min(viewport))
}

fn duration_label(duration: std::time::Duration) -> String {
    let s = duration.as_secs();
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{:02}:{:02}", s / 60, s % 60)
    }
}

/// What the card's game line says: the recorded source when known, else the kind.
fn origin_label(clip: &Clip) -> String {
    match &clip.source {
        Some(source) => source.to_uppercase(),
        None if clip.is_replay() => "INSTANT REPLAY".into(),
        None => "RECORDING".into(),
    }
}

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

/// Square checkbox drawn over thumbnails and in the selection bar.
fn check_mark(t: &Theme, checked: bool, size: f32) -> Div {
    div()
        .flex_none()
        .size(px(size))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(5.))
        .border_1()
        .text_xs()
        .font_weight(gpui::FontWeight::BOLD)
        .text_color(t.colors.accent_foreground)
        .map(|d| {
            if checked {
                d.border_color(t.colors.accent)
                    .bg(t.colors.accent)
                    .child("✓")
            } else {
                d.border_color(t.colors.border_strong).bg(t.colors.scrim)
            }
        })
}

impl ClipLibrary {
    /// Selects only `id`, so the bar's actions apply to that one clip.
    fn focus_clip(&mut self, id: &str) {
        self.context_position = None;
        self.delete_targets.clear();
        self.rename_target = None;
        self.selection.select(id, &self.ordered(), false, false);
        self.cursor = Some(id.to_owned());
    }

    fn toggle_favorite(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if !self.settings.favorites.remove(id) {
            self.settings.favorites.insert(id.to_owned());
        }
        self.changed_settings(cx);
        self.refilter(window, cx);
    }

    fn tabs(&self, t: &Theme, cx: &mut Context<Self>) -> Div {
        let enabled = !self.busy;
        div()
            .flex()
            .items_center()
            .gap_1()
            .p_1()
            .rounded_lg()
            .bg(t.colors.surface)
            .children(
                Tab::ALL
                    .into_iter()
                    .enumerate()
                    .map(|(index, (tab, glyph, label))| {
                        let active = self.tab == tab;
                        let item = div()
                            .id(("library-tab", index))
                            .flex()
                            .items_center()
                            .gap_2()
                            .h(t.metrics.control_height)
                            .px_3()
                            .rounded_md()
                            .border_1()
                            .text_sm()
                            .map(|d| {
                                if active {
                                    d.border_color(t.colors.accent)
                                        .bg(t.colors.accent_subtle)
                                        .text_color(t.colors.text)
                                } else {
                                    d.border_color(gpui::transparent_black())
                                        .text_color(t.colors.text_secondary)
                                        .hover(|s| s.bg(t.colors.surface_hover))
                                }
                            })
                            .child(glyph)
                            .child(label);
                        let focus = &self.controls[19 + index];
                        item.when(enabled, |d| {
                            d.cursor_pointer()
                                .track_focus(focus)
                                .key_context("LibraryButton")
                        })
                        .focus(|s| s.border_color(t.colors.focus_ring))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            if enabled && this.tab != tab {
                                this.tab = tab;
                                this.page = 0;
                                this.refilter(window, cx);
                            }
                        }))
                        .on_action(cx.listener(
                            move |this, _: &crate::components::Activate, window, cx| {
                                if enabled && this.tab != tab {
                                    this.tab = tab;
                                    this.page = 0;
                                    this.refilter(window, cx);
                                }
                            },
                        ))
                    }),
            )
    }

    fn view_toggle(&self, t: &Theme, cx: &mut Context<Self>) -> Div {
        let current = self.settings.view;
        let item = |slot: usize, view: LibraryView, glyph: &'static str, hint: &'static str| {
            let active = current == view;
            div()
                .id(("library-view", slot))
                .size(t.metrics.control_height)
                .flex()
                .items_center()
                .justify_center()
                .rounded_md()
                .border_1()
                .border_color(gpui::transparent_black())
                .cursor_pointer()
                .tooltip(crate::components::tooltip(hint))
                .map(|d| {
                    if active {
                        d.bg(t.colors.accent).text_color(t.colors.accent_foreground)
                    } else {
                        d.text_color(t.colors.text_secondary)
                            .hover(|s| s.bg(t.colors.surface_hover))
                    }
                })
                .track_focus(&self.controls[slot])
                .key_context("LibraryButton")
                .focus(|s| s.border_color(t.colors.focus_ring))
                .child(glyph)
        };
        let switch = |view: LibraryView| {
            move |this: &mut ClipLibrary, _: &mut Window, cx: &mut Context<ClipLibrary>| {
                if this.settings.view != view {
                    this.settings.view = view;
                    this.changed_settings(cx);
                    cx.notify();
                }
            }
        };
        let (grid, list) = (switch(LibraryView::Grid), switch(LibraryView::List));
        let (grid_key, list_key) = (switch(LibraryView::Grid), switch(LibraryView::List));
        div()
            .flex()
            .gap_1()
            .p_1()
            .rounded_lg()
            .bg(t.colors.surface)
            .child(
                item(1, LibraryView::Grid, "▦", "Grid view")
                    .on_click(cx.listener(move |this, _, w, cx| grid(this, w, cx)))
                    .on_action(
                        cx.listener(move |this, _: &crate::components::Activate, w, cx| {
                            grid_key(this, w, cx)
                        }),
                    ),
            )
            .child(
                item(3, LibraryView::List, "☰", "List view")
                    .on_click(cx.listener(move |this, _, w, cx| list(this, w, cx)))
                    .on_action(
                        cx.listener(move |this, _: &crate::components::Activate, w, cx| {
                            list_key(this, w, cx)
                        }),
                    ),
            )
    }
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
        let viewport: f32 = window.viewport_size().width.into();
        self.columns = columns_for(viewport);
        let list = self.settings.view == LibraryView::List;
        let selected_clips = self.selected();
        let selected = selected_clips.len();
        let selected_bytes: u64 = selected_clips.iter().map(|c| c.size_bytes).sum();
        let enabled = !self.busy;
        let all_favorite = selected > 0
            && self
                .selection
                .ids
                .iter()
                .all(|id| self.settings.favorites.contains(id));

        let search = div()
            .w(px(260.))
            .flex()
            .items_center()
            .gap_2()
            .child(div().flex_1().min_w_0().child(self.search.clone()))
            .child(crate::components::shortcut(&t, &["Ctrl", "F"]));
        let toolbar = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_3()
            .child(self.tabs(&t, cx))
            .child(div().flex_1())
            .child(search)
            .child(self.menu_control(&t, Menu::Source, window, cx))
            .child(self.menu_control(&t, Menu::Sort, window, cx))
            .child(self.view_toggle(&t, cx))
            // Only needed when Windows cannot watch the folder for changes.
            .when(self.watcher.is_none(), |d| {
                d.child(control(
                    &t,
                    4,
                    "↻ Refresh",
                    enabled,
                    ButtonVariant::Ghost,
                    &self.controls[4],
                    cx,
                    |this, _, cx| this.refresh(cx),
                ))
            });
        let filtered_bytes: u64 = self
            .filtered
            .iter()
            .map(|i| self.clips[*i].size_bytes)
            .sum();
        let count = div()
            .text_sm()
            .text_color(t.colors.text_muted)
            .child(format!(
                "{} {} • {}",
                self.filtered.len(),
                if self.filtered.len() == 1 {
                    "clip"
                } else {
                    "clips"
                },
                size_label(filtered_bytes)
            ));

        let mut content = div()
            .key_context(PAGE_CONTEXT)
            .on_action(cx.listener(|this, _: &FocusSearch, window, cx| {
                window.focus(this.search.read(cx).focus_handle());
            }))
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
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .text_xl()
                            .text_color(t.colors.text_secondary)
                            .child("▣"),
                    )
                    .child(
                        div()
                            .text_2xl()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child("Library"),
                    ),
            )
            .child(toolbar)
            .child(count);
        if let Some(message) = &self.notice {
            content = content.child(
                panel(&t)
                    .id("library-notice")
                    .max_h(px(88.))
                    .overflow_y_scroll()
                    .p_3()
                    .flex()
                    .items_start()
                    .gap_3()
                    .text_sm()
                    .child(div().flex_1().child(message.clone()))
                    .child(
                        div()
                            .id("library-notice-close")
                            .cursor_pointer()
                            .text_color(t.colors.text_muted)
                            .hover(|s| s.text_color(t.colors.text))
                            .child("×")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.notice = None;
                                cx.notify();
                            })),
                    ),
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
            .gap(px(if list { 8. } else { GAP }))
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
        } else if let (Some(error), true) = (&self.index_error, self.clips.is_empty()) {
            grid = grid.child(empty_state(
                &t,
                "▤",
                "Clips folder unavailable",
                error.clone(),
            ));
        } else if ids.is_empty() {
            grid = grid.child(empty_state(
                &t,
                "▤",
                "No clips found",
                if self.clips.is_empty() {
                    "Save a replay to start your library."
                } else {
                    "Try another tab, search or filter."
                },
            ));
        } else if list {
            for index in &ids {
                grid = grid.child(self.clip(&t, *index, true, viewport, window, cx));
            }
        } else {
            for chunk in ids.chunks(self.columns) {
                let mut row = div().flex().gap(px(GAP));
                for index in chunk {
                    row = row.child(self.clip(&t, *index, false, viewport, window, cx));
                }
                for _ in chunk.len()..self.columns {
                    row = row.child(div().flex_1().flex_basis(px(0.)).min_w_0());
                }
                grid = grid.child(row);
            }
        }
        content = content.child(grid);

        // Bottom selection bar.
        let shown = self.ordered();
        let all_selected =
            !shown.is_empty() && shown.iter().all(|id| self.selection.ids.contains(id));
        let select_all =
            div()
                .id("library-select-all")
                .flex()
                .items_center()
                .gap_3()
                .cursor_pointer()
                .tooltip(crate::components::tooltip(if all_selected {
                    "Clear selection (Esc)"
                } else {
                    "Select all (Ctrl+A)"
                }))
                .child(check_mark(&t, all_selected, 22.))
                .child(div().text_sm().text_color(t.colors.text_secondary).child(
                    if selected == 0 {
                        "No clips selected".to_owned()
                    } else {
                        format!("{selected} selected • {}", size_label(selected_bytes))
                    },
                ))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if all_selected {
                        this.selection.clear();
                    } else {
                        this.selection.all(&this.ordered());
                    }
                    this.delete_targets.clear();
                    this.rename_target = None;
                    cx.notify();
                }));
        let pages = self.filtered.len().div_ceil(PAGE_SIZE).max(1);
        let pager = (pages > 1).then(|| {
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(control(
                    &t,
                    17,
                    "‹",
                    self.page > 0 && enabled,
                    ButtonVariant::Ghost,
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
                        .child(format!("Page {} / {pages}", self.page + 1)),
                )
                .child(control(
                    &t,
                    18,
                    "›",
                    (self.page + 1) < pages && enabled,
                    ButtonVariant::Ghost,
                    &self.controls[18],
                    cx,
                    |this, window, cx| {
                        this.page += 1;
                        this.scroll.scroll_to_top_of_item(0);
                        this.previews(window, cx);
                        cx.notify();
                    },
                ))
        });
        let bar = panel(&t)
            .flex_none()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_2()
            .px_4()
            .py_3()
            .child(select_all)
            .child(div().flex_1())
            .children(pager)
            .child(control(
                &t,
                10,
                "✕  Delete",
                enabled && selected > 0,
                ButtonVariant::Secondary,
                &self.controls[10],
                cx,
                |this, _, cx| this.confirm_delete(cx),
            ))
            .child(control(
                &t,
                7,
                if all_favorite {
                    "★  Unfavorite"
                } else {
                    "☆  Favorite"
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
                "↗  Open Folder",
                enabled && selected == 1,
                ButtonVariant::Secondary,
                &self.controls[8],
                cx,
                |this, _, cx| this.folder(cx),
            ))
            .child(control(
                &t,
                6,
                "✂  Open in Editor",
                enabled && selected == 1,
                ButtonVariant::Primary,
                &self.controls[6],
                cx,
                |this, _, cx| this.open(true, cx),
            ));
        content = content.child(bar);

        if let Some(position) = self.context_position {
            let single = selected == 1;
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
                    enabled && single,
                    ButtonVariant::Ghost,
                    &self.controls[30],
                    cx,
                    |this, _, cx| this.open(false, cx),
                ))
                .child(control(
                    &t,
                    31,
                    "Open in Editor",
                    enabled && single,
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
                    "Rename (F2)",
                    enabled && single,
                    ButtonVariant::Ghost,
                    &self.controls[33],
                    cx,
                    |this, window, cx| this.begin_rename(window, cx),
                ))
                .child(control(
                    &t,
                    35,
                    "Open Folder",
                    enabled && single,
                    ButtonVariant::Ghost,
                    &self.controls[35],
                    cx,
                    |this, _, cx| this.folder(cx),
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
        content
    }
}

impl ClipLibrary {
    /// One small icon button on a card; acts on that card's clip only.
    #[allow(clippy::too_many_arguments)]
    fn card_action(
        &self,
        t: &Theme,
        id: &str,
        slot: usize,
        glyph: &'static str,
        hint: &'static str,
        primary: bool,
        cx: &mut Context<Self>,
        act: impl Fn(&mut ClipLibrary, &str, &mut Window, &mut Context<ClipLibrary>) + 'static,
    ) -> Stateful<Div> {
        let target = id.to_owned();
        let enabled = !self.busy;
        div()
            .id(SharedString::from(format!("clip-action-{slot}-{id}")))
            .flex_1()
            .h(px(30.))
            .flex()
            .items_center()
            .justify_center()
            .rounded_md()
            .text_sm()
            .tooltip(crate::components::tooltip(hint))
            .map(|d| {
                if primary {
                    d.bg(t.colors.accent)
                        .text_color(t.colors.accent_foreground)
                        .hover(|s| s.bg(t.colors.accent_hover))
                } else {
                    d.bg(t.colors.surface_raised)
                        .text_color(t.colors.text_secondary)
                        .hover(|s| s.bg(t.colors.surface_hover).text_color(t.colors.text))
                }
            })
            .when(enabled, |d| d.cursor_pointer())
            .when(!enabled, |d| d.opacity(0.45))
            .child(glyph)
            .on_click(cx.listener(move |this, _, window, cx| {
                cx.stop_propagation();
                if enabled {
                    act(this, &target, window, cx);
                }
            }))
    }

    fn clip(
        &self,
        t: &Theme,
        index: usize,
        list: bool,
        viewport: f32,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let clip = &self.clips[index];
        let id = clip.id.clone();
        let click = id.clone();
        let context_id = id.clone();
        let check_id = id.clone();
        let more_id = id.clone();
        let selected = self.selection.ids.contains(&id);
        let favorite = self.settings.favorites.contains(&id);
        let thumb_height = if list {
            72.
        } else {
            (card_width(viewport, self.columns) * THUMB_RATIO).max(90.)
        };

        let checkbox = div()
            .id(SharedString::from(format!("clip-check-{id}")))
            .absolute()
            .top(px(10.))
            .left(px(10.))
            .cursor_pointer()
            .tooltip(crate::components::tooltip("Select (Ctrl+click)"))
            .child(check_mark(t, selected, 22.))
            .on_click(cx.listener(move |this, _, window, cx| {
                cx.stop_propagation();
                if !this.busy {
                    this.context_position = None;
                    this.delete_targets.clear();
                    this.rename_target = None;
                    this.selection
                        .select(&check_id, &this.ordered(), true, false);
                    this.cursor = Some(check_id.clone());
                    window.focus(&this.focus);
                    cx.notify();
                }
            }));
        let badge = clip.duration.map(|duration| {
            div()
                .absolute()
                .bottom(px(8.))
                .right(px(8.))
                .px_1p5()
                .py_0p5()
                .rounded(px(4.))
                .bg(gpui::black().opacity(0.72))
                .text_xs()
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(gpui::white())
                .child(duration_label(duration))
        });
        let thumb = div()
            .relative()
            .overflow_hidden()
            .bg(t.colors.media_background)
            .when(list, |d| {
                d.w(px(128.)).h(px(thumb_height)).flex_none().rounded_md()
            })
            .when(!list, |d| d.w_full().h(px(thumb_height)).rounded_t_lg())
            .map(|d| match self.images.get(&id) {
                Some(image) => d.child(img(image.clone()).size_full().object_fit(ObjectFit::Cover)),
                None => d
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_xs()
                    .text_color(t.colors.text_muted)
                    .child("No preview"),
            })
            .child(checkbox)
            .children(badge);

        let date = relative_time(SystemTime::now(), clip.created.unwrap_or(clip.modified));
        let more = div()
            .id(SharedString::from(format!("clip-more-{id}")))
            .flex_none()
            .px_1()
            .rounded_md()
            .cursor_pointer()
            .text_color(t.colors.text_secondary)
            .hover(|s| s.bg(t.colors.surface_hover).text_color(t.colors.text))
            .tooltip(crate::components::tooltip("More actions"))
            .child("⋯")
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    if this.busy {
                        return;
                    }
                    this.focus_clip(&more_id);
                    window.focus(&this.focus);
                    this.context_position = Some(event.position);
                    cx.notify();
                }),
            );
        let details = div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_0p5()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().text_xs().text_color(t.colors.accent).child("◆"))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .text_color(t.colors.text_secondary)
                            .child(origin_label(clip)),
                    )
                    .child(more),
            )
            .child(
                div()
                    .truncate()
                    .text_base()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(clip.name.clone()),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(t.colors.text_muted)
                    .truncate()
                    .child(format!("{date}  •  {}", size_label(clip.size_bytes))),
            );
        let actions = div()
            .flex()
            .gap_2()
            .when(list, |d| d.w(px(260.)).flex_none())
            .child(
                self.card_action(t, &id, 0, "▶", "Play", true, cx, |this, id, _, cx| {
                    this.focus_clip(id);
                    this.open(false, cx);
                }),
            )
            .child(self.card_action(
                t,
                &id,
                1,
                "✂",
                "Open in Editor",
                false,
                cx,
                |this, id, _, cx| {
                    this.focus_clip(id);
                    this.open(true, cx);
                },
            ))
            .child(self.card_action(
                t,
                &id,
                2,
                if favorite { "★" } else { "☆" },
                if favorite {
                    "Remove from favorites"
                } else {
                    "Add to favorites"
                },
                false,
                cx,
                |this, id, window, cx| this.toggle_favorite(id, window, cx),
            ))
            .child(
                self.card_action(t, &id, 3, "✕", "Delete", false, cx, |this, id, _, cx| {
                    this.focus_clip(id);
                    this.confirm_delete(cx);
                }),
            )
            .child(self.card_action(
                t,
                &id,
                4,
                "↗",
                "Show in folder",
                false,
                cx,
                |this, id, _, cx| {
                    this.focus_clip(id);
                    this.folder(cx);
                },
            ));

        div()
            .id(SharedString::from(format!("library-clip-{id}")))
            .min_w_0()
            .rounded_lg()
            .border_2()
            .bg(t.colors.surface)
            .border_color(if selected {
                t.colors.accent
            } else {
                t.colors.border
            })
            .when(!selected, |d| {
                d.hover(|s| s.border_color(t.colors.border_strong))
            })
            .when(selected && self.focus.is_focused(window), |d| d.shadow_sm())
            .cursor_pointer()
            .map(|d| {
                if list {
                    d.flex()
                        .flex_row()
                        .items_center()
                        .gap_4()
                        .p_2()
                        .child(thumb)
                        .child(details)
                        .child(actions)
                } else {
                    d.flex_1()
                        .flex_basis(px(0.))
                        .flex()
                        .flex_col()
                        .child(thumb)
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_2()
                                .px_3()
                                .pt_2()
                                .pb_3()
                                .child(details)
                                .child(actions),
                        )
                }
            })
            .on_mouse_down(
                gpui::MouseButton::Right,
                cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
                    if !this.selection.ids.contains(&context_id) {
                        this.select(&context_id, false, false, window, cx);
                    }
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn four_columns_at_the_reference_size_fewer_when_narrow() {
        assert_eq!(columns_for(1672.), 4);
        assert_eq!(columns_for(1920.), 4);
        assert_eq!(columns_for(1100.), 2);
        assert_eq!(columns_for(400.), 1);
        assert!(card_width(1672., 4) >= MIN_CARD_WIDTH);
    }

    #[test]
    fn duration_labels_match_the_badge_format() {
        use std::time::Duration;
        assert_eq!(duration_label(Duration::from_secs(45)), "00:45");
        assert_eq!(duration_label(Duration::from_secs(60)), "01:00");
        assert_eq!(duration_label(Duration::from_secs(3_725)), "1:02:05");
    }
}
