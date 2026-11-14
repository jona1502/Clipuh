//! Shared, paged clip library. All scanning and decoding runs in LibraryWorker.

use crate::{
    components::{
        ButtonVariant, DropdownOption, TextInput, TextInputEvent, button, dropdown, empty_state,
        panel,
    },
    library_worker::{Event, LibraryWorker, Request},
    recent_clips::{RecentClip, RecentClips, relative_time, size_label},
    theme::{ActiveTheme, Theme},
};
use clipper_platform::windows::{shell, watch::DirectoryWatch};
use clipper_storage::{
    config::{LibrarySettings, LibrarySort, LibraryView},
    library::{self, Clip, Selection},
};
use gpui::{
    App, Context, Div, Entity, EventEmitter, FocusHandle, KeyBinding, ObjectFit, Render,
    RenderImage, SharedString, Stateful, Window, actions, div, img, prelude::*, px,
};
use std::{
    collections::{BTreeSet, HashMap},
    path::PathBuf,
    sync::Arc,
    time::SystemTime,
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Menu {
    Source,
    Sort,
}
const SORTS: [LibrarySort; 6] = [
    LibrarySort::Newest,
    LibrarySort::Oldest,
    LibrarySort::NameAsc,
    LibrarySort::NameDesc,
    LibrarySort::Duration,
    LibrarySort::Size,
];
mod presentation;
const PAGE_SIZE: usize = 24;
const CONTEXT: &str = "ClipLibrary";
actions!(clip_library, [Play, Edit, Rename, Delete, SelectAll, Clear]);

pub(super) fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("escape", Clear, Some("LibraryButton")),
        KeyBinding::new("enter", Play, Some(CONTEXT)),
        KeyBinding::new("ctrl-e", Edit, Some(CONTEXT)),
        KeyBinding::new("f2", Rename, Some(CONTEXT)),
        KeyBinding::new("delete", Delete, Some(CONTEXT)),
        KeyBinding::new("ctrl-a", SelectAll, Some(CONTEXT)),
        KeyBinding::new("escape", Clear, Some(CONTEXT)),
        KeyBinding::new("enter", crate::components::Activate, Some("LibraryButton")),
        KeyBinding::new("space", crate::components::Activate, Some("LibraryButton")),
    ]);
}

pub(super) enum LibraryEvent {
    Changed,
    Settings(LibrarySettings),
    Editor(PathBuf),
}

pub(super) struct ClipLibrary {
    worker: LibraryWorker,
    directory: PathBuf,
    watcher: Option<DirectoryWatch>,
    watch_tx: async_channel::Sender<()>,
    /// Bumped per refresh so only the newest scan's result is applied.
    generation: u64,
    clips: Vec<Clip>,
    recent_indices: Vec<usize>,
    cursor: Option<String>,
    index_error: Option<String>,
    images: HashMap<String, Arc<RenderImage>>,
    settings: LibrarySettings,
    selection: Selection,
    search: Entity<TextInput>,
    rename_input: Entity<TextInput>,
    query: String,
    source: Option<String>,
    menu: Option<(Menu, usize)>,
    context_position: Option<gpui::Point<gpui::Pixels>>,
    favorites_only: bool,
    filtered: Vec<usize>,
    page: usize,
    columns: usize,
    visible: bool,
    scroll: gpui::ScrollHandle,
    duration_pending: bool,
    loading: bool,
    busy: bool,
    notice: Option<String>,
    rename_target: Option<Clip>,
    delete_targets: Vec<Clip>,
    focus: FocusHandle,
    controls: Vec<FocusHandle>,
}
impl EventEmitter<LibraryEvent> for ClipLibrary {}

impl ClipLibrary {
    pub(super) fn new(
        settings: LibrarySettings,
        directory: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (worker, events) = LibraryWorker::start();
        let (watch_tx, watch_rx) = async_channel::bounded(1);
        let search = cx.new(|cx| {
            TextInput::new(cx)
                .placeholder("Search clips…")
                .tab_index(20)
        });
        let rename_input = cx.new(|cx| {
            TextInput::new(cx)
                .max_len(255)
                .placeholder("Clip name")
                .tab_index(21)
        });
        cx.subscribe_in(&search, window, |this, _, event, window, cx| match event {
            TextInputEvent::Changed(text) => {
                this.query = text.clone();
                this.page = 0;
                this.refilter(window, cx);
            }
            TextInputEvent::Dismissed | TextInputEvent::Submitted(_) => window.focus(&this.focus),
        })
        .detach();
        cx.subscribe_in(
            &rename_input,
            window,
            |this, _, event, window, cx| match event {
                TextInputEvent::Submitted(name) => this.rename(name.clone(), window, cx),
                TextInputEvent::Dismissed => {
                    this.rename_target = None;
                    window.focus(&this.focus);
                    cx.notify();
                }
                _ => {}
            },
        )
        .detach();
        cx.spawn_in(window, async move |this, cx| {
            while let Ok(event) = events.recv().await {
                if this
                    .update_in(cx, |this, window, cx| this.receive(event, window, cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        cx.spawn_in(window, async move |this, cx| {
            while watch_rx.recv().await.is_ok() {
                if this.update_in(cx, |this, _, cx| this.refresh(cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
        let mut this = Self {
            worker,
            directory,
            watcher: None,
            watch_tx,
            generation: 0,
            clips: vec![],
            recent_indices: vec![],
            cursor: None,
            index_error: None,
            images: HashMap::new(),
            duration_pending: settings.sort == LibrarySort::Duration,
            settings,
            selection: Selection::default(),
            search,
            rename_input,
            query: String::new(),
            source: None,
            menu: None,
            context_position: None,
            favorites_only: false,
            filtered: vec![],
            page: 0,
            columns: 4,
            visible: false,
            scroll: gpui::ScrollHandle::default(),
            loading: true,
            busy: false,
            notice: None,
            rename_target: None,
            delete_targets: vec![],
            focus: cx.focus_handle().tab_index(35).tab_stop(true),
            controls: (0..64)
                .map(|n| cx.focus_handle().tab_index(22 + n).tab_stop(true))
                .collect(),
        };
        this.start_watch(window, cx);
        this.refresh(cx);
        this
    }

    fn start_watch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let directory = self.directory.clone();
        let expected = directory.clone();
        let tx = self.watch_tx.clone();
        self.watcher = None;
        cx.spawn_in(window, async move |this, cx| {
            let result = cx.background_spawn(async move { clipper_platform::windows::watch::watch(&directory, tx) }).await;
            this.update_in(cx, |this, _, cx| {
                if this.directory != expected { return; }
                match result {
                    Ok(watcher) => this.watcher = Some(watcher),
                    Err(error) => this.notice = Some(format!("Folder monitoring unavailable: {error}. Use Refresh after external changes.")),
                }
                cx.notify();
            }).ok();
        }).detach();
    }

    pub(super) fn set_directory(
        &mut self,
        directory: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.directory != directory {
            self.directory = directory;
            self.clips.clear();
            self.selection.clear();
            self.page = 0;
            self.start_watch(window, cx);
        } else if self.watcher.is_none() {
            self.start_watch(window, cx);
        }
        self.refresh(cx);
    }

    pub(super) fn set_visible(
        &mut self,
        visible: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.visible != visible {
            self.visible = visible;
            self.previews(window, cx);
        }
    }

    pub(super) fn show_error(&mut self, message: String, cx: &mut Context<Self>) {
        self.notice = Some(message);
        cx.notify();
    }

    pub(super) fn refresh(&mut self, cx: &mut Context<Self>) {
        self.generation += 1;
        self.duration_pending = self.settings.sort == LibrarySort::Duration;
        self.worker.request(Request {
            directory: self.directory.clone(),
            refresh: true,
            metadata_all: self.visible && self.settings.sort == LibrarySort::Duration,
            wanted: vec![],
            generation: self.generation,
        });
        cx.notify();
    }

    fn ordered(&self) -> Vec<String> {
        self.filtered
            .iter()
            .map(|i| self.clips[*i].id.clone())
            .collect()
    }

    fn sources(&self) -> Vec<Option<String>> {
        let mut values = vec![None];
        values.extend(
            self.clips
                .iter()
                .filter_map(|c| c.source.clone())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .map(Some),
        );
        values
    }

    fn choose_menu(
        &mut self,
        menu: Menu,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match menu {
            Menu::Source => self.source = self.sources().get(index).cloned().flatten(),
            Menu::Sort => {
                self.settings.sort = SORTS[index.min(SORTS.len() - 1)];
                self.duration_pending = self.settings.sort == LibrarySort::Duration;
                self.changed_settings(cx);
            }
        }
        self.menu = None;
        self.page = 0;
        self.refilter(window, cx);
    }

    fn step_menu(&mut self, delta: isize, cx: &mut Context<Self>) {
        if let Some((menu, index)) = self.menu {
            let len = if menu == Menu::Sort {
                SORTS.len()
            } else {
                self.sources().len()
            };
            self.menu = Some((
                menu,
                (index as isize + delta).rem_euclid(len as isize) as usize,
            ));
            cx.notify();
        } else {
            cx.propagate();
        }
    }

    fn menu_control(&self, t: &Theme, menu: Menu, window: &Window, cx: &mut Context<Self>) -> Div {
        let sources = self.sources();
        let (labels, selected, slot): (Vec<String>, usize, usize) = if menu == Menu::Source {
            (
                sources
                    .iter()
                    .map(|s| s.clone().unwrap_or_else(|| "All sources".into()))
                    .collect(),
                sources.iter().position(|s| s == &self.source).unwrap_or(0),
                0,
            )
        } else {
            (
                SORTS
                    .iter()
                    .map(|s| sort_label(*s).trim_end_matches(" ▾").to_owned())
                    .collect(),
                SORTS
                    .iter()
                    .position(|s| *s == self.settings.sort)
                    .unwrap_or(0),
                2,
            )
        };
        let options: Vec<_> = labels
            .into_iter()
            .map(|label| DropdownOption {
                label: label.into(),
                detail: None,
                enabled: true,
                reason: None,
            })
            .collect();
        let highlighted = self
            .menu
            .filter(|(kind, _)| *kind == menu)
            .map(|(_, index)| index);
        let toggle = cx.entity().downgrade();
        let select = toggle.clone();
        let dismiss = toggle.clone();
        div().w(px(150.)).child(dropdown(
            t,
            ("library-menu", slot),
            &options,
            selected,
            highlighted,
            &self.controls[slot],
            self.controls[slot].is_focused(window),
            !self.busy && (menu == Menu::Sort || sources.len() > 1),
            move |_, cx| {
                toggle
                    .update(cx, |this, cx| {
                        this.menu = if this.menu.is_some_and(|(kind, _)| kind == menu) {
                            None
                        } else {
                            Some((menu, selected))
                        };
                        cx.notify();
                    })
                    .ok();
            },
            move |index, window, cx| {
                select
                    .update(cx, |this, cx| this.choose_menu(menu, index, window, cx))
                    .ok();
            },
            move |_, cx| {
                dismiss
                    .update(cx, |this, cx| {
                        this.menu = None;
                        cx.notify();
                    })
                    .ok();
            },
        ))
    }

    fn refilter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.update_filter();
        self.scroll.scroll_to_top_of_item(0);
        self.previews(window, cx);
        cx.notify();
    }

    fn update_filter(&mut self) {
        self.filtered = library::query(
            &self.clips,
            &self.query,
            self.source.as_deref(),
            self.favorites_only,
            &self.settings.favorites,
            self.settings.sort,
        );
        self.page = self
            .page
            .min(self.filtered.len().saturating_sub(1) / PAGE_SIZE);
    }

    fn previews(&mut self, window: &mut Window, _cx: &mut Context<Self>) {
        self.generation += 1;
        self.previews_current_generation(window);
    }

    fn previews_current_generation(&mut self, window: &mut Window) {
        let mut wanted: BTreeSet<String> = self
            .filtered
            .iter()
            .skip(self.page * PAGE_SIZE)
            .take(if self.visible { PAGE_SIZE } else { 0 })
            .map(|i| self.clips[*i].id.clone())
            .collect();
        for clip in self.newest().into_iter().take(3) {
            wanted.insert(clip.id.clone());
        }
        self.images.retain(|id, image| {
            if wanted.contains(id) {
                true
            } else {
                let _ = window.drop_image(image.clone());
                false
            }
        });
        self.worker.request(Request {
            directory: self.directory.clone(),
            refresh: false,
            metadata_all: self.visible && self.settings.sort == LibrarySort::Duration,
            wanted: wanted.into_iter().collect(),
            generation: self.generation,
        });
    }

    fn receive(&mut self, event: Event, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            Event::MetadataReady { generation } if generation == self.generation => {
                self.duration_pending = false;
            }
            Event::Index { generation, clips } if generation == self.generation => {
                // Invalidate displayed textures before same-ID replacements arrive.
                self.images.retain(|id, image| {
                    let same = self
                        .clips
                        .iter()
                        .find(|c| &c.id == id)
                        .zip(clips.iter().find(|c| &c.id == id))
                        .is_some_and(|(a, b)| {
                            a.modified == b.modified && a.size_bytes == b.size_bytes
                        });
                    if !same {
                        let _ = window.drop_image(image.clone());
                    }
                    same
                });
                // Keep favorites for offline volumes and other configured directories.
                // Explicit successful deletion removes its ID; scans never erase preferences.
                self.context_position = None;
                self.clips = clips;
                self.index_error = None;
                self.recent_indices = (0..self.clips.len()).collect();
                self.recent_indices.sort_by(|a, b| {
                    let a = &self.clips[*a];
                    let b = &self.clips[*b];
                    b.created
                        .unwrap_or(b.modified)
                        .cmp(&a.created.unwrap_or(a.modified))
                        .then(a.id.cmp(&b.id))
                });
                self.recent_indices.truncate(3);
                self.loading = false;
                self.selection.retain(&self.clips);
                self.update_filter();
                self.previews_current_generation(window);
            }
            Event::Preview {
                generation,
                id,
                duration,
                image,
            } if generation == self.generation => {
                if let Some(clip) = self.clips.iter_mut().find(|c| c.id == id) {
                    clip.duration = duration;
                }
                if let Some(image) = image {
                    self.images.insert(id, image);
                }
                if self.settings.sort == LibrarySort::Duration {
                    self.filtered = library::query(
                        &self.clips,
                        &self.query,
                        self.source.as_deref(),
                        self.favorites_only,
                        &self.settings.favorites,
                        self.settings.sort,
                    );
                }
            }
            Event::Error {
                generation,
                message,
            } if generation == self.generation => {
                self.duration_pending = false;
                self.index_error = Some(message.clone());
                self.notice = Some(message);
                self.loading = false;
            }
            _ => return,
        }
        cx.emit(LibraryEvent::Changed);
        cx.notify();
    }

    fn newest(&self) -> Vec<&Clip> {
        self.recent_indices
            .iter()
            .filter_map(|i| self.clips.get(*i))
            .collect()
    }

    pub(super) fn recent(&self) -> RecentClips {
        if self.loading {
            return RecentClips::Loading;
        }
        if let Some(error) = &self.index_error {
            return RecentClips::Unavailable(error.clone());
        }
        RecentClips::Ready(
            self.newest()
                .into_iter()
                .take(3)
                .map(|c| RecentClip {
                    clip: (*c).clone(),
                    thumbnail: self.images.get(&c.id).cloned(),
                })
                .collect(),
        )
    }

    fn changed_settings(&self, cx: &mut Context<Self>) {
        cx.emit(LibraryEvent::Settings(self.settings.clone()));
    }
    fn selected(&self) -> Vec<Clip> {
        self.clips
            .iter()
            .filter(|c| self.selection.ids.contains(&c.id))
            .cloned()
            .collect()
    }
    fn select(
        &mut self,
        id: &str,
        ctrl: bool,
        shift: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy {
            return;
        }
        self.context_position = None;
        self.delete_targets.clear();
        self.rename_target = None;
        self.selection.select(id, &self.ordered(), ctrl, shift);
        self.cursor = Some(id.to_owned());
        window.focus(&self.focus);
        cx.notify();
    }
    fn favorite(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ids: Vec<_> = self.selection.ids.iter().cloned().collect();
        let remove = ids.iter().all(|id| self.settings.favorites.contains(id));
        for id in ids {
            if remove {
                self.settings.favorites.remove(&id);
            } else {
                self.settings.favorites.insert(id);
            }
        }
        self.changed_settings(cx);
        self.refilter(window, cx);
    }
    fn open(&mut self, edit: bool, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let selected = self.selected();
        if selected.len() != 1 {
            return;
        }
        let clip = &selected[0];
        if let Err(error) = library::verify(clip) {
            self.notice = Some(format!("Clip unavailable: {error}. Refresh the library."));
            cx.notify();
            return;
        }
        if edit {
            cx.emit(LibraryEvent::Editor(clip.path.clone()));
        } else if let Err(error) = shell::open_path(&clip.path) {
            self.notice = Some(format!("Could not play clip: {error}"));
            cx.notify();
        }
    }
    fn folder(&mut self, cx: &mut Context<Self>) {
        if let Some(clip) = self.selected().first()
            && let Err(error) =
                library::verify(clip).and_then(|()| shell::reveal_in_folder(&clip.path))
        {
            self.notice = Some(format!("Could not open folder: {error}"));
            cx.notify();
        }
    }
    fn begin_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let selected = self.selected();
        if selected.len() != 1 || self.busy {
            return;
        }
        self.rename_input
            .update(cx, |input, cx| input.set_text(selected[0].name.clone(), cx));
        self.rename_target = Some(selected[0].clone());
        self.delete_targets.clear();
        window.focus(self.rename_input.read(cx).focus_handle());
        cx.notify();
    }
    fn rename(&mut self, name: String, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(clip) = self.rename_target.clone() else {
            return;
        };
        if self.busy {
            return;
        }
        self.busy = true;
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { library::rename(&clip, &name) })
                .await;
            this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(_) => {
                        this.rename_target = None;
                        this.notice = Some("Clip renamed.".into());
                        this.refresh(cx);
                    }
                    Err(error) => this.notice = Some(format!("Rename failed: {error}")),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
    fn confirm_delete(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.delete_targets = self.selected();
        self.rename_target = None;
        cx.notify();
    }
    fn delete(&mut self, cx: &mut Context<Self>) {
        if self.busy || self.delete_targets.is_empty() {
            return;
        }
        let targets = self.delete_targets.clone();
        self.busy = true;
        cx.spawn(async move |this, cx| {
            let results = cx
                .background_spawn(async move {
                    // IFileOperation requires STA; executor threads may already be MTA.
                    std::thread::spawn(move || {
                        targets
                            .into_iter()
                            .map(|clip| {
                                let result = library::verify(&clip)
                                    .and_then(|()| shell::recycle_file(&clip.path))
                                    .map_err(|e| format!("{}: {e}", clip.name));
                                (clip.id, result)
                            })
                            .collect::<Vec<_>>()
                    })
                    .join()
                    .unwrap_or_else(|_| {
                        vec![(
                            String::new(),
                            Err("Recycle worker failed; refresh to inspect files.".into()),
                        )]
                    })
                })
                .await;
            this.update(cx, |this, cx| {
                this.busy = false;
                this.delete_targets.clear();
                let mut errors = vec![];
                let mut removed = 0;
                for (id, result) in results {
                    match result {
                        Ok(()) => {
                            this.selection.ids.remove(&id);
                            this.settings.favorites.remove(&id);

                            removed += 1;
                        }
                        Err(error) => errors.push(error),
                    }
                }
                this.notice = Some(if errors.is_empty() {
                    format!("{removed} clip(s) moved to Recycle Bin.")
                } else {
                    format!(
                        "{removed} removed; {} failed. {}",
                        errors.len(),
                        errors
                            .iter()
                            .take(3)
                            .cloned()
                            .collect::<Vec<_>>()
                            .join("; ")
                    )
                });
                this.changed_settings(cx);
                this.refresh(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn move_selection(
        &mut self,
        delta: isize,
        extend: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ordered = self.ordered();
        if ordered.is_empty() {
            return;
        }
        let at = self
            .cursor
            .as_ref()
            .and_then(|id| ordered.iter().position(|i| i == id))
            .unwrap_or(0);
        let next = (at as isize + delta).clamp(0, ordered.len() as isize - 1) as usize;
        self.select(&ordered[next], false, extend, window, cx);
        let page = next / PAGE_SIZE;
        if self.page != page {
            self.page = page;
            self.previews(window, cx);
        }
        let row = (next % PAGE_SIZE)
            / if self.settings.view == LibraryView::List {
                1
            } else {
                self.columns
            };
        self.scroll.scroll_to_item(row);
    }
}

fn sort_label(sort: LibrarySort) -> &'static str {
    match sort {
        LibrarySort::Newest => "Newest ▾",
        LibrarySort::Oldest => "Oldest ▾",
        LibrarySort::NameAsc => "Name A–Z ▾",
        LibrarySort::NameDesc => "Name Z–A ▾",
        LibrarySort::Duration => "Duration ▾",
        LibrarySort::Size => "File size ▾",
    }
}
