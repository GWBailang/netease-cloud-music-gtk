use crate::download::{
    manager::{DownloadManager, EnqueueReport},
    model::*,
    storage,
};
use crate::download_dialog::capture_folder;
use adw::prelude::*;
use gettextrs::gettext;
use gtk::{gio, glib};
use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, HashMap, HashSet},
    path::{Path, PathBuf},
    rc::Rc,
};

pub fn enqueue_notice(report: &EnqueueReport) -> String {
    let parts = [
        (report.queued, gettext("Added to download queue")),
        (
            report.already_saved,
            gettext("Already saved; skipped duplicate downloads"),
        ),
        (report.already_queued, gettext("Already in download queue")),
        (report.unavailable, gettext("Unavailable songs")),
    ]
    .into_iter()
    .filter(|(count, _)| *count > 0)
    .map(|(count, label)| format!("{label}: {count}"))
    .collect::<Vec<_>>();
    if parts.is_empty() {
        gettext("No songs to download")
    } else {
        parts.join(" · ")
    }
}

type Retry = Box<dyn Fn(TaskId)>;
struct Data {
    manager: DownloadManager,
    snapshot: RefCell<ManagerSnapshot>,
    selected: RefCell<HashSet<String>>,
    retry: RefCell<Vec<Retry>>,
    task_rows: RefCell<HashMap<TaskId, (adw::ActionRow, gtk::ProgressBar, gtk::Button)>>,
    task_filter: Cell<usize>,
    directory: RefCell<Option<(LocationId, PathBuf)>>,
}
#[derive(Clone)]
pub struct DownloadsPage {
    pub widget: gtk::Box,
    pub builder: gtk::Builder,
    data: Rc<Data>,
}
impl DownloadsPage {
    pub fn new(manager: DownloadManager) -> Self {
        let builder = gtk::Builder::new();
        builder.set_translation_domain(Some("netease-cloud-music-gtk4"));
        builder
            .add_from_resource("/com/gitee/gmg137/NeteaseCloudMusicGtk4/gtk/downloads-page.ui")
            .expect("Downloads resource");
        let widget = builder.object::<gtk::Box>("downloads_page").unwrap();
        let data = Rc::new(Data {
            snapshot: RefCell::new(manager.snapshot()),
            manager,
            selected: RefCell::new(HashSet::new()),
            retry: RefCell::new(vec![]),
            task_rows: RefCell::new(HashMap::new()),
            task_filter: Cell::new(0),
            directory: RefCell::new(None),
        });
        let page = Self {
            widget,
            builder,
            data,
        };
        page.refresh();
        for (index, name) in [
            "tasks_all",
            "tasks_complete",
            "tasks_failed",
            "tasks_active",
        ]
        .iter()
        .enumerate()
        {
            let weak = page.widget.downgrade();
            let shared = page.data.clone();
            page.object::<gtk::ToggleButton>(name)
                .connect_toggled(move |button| {
                    if button.is_active() {
                        shared.task_filter.set(index);
                        if let Some(root) = weak.upgrade() {
                            filter_tasks(&root, &shared);
                        }
                    }
                });
        }
        let weak = page.widget.downgrade();
        let shared = page.data.clone();
        page.object::<gtk::Button>("folder_up")
            .connect_clicked(move |_| {
                let parent = shared.directory.borrow().as_ref().and_then(|(id, path)| {
                    path.parent()
                        .filter(|p| !p.as_os_str().is_empty())
                        .map(|p| (id.clone(), p.to_owned()))
                });
                shared.directory.replace(parent);
                shared.selected.borrow_mut().clear();
                if let Some(root) = weak.upgrade() {
                    render_files(&root, &shared);
                }
            });
        let weak = page.widget.downgrade();
        let shared = page.data.clone();
        page.object::<gtk::Button>("clear_history")
            .connect_clicked(move |_| {
                if let Some(root) = weak.upgrade() {
                    background(&root, shared.clone(), |manager| {
                        manager.clear_history()?;
                        Ok(gettext(
                            "Finished history cleared; saved files are retained",
                        ))
                    });
                }
            });
        let weak = page.widget.downgrade();
        let shared = page.data.clone();
        page.object::<gtk::SearchEntry>("search")
            .connect_search_changed(move |_| {
                shared.selected.borrow_mut().clear();
                if let Some(root) = weak.upgrade() {
                    if object::<gtk::Stack>(&root, "views")
                        .visible_child_name()
                        .as_deref()
                        == Some("saved")
                    {
                        render_files(&root, &shared);
                    }
                }
            });
        let weak = page.widget.downgrade();
        let shared = page.data.clone();
        page.object::<gtk::DropDown>("location_filter")
            .connect_selected_notify(move |_| {
                shared.directory.replace(None);
                shared.selected.borrow_mut().clear();
                if let Some(root) = weak.upgrade() {
                    if object::<gtk::Stack>(&root, "views")
                        .visible_child_name()
                        .as_deref()
                        == Some("saved")
                    {
                        render_files(&root, &shared);
                    }
                }
            });
        let weak = page.widget.downgrade();
        let shared = page.data.clone();
        page.object::<gtk::Button>("select_all")
            .connect_clicked(move |_| {
                if let Some(root) = weak.upgrade() {
                    let list = object::<gtk::ListBox>(&root, "files");
                    let mut child = list.first_child();
                    while let Some(row) = child {
                        if let Some(check) = find(&row, "selection")
                            .and_then(|w| w.downcast::<gtk::CheckButton>().ok())
                        {
                            check.set_active(true);
                        }
                        child = row.next_sibling();
                    }
                    render_files(&root, &shared);
                }
            });
        let weak = page.widget.downgrade();
        let shared = page.data.clone();
        page.object::<gtk::Button>("delete")
            .connect_clicked(move |_| {
                if let Some(root) = weak.upgrade() {
                    delete_options(&root, shared.clone());
                }
            });
        let weak = page.widget.downgrade();
        let shared = page.data.clone();
        page.object::<gtk::Button>("scan")
            .connect_clicked(move |_| {
                if let Some(root) = weak.upgrade() {
                    choose_scan(&root, shared.clone());
                }
            });
        let weak = page.widget.downgrade();
        let shared = page.data.clone();
        page.object::<gtk::Button>("refresh")
            .connect_clicked(move |_| {
                if let Some(root) = weak.upgrade() {
                    background(&root, shared.clone(), |manager| {
                        manager.refresh_files()?;
                        Ok(gettext("File status refreshed"))
                    });
                }
            });
        let weak = page.widget.downgrade();
        let shared = page.data.clone();
        page.object::<gtk::Stack>("views")
            .connect_visible_child_name_notify(move |stack| {
                if stack.visible_child_name().as_deref() == Some("saved") {
                    if let Some(root) = weak.upgrade() {
                        render_files(&root, &shared);
                    }
                }
            });
        let group = gio::SimpleActionGroup::new();
        let action = gio::SimpleAction::new("remove-missing", None);
        let weak = page.widget.downgrade();
        let shared = page.data.clone();
        action.connect_activate(move |_, _| {
            if let Some(root) = weak.upgrade() {
                background(&root, shared.clone(), |manager| {
                    manager.remove_missing()?;
                    Ok(gettext("Missing entries removed"))
                });
            }
        });
        group.add_action(&action);
        page.widget.insert_action_group("downloads", Some(&group));
        page
    }
    pub fn object<T: glib::object::IsA<gtk::Widget>>(&self, id: &str) -> T {
        object(&self.widget, id)
    }
    pub fn connect_retry(&self, callback: impl Fn(TaskId) + 'static) {
        self.data.retry.borrow_mut().push(Box::new(callback));
    }
    pub fn apply_event(&self, event: DownloadEvent) {
        match event {
            DownloadEvent::Task(task) => {
                if let Some(record) = self
                    .data
                    .snapshot
                    .borrow_mut()
                    .tasks
                    .iter_mut()
                    .find(|t| t.id == task.id)
                {
                    *record = task.clone();
                }
                if let Some((row, bar, button)) = self.data.task_rows.borrow().get(&task.id) {
                    row.set_subtitle(&task_description(&task));
                    row.set_tooltip_text(Some(&task_description(&task)));
                    bar.set_fraction(task.progress.clamp(0., 1.));
                    button.set_label(&task_button_label(&task));
                }
                filter_tasks(&self.widget, &self.data);
            }
            DownloadEvent::Changed(snapshot) => {
                *self.data.snapshot.borrow_mut() = snapshot;
                self.refresh();
            }
            DownloadEvent::LocationInvalid(_) => message(
                &self.widget,
                &gettext("Storage device disconnected; choose its folder again before retrying"),
            ),
            DownloadEvent::Error(error) => message(&self.widget, &error),
        }
    }
    fn refresh(&self) {
        let snapshot = self.data.snapshot.borrow();
        self.data
            .selected
            .borrow_mut()
            .retain(|key| snapshot.saved.iter().any(|s| &s.key == key));
        let filter = self.object::<gtk::DropDown>("location_filter");
        let selected = filter.selected();
        let names: Vec<String> = std::iter::once(gettext("All save locations"))
            .chain(
                snapshot
                    .locations
                    .iter()
                    .map(|l| l.root.display().to_string()),
            )
            .collect();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        let current = filter.model().and_downcast::<gtk::StringList>();
        if !current.as_ref().is_some_and(|model| {
            model.n_items() == names.len() as u32
                && names
                    .iter()
                    .enumerate()
                    .all(|(i, s)| model.string(i as u32).as_deref() == Some(s))
        }) {
            filter.set_model(Some(&gtk::StringList::new(&names)));
        }
        filter.set_selected(selected.min(names.len().saturating_sub(1) as u32));
        let list = self.object::<gtk::ListBox>("tasks");
        self.data.task_rows.borrow_mut().retain(|id, (row, _, _)| {
            if snapshot.tasks.iter().any(|task| &task.id == id) {
                true
            } else {
                list.remove(row);
                false
            }
        });
        for task in &snapshot.tasks {
            if let Some((row, bar, button)) = self.data.task_rows.borrow().get(&task.id) {
                let description = task_description(task);
                if row.subtitle().as_deref() != Some(&description) {
                    row.set_subtitle(&description);
                    row.set_tooltip_text(Some(&description));
                }
                bar.set_fraction(task.progress.clamp(0., 1.));
                button.set_label(&task_button_label(task));
                continue;
            }
            let row = adw::ActionRow::builder()
                .title(
                    task.song
                        .as_ref()
                        .map(|s| s.name.as_str())
                        .unwrap_or("Unknown song"),
                )
                .subtitle(task_description(task))
                .subtitle_lines(3)
                .build();
            row.set_use_markup(false);
            row.set_tooltip_text(Some(&task_description(task)));
            let bar = gtk::ProgressBar::builder()
                .valign(gtk::Align::Center)
                .width_request(100)
                .fraction(task.progress.clamp(0., 1.))
                .build();
            row.add_suffix(&bar);
            let button = gtk::Button::builder()
                .label(task_button_label(task))
                .valign(gtk::Align::Center)
                .build();
            let id = task.id;

            let shared = Rc::downgrade(&self.data);
            let weak = self.widget.downgrade();
            button.connect_clicked(move |_| {
                let Some(shared) = shared.upgrade() else {
                    return;
                };
                let terminal = shared
                    .snapshot
                    .borrow()
                    .tasks
                    .iter()
                    .find(|t| t.id == id)
                    .is_some_and(|t| t.stage.terminal());
                if terminal {
                    for callback in shared.retry.borrow().iter() {
                        callback(id);
                    }
                } else {
                    if let Some(root) = weak.upgrade() {
                        let manager = shared.manager.clone();
                        background(&root, shared.clone(), move |_| {
                            manager.cancel(id);
                            Ok(gettext("Download cancelled"))
                        });
                    }
                }
            });
            row.add_suffix(&button);
            list.append(&row);
            self.data
                .task_rows
                .borrow_mut()
                .insert(id, (row, bar, button));
        }
        drop(snapshot);
        filter_tasks(&self.widget, &self.data);
        if self
            .object::<gtk::Stack>("views")
            .visible_child_name()
            .as_deref()
            == Some("saved")
        {
            render_files(&self.widget, &self.data);
        }
    }
}
fn task_matches(stage: TaskStage, filter: usize) -> bool {
    match filter {
        1 => stage == TaskStage::Complete,
        2 => stage.terminal() && stage != TaskStage::Complete,
        3 => !stage.terminal(),
        _ => true,
    }
}
fn filter_tasks(root: &gtk::Box, data: &Data) {
    let snapshot = data.snapshot.borrow();
    let rows = data.task_rows.borrow();
    let mut visible = 0;
    for task in &snapshot.tasks {
        let show = task_matches(task.stage, data.task_filter.get());
        visible += usize::from(show);
        if let Some((row, _, _)) = rows.get(&task.id) {
            row.set_visible(show);
        }
    }
    let empty = object::<adw::StatusPage>(root, "tasks_empty");
    empty.set_visible(visible == 0);
    empty.set_title(&gettext(if snapshot.tasks.is_empty() {
        "No downloads yet"
    } else {
        "No downloads in this category"
    }));
    empty.set_description(Some(&gettext(if snapshot.tasks.is_empty() {
        "Choose songs from a song list to start downloading"
    } else {
        "Choose another category to view downloads"
    })));
    object::<gtk::ScrolledWindow>(root, "tasks_scroll").set_visible(visible > 0);
    object::<gtk::Label>(root, "task_summary").set_label(&format!(
        "{}: {} · {}: {} · {}: {}",
        gettext("Complete"),
        snapshot
            .tasks
            .iter()
            .filter(|t| task_matches(t.stage, 1))
            .count(),
        gettext("Failed"),
        snapshot
            .tasks
            .iter()
            .filter(|t| task_matches(t.stage, 2))
            .count(),
        gettext("Active"),
        snapshot
            .tasks
            .iter()
            .filter(|t| task_matches(t.stage, 3))
            .count(),
    ));
}
fn find(root: &gtk::Widget, id: &str) -> Option<gtk::Widget> {
    if root.buildable_id().as_deref() == Some(id) || root.widget_name() == id {
        return Some(root.clone());
    }
    let mut child = root.first_child();
    while let Some(w) = child {
        if let Some(found) = find(&w, id) {
            return Some(found);
        }
        child = w.next_sibling();
    }
    None
}
fn object<T: IsA<gtk::Widget>>(root: &gtk::Box, id: &str) -> T {
    find(root.upcast_ref(), id)
        .unwrap_or_else(|| panic!("Missing downloads widget {id}"))
        .downcast()
        .unwrap_or_else(|_| panic!("Wrong widget type {id}"))
}
fn clear(list: &gtk::ListBox) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
}
fn message(root: &gtk::Box, text: &str) {
    let label = object::<gtk::Label>(root, "message");
    label.set_label(text);
    label.set_visible(true);
}
fn task_description(task: &TaskRecord) -> String {
    let source = task
        .source_info
        .as_ref()
        .map(|source| format!("{}: {source}\n", gettext("Source audio")))
        .unwrap_or_default();
    let outputs: Vec<String> = task
        .outputs
        .iter()
        .map(|o| {
            format!(
                "{}: {}{}",
                o.suffix,
                gettext(match o.state {
                    OutputState::Pending => "Pending",
                    OutputState::Saved => "Saved",
                    OutputState::Skipped => "Already saved",
                    OutputState::Unavailable => "Unavailable",
                    OutputState::Failed => "Failed",
                }),
                o.error
                    .as_ref()
                    .map(|e| format!(" ({})", gettext(e)))
                    .unwrap_or_default()
            )
        })
        .collect();
    format!(
        "{} · {}% · {}\n{}{}",
        gettext(task.stage.label()),
        (task.progress * 100.) as u32,
        task.options
            .target
            .root
            .join(task.basename.parent().unwrap_or(std::path::Path::new("")))
            .display(),
        format!("{source}{}", outputs.join(" · ")),
        task.error
            .as_ref()
            .map(|e| format!("\n{}", gettext(e)))
            .unwrap_or_default()
    )
}
fn visible_items(root: &gtk::Box, data: &Data) -> Vec<SavedSong> {
    let text = object::<gtk::SearchEntry>(root, "search")
        .text()
        .to_lowercase();
    let filter = object::<gtk::DropDown>(root, "location_filter").selected();
    let snapshot = data.snapshot.borrow();
    let location = if filter > 0 {
        snapshot.locations.get(filter as usize - 1)
    } else {
        None
    };
    snapshot
        .saved
        .iter()
        .filter(|s| {
            location.is_none_or(|l| l.id == s.location.id)
                && data.directory.borrow().as_ref().is_none_or(|(id, path)| {
                    &s.location.id == id && s.files.iter().any(|f| f.relative.starts_with(path))
                })
                && format!(
                    "{} {} {} {}",
                    s.files
                        .iter()
                        .map(|f| f.relative.display().to_string())
                        .collect::<Vec<_>>()
                        .join(" "),
                    s.title,
                    s.song.as_ref().map(|s| s.singer.as_str()).unwrap_or(""),
                    s.location.root.display()
                )
                .to_lowercase()
                .contains(&text)
        })
        .cloned()
        .collect()
}
fn render_files(root: &gtk::Box, data: &Rc<Data>) {
    let mut items = visible_items(root, data);
    let directory = data.directory.borrow().clone();
    let current_path = directory
        .as_ref()
        .map(|(_, path)| path.as_path())
        .unwrap_or(Path::new(""));
    let searching = !object::<gtk::SearchEntry>(root, "search").text().is_empty();
    let mut folders = BTreeMap::new();
    if !searching {
        items.retain(|saved| {
            let parent = saved
                .files
                .first()
                .and_then(|file| file.relative.parent())
                .unwrap_or(Path::new(""));
            let relative = parent.strip_prefix(current_path).unwrap_or(parent);
            if let Some(component) = relative.components().next() {
                let path = current_path.join(component);
                folders.insert(
                    (path, saved.location.id.clone()),
                    saved.location.root.clone(),
                );
                false
            } else {
                true
            }
        });
    }
    let empty = items.is_empty() && folders.is_empty();
    object::<adw::StatusPage>(root, "files_empty").set_visible(empty);
    object::<gtk::ScrolledWindow>(root, "files_scroll").set_visible(!empty);
    object::<gtk::Button>(root, "folder_up").set_sensitive(directory.is_some());
    let path_label = directory
        .as_ref()
        .and_then(|(id, path)| {
            data.snapshot
                .borrow()
                .locations
                .iter()
                .find(|l| &l.id == id)
                .map(|l| l.root.join(path).display().to_string())
        })
        .unwrap_or_else(|| gettext("Save location"));
    object::<gtk::Label>(root, "folder_path").set_label(&path_label);
    object::<gtk::Label>(root, "folder_path").set_tooltip_text(Some(&path_label));
    let list = object::<gtk::ListBox>(root, "files");
    clear(&list);
    for ((path, location_id), location_root) in folders {
        let row = adw::ActionRow::builder()
            .title(path.file_name().unwrap_or_default().to_string_lossy())
            .subtitle(location_root.join(&path).display().to_string())
            .activatable(true)
            .build();
        row.set_use_markup(false);
        row.add_prefix(&gtk::Image::from_icon_name("folder-symbolic"));
        row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
        let weak = root.downgrade();
        let shared = data.clone();
        row.connect_activated(move |_| {
            shared
                .directory
                .replace(Some((location_id.clone(), path.clone())));
            shared.selected.borrow_mut().clear();
            if let Some(root) = weak.upgrade() {
                render_files(&root, &shared);
            }
        });
        list.append(&row);
    }
    for saved in &items {
        let row = adw::ActionRow::builder()
            .title(&saved.title)
            .subtitle(format!(
                "{} · {} · {:.1} MB\n{}",
                if saved.state != SavedState::Available {
                    gettext(match saved.state {
                        SavedState::Missing => "Missing files",
                        SavedState::Changed => "Files changed",
                        SavedState::Unavailable => "Location unavailable",
                        _ => "Available",
                    })
                } else if saved.imported {
                    gettext("Imported")
                } else {
                    gettext("Downloaded")
                },
                saved
                    .files
                    .iter()
                    .map(|f| f
                        .relative
                        .extension()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_uppercase())
                    .collect::<Vec<_>>()
                    .join(" / "),
                saved.files.iter().map(|f| f.fingerprint.size).sum::<u64>() as f64 / 1e6,
                saved
                    .location
                    .root
                    .join(
                        saved
                            .files
                            .first()
                            .and_then(|f| f.relative.parent())
                            .unwrap_or(Path::new(""))
                    )
                    .display()
            ))
            .subtitle_lines(2)
            .build();
        row.set_use_markup(false);
        let check = gtk::CheckButton::builder()
            .valign(gtk::Align::Center)
            .active(data.selected.borrow().contains(&saved.key))
            .build();
        check.set_widget_name("selection");
        let key = saved.key.clone();
        let shared = data.clone();
        let weak = root.downgrade();
        let count = items.len();
        check.connect_toggled(move |c| {
            if c.is_active() {
                shared.selected.borrow_mut().insert(key.clone());
            } else {
                shared.selected.borrow_mut().remove(&key);
            }
            if let Some(root) = weak.upgrade() {
                object::<gtk::Label>(&root, "file_count").set_label(&format!(
                    "{} {} · {} {}",
                    count,
                    gettext("songs"),
                    shared.selected.borrow().len(),
                    gettext("selected")
                ));
            }
        });
        row.add_prefix(&check);
        row.set_activatable_widget(Some(&check));
        list.append(&row);
    }
    object::<gtk::Label>(root, "file_count").set_label(&format!(
        "{} {} · {} {}",
        items.len(),
        gettext("songs"),
        data.selected.borrow().len(),
        gettext("selected")
    ));
}
fn background(
    root: &gtk::Box,
    data: Rc<Data>,
    work: impl FnOnce(DownloadManager) -> anyhow::Result<String> + Send + 'static,
) {
    let manager = data.manager.clone();
    let (tx, rx) = async_channel::bounded(1);
    let weak = root.downgrade();
    std::thread::spawn(move || {
        let _ = tx.send_blocking(work(manager));
    });
    glib::MainContext::default().spawn_local(async move {
        if let (Ok(result), Some(root)) = (rx.recv().await, weak.upgrade()) {
            message(&root, &result.unwrap_or_else(|e| e.to_string()));
            *data.snapshot.borrow_mut() = data.manager.snapshot();
            render_files(&root, &data);
        }
    });
}
fn choose_scan(root: &gtk::Box, data: Rc<Data>) {
    let chooser = gtk::FileDialog::builder()
        .title(gettext("Scan folder for existing MP3 files"))
        .build();
    let weak = root.downgrade();
    let window = root.root().and_then(|r| r.downcast::<gtk::Window>().ok());
    chooser.select_folder(window.as_ref(), None::<&gio::Cancellable>, move |result| {
        if let (Ok(file), Some(root)) = (result, weak.upgrade()) {
            if let Some(path) = file.path() {
                let (tx, rx) = async_channel::bounded(1);
                let weak = root.downgrade();
                std::thread::spawn(move || {
                    let _ =
                        tx.send_blocking(capture_folder(&path, false, None).and_then(|target| {
                            let items = storage::scan(&target)?;
                            Ok((target, items))
                        }));
                });
                glib::MainContext::default().spawn_local(async move {
                    if let (Ok(result), Some(root)) = (rx.recv().await, weak.upgrade()) {
                        match result {
                            Err(e) => message(&root, &e.to_string()),
                            Ok((target, items)) => {
                                if items.is_empty() {
                                    message(&root, &gettext("No MP3 files found"));
                                    return;
                                }
                                let preview = adw::AlertDialog::builder()
                                    .heading(gettext("Import existing files?"))
                                    .body(format!(
                                        "{}\n{} {}\n{}",
                                        target.root.display(),
                                        items.len(),
                                        gettext("songs"),
                                        items
                                            .iter()
                                            .take(20)
                                            .map(|s| format!(
                                                "{}: {}",
                                                s.title,
                                                s.files
                                                    .iter()
                                                    .map(|f| f.relative.display().to_string())
                                                    .collect::<Vec<_>>()
                                                    .join(", ")
                                            ))
                                            .collect::<Vec<_>>()
                                            .join("\n")
                                    ))
                                    .build();
                                preview.add_response("cancel", &gettext("Cancel"));
                                preview.add_response("import", &gettext("Import"));
                                preview.set_default_response(Some("cancel"));
                                preview.set_close_response("cancel");
                                let weak = root.downgrade();
                                preview.connect_response(None, move |_, response| {
                                    if response == "import" {
                                        if let Some(root) = weak.upgrade() {
                                            let target = target.clone();
                                            let items = items.clone();
                                            background(&root, data.clone(), move |m| {
                                                m.import_files(target, items)?;
                                                Ok(gettext("Files imported"))
                                            });
                                        }
                                    }
                                });
                                preview.present(Some(&root));
                            }
                        }
                    }
                });
            }
        }
    });
}
fn delete_options(root: &gtk::Box, data: Rc<Data>) {
    let items: Vec<_> = data
        .snapshot
        .borrow()
        .saved
        .iter()
        .filter(|s| data.selected.borrow().contains(&s.key))
        .cloned()
        .collect();
    if items.is_empty() {
        message(root, &gettext("Select files first"));
        return;
    }
    let dialog = adw::AlertDialog::builder()
        .heading(gettext("Delete saved files"))
        .body(gettext(
            "Choose which files to include. The next step shows the exact files.",
        ))
        .build();
    let box_ = gtk::Box::new(gtk::Orientation::Vertical, 8);
    let audio = gtk::CheckButton::with_label(&gettext("MP3 audio"));
    audio.set_active(true);
    let lyrics = gtk::CheckButton::with_label(&gettext("Lyrics"));
    lyrics.set_active(true);
    box_.append(&audio);
    box_.append(&lyrics);
    dialog.set_extra_child(Some(&box_));
    dialog.add_response("cancel", &gettext("Cancel"));
    dialog.add_response("preview", &gettext("Preview files"));
    dialog.set_close_response("cancel");
    let weak = root.downgrade();
    dialog.connect_response(None, move |_, response| {
        if response == "preview" {
            if let Some(root) = weak.upgrade() {
                let items = items.clone();
                let audio = audio.is_active();
                let lyrics = lyrics.is_active();
                let (tx, rx) = async_channel::bounded(1);
                let weak = root.downgrade();
                std::thread::spawn(move || {
                    let _ = tx.send_blocking(storage::preview_delete(&items, audio, lyrics));
                });
                let shared = data.clone();
                glib::MainContext::default().spawn_local(async move {
                    if let (Ok(result), Some(root)) = (rx.recv().await, weak.upgrade()) {
                        match result {
                            Ok(preview) => confirm_delete(&root, shared, preview),
                            Err(e) => message(&root, &e.to_string()),
                        }
                    }
                });
            }
        }
    });
    dialog.present(Some(root));
}
fn confirm_delete(root: &gtk::Box, data: Rc<Data>, preview: DeletePreview) {
    if preview.items.is_empty() {
        message(root, &gettext("No matching files"));
        return;
    }
    let text = format!(
        "{} {} · {:.1} MB\n\n{}",
        preview.items.len(),
        gettext("files"),
        preview.total_bytes as f64 / 1e6,
        preview
            .items
            .iter()
            .map(|i| i.location.root.join(&i.file.relative).display().to_string())
            .collect::<Vec<_>>()
            .join("\n")
    );
    let dialog=adw::AlertDialog::builder().heading(gettext("Confirm file deletion")).body(gettext("Only these files will be removed. Other locations and the online playlist are retained. Moving to Trash may not free space on this device.")).build();
    let label = gtk::Label::new(Some(&text));
    label.set_wrap(true);
    label.set_xalign(0.);
    label.set_selectable(true);
    let scroll = gtk::ScrolledWindow::builder()
        .child(&label)
        .min_content_height(160)
        .max_content_height(250)
        .propagate_natural_height(true)
        .build();
    dialog.set_extra_child(Some(&scroll));
    dialog.add_response("cancel", &gettext("Cancel"));
    dialog.add_response("trash", &gettext("Move to Trash"));
    dialog.add_response("permanent", &gettext("Delete permanently"));
    dialog.set_response_appearance("permanent", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    let weak = root.downgrade();
    dialog.connect_response(None,move|_,response|{let mode=match response{"trash"=>DeleteMode::Trash,"permanent"=>DeleteMode::Permanent,_=>return};if let Some(root)=weak.upgrade(){let preview=preview.clone();background(&root,data.clone(),move|manager|{let report=manager.delete_files(&preview,mode)?;let errors:Vec<_>=report.results.iter().filter_map(|r|r.error.as_ref().map(|e|format!("{}: {e}",r.item.file.relative.display()))).collect();Ok(format!("{}: {} / {}{}",gettext("Files deleted"),report.results.len()-errors.len(),report.results.len(),if errors.is_empty(){String::new()}else{format!("\n{}\n{}",gettext("These files were retained. Trash failures require a separate permanent-delete confirmation."),errors.join("\n"))}))});}});
    dialog.present(Some(root));
}

fn task_button_label(task: &TaskRecord) -> String {
    gettext(if !task.stage.terminal() {
        "Cancel"
    } else if task.error.as_ref().is_some_and(|e| e.contains("GBK")) {
        "Retry as UTF-8"
    } else {
        "Retry"
    })
}
