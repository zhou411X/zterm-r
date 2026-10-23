use crate::{
    transfer::{self, Action, Endpoint, Entry},
    widgets::*,
};
use gtk::{gdk, prelude::*};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    path::{Path, PathBuf},
    rc::Rc,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc, Arc,
    },
    time::Duration,
};

#[derive(Clone)]
struct Task {
    id: u64,
    endpoint: Endpoint,
    action: Action,
    overwrite: bool,
    cancel: Arc<AtomicBool>,
}
enum Update {
    Started(u64),
    Progress(u64, u64, u64),
    Finished(u64, Result<(), String>),
}
struct Job {
    task: Task,
    state: String,
    row: gtk::Box,
    detail: gtk::Label,
    progress: gtk::ProgressBar,
    retry: gtk::Button,
    overwrite: gtk::Button,
    save_as: gtk::Button,
    cancel: gtk::Button,
}

pub struct FilePanel {
    pub widget: gtk::Box,
    window: glib::WeakRef<gtk::ApplicationWindow>,
    heading: gtk::Label,
    path: gtk::Entry,
    list: gtk::ListBox,
    info: gtk::Label,
    queue: gtk::Box,
    tools: gtk::Box,
    endpoint: RefCell<Option<Endpoint>>,
    locations: RefCell<HashMap<String, String>>,
    entries: RefCell<Vec<Entry>>,
    browsing: RefCell<Option<Arc<AtomicBool>>>,
    generation: Cell<u64>,
    sender: mpsc::Sender<Task>,
    jobs: RefCell<HashMap<u64, Job>>,
    next: Cell<u64>,
    closed: Cell<bool>,
    workers: Arc<AtomicUsize>,
}

struct Activity(Arc<AtomicUsize>);
impl Activity {
    fn new(count: Arc<AtomicUsize>) -> Self {
        count.fetch_add(1, Ordering::SeqCst);
        Self(count)
    }
}
impl Drop for Activity {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl FilePanel {
    pub fn new(window: &gtk::ApplicationWindow) -> Rc<Self> {
        let widget = gtk::Box::new(gtk::Orientation::Vertical, 10);
        widget.set_size_request(310, -1);
        widget.add_css_class("file-panel");
        margins(&widget, 14);
        let heading = label("文件", "section-heading");
        widget.append(&heading);
        let pathrow = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let up = icon("go-up-symbolic", "上一级目录");
        let path = gtk::Entry::new();
        path.set_hexpand(true);
        path.set_placeholder_text(Some("远程目录"));
        let refresh = icon("view-refresh-symbolic", "刷新文件列表");
        pathrow.append(&up);
        pathrow.append(&path);
        pathrow.append(&refresh);
        widget.append(&pathrow);
        let tools = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        let upload = icon("document-send-symbolic", "上传文件");
        let upload_dir = icon("folder-upload-symbolic", "上传目录");
        let download = icon("folder-download-symbolic", "下载所选文件或目录");
        let mkdir = icon("folder-new-symbolic", "新建目录");
        let rename = icon("document-edit-symbolic", "重命名");
        let delete = icon("user-trash-symbolic", "删除文件或空目录");
        for b in [&upload, &upload_dir, &download, &mkdir, &rename, &delete] {
            tools.append(b);
        }
        widget.append(&tools);
        let info = label("选择远程服务器后浏览文件", "dim-label");
        info.set_wrap(true);
        widget.append(&info);
        let list = gtk::ListBox::new();
        list.set_selection_mode(gtk::SelectionMode::Multiple);
        list.add_css_class("file-list");
        let scroll = gtk::ScrolledWindow::builder()
            .child(&list)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .build();
        widget.append(&scroll);
        widget.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        widget.append(&label("传输队列", "section-heading"));
        let queue = gtk::Box::new(gtk::Orientation::Vertical, 10);
        let queue_scroll = gtk::ScrolledWindow::builder()
            .child(&queue)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .min_content_height(100)
            .max_content_height(240)
            .propagate_natural_height(true)
            .build();
        widget.append(&queue_scroll);
        let (sender, receiver) = mpsc::channel::<Task>();
        let (events, updates) = mpsc::channel();
        let workers = Arc::new(AtomicUsize::new(0));
        let running = workers.clone();
        std::thread::spawn(move || {
            for task in receiver {
                let _activity = Activity::new(running.clone());
                if task.cancel.load(Ordering::Relaxed) {
                    let _ = events.send(Update::Finished(task.id, Err("已取消".into())));
                    continue;
                }
                let _ = events.send(Update::Started(task.id));
                let mut last = std::time::Instant::now() - Duration::from_secs(1);
                let result = transfer::run(
                    &task.endpoint,
                    &task.action,
                    task.overwrite,
                    task.cancel.clone(),
                    |done, total| {
                        if done == total || last.elapsed() >= Duration::from_millis(100) {
                            let _ = events.send(Update::Progress(task.id, done, total));
                            last = std::time::Instant::now();
                        }
                    },
                );
                let _ = events.send(Update::Finished(task.id, result));
            }
        });
        let this = Rc::new(Self {
            widget,
            window: window.downgrade(),
            heading,
            path,
            list,
            info,
            queue,
            tools,
            endpoint: RefCell::new(None),
            locations: RefCell::new(HashMap::new()),
            entries: RefCell::new(vec![]),
            browsing: RefCell::new(None),
            generation: Cell::new(0),
            sender,
            jobs: RefCell::new(HashMap::new()),
            next: Cell::new(1),
            closed: Cell::new(false),
            workers,
        });
        this.tools.set_sensitive(false);
        this.path.set_sensitive(false);
        let weak = Rc::downgrade(&this);
        glib::timeout_add_local(Duration::from_millis(80), move || {
            let Some(panel) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            if panel.closed.get() {
                return glib::ControlFlow::Break;
            }
            while let Ok(event) = updates.try_recv() {
                panel.updated(event);
            }
            glib::ControlFlow::Continue
        });
        let weak = Rc::downgrade(&this);
        refresh.connect_clicked(move |_| {
            if let Some(p) = weak.upgrade() {
                p.refresh();
            }
        });
        let weak = Rc::downgrade(&this);
        this.path.connect_activate(move |entry| {
            if let Some(p) = weak.upgrade() {
                p.load(entry.text().to_string());
            }
        });
        let weak = Rc::downgrade(&this);
        up.connect_clicked(move |_| {
            if let Some(p) = weak.upgrade() {
                let location = p.path.text();
                let parent = Path::new(location.as_str())
                    .parent()
                    .unwrap_or(Path::new("/"));
                p.load(parent.to_string_lossy().into_owned());
            }
        });
        let weak = Rc::downgrade(&this);
        this.list.connect_row_activated(move |_, row| {
            if let Some(p) = weak.upgrade() {
                let entry = p.entries.borrow().get(row.index() as usize).cloned();
                if let Some(entry) = entry {
                    if entry.is_dir {
                        let path = remote_join(p.path.text().as_str(), &entry.name);
                        p.load(path);
                    }
                }
            }
        });
        let weak = Rc::downgrade(&this);
        upload.connect_clicked(move |_| {
            if let Some(p) = weak.upgrade() {
                p.upload(false);
            }
        });
        let weak = Rc::downgrade(&this);
        upload_dir.connect_clicked(move |_| {
            if let Some(p) = weak.upgrade() {
                p.upload(true);
            }
        });
        let weak = Rc::downgrade(&this);
        download.connect_clicked(move |_| {
            if let Some(p) = weak.upgrade() {
                p.download();
            }
        });
        let weak = Rc::downgrade(&this);
        mkdir.connect_clicked(move |_| {
            if let Some(p) = weak.upgrade() {
                p.mkdir();
            }
        });
        let weak = Rc::downgrade(&this);
        rename.connect_clicked(move |_| {
            if let Some(p) = weak.upgrade() {
                p.rename();
            }
        });
        let weak = Rc::downgrade(&this);
        delete.connect_clicked(move |_| {
            if let Some(p) = weak.upgrade() {
                p.delete();
            }
        });
        let drop = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
        let weak = Rc::downgrade(&this);
        drop.connect_drop(move |_, value, _, _| {
            let (Some(p), Ok(files)) = (weak.upgrade(), value.get::<gdk::FileList>()) else {
                return false;
            };
            let Some(endpoint) = p.endpoint.borrow().clone() else {
                return false;
            };
            let location = p.path.text().to_string();
            if location.is_empty() {
                return false;
            }
            for file in files.files() {
                if let Some(source) = file.path() {
                    p.upload_path(endpoint.clone(), location.clone(), source);
                }
            }
            true
        });
        this.list.add_controller(drop);
        this
    }
    pub fn set_endpoint(self: &Rc<Self>, endpoint: Option<Endpoint>) {
        let same = match (self.endpoint.borrow().as_ref(), endpoint.as_ref()) {
            (Some(a), Some(b)) => {
                a.server_id == b.server_id
                    && a.argv == b.argv
                    && a.environment == b.environment
                    && a.label == b.label
            }
            (None, None) => true,
            _ => false,
        };
        if same {
            return;
        }
        if let Some(cancel) = self.browsing.borrow_mut().take() {
            cancel.store(true, Ordering::Relaxed);
        }
        self.generation.set(self.generation.get() + 1);
        *self.endpoint.borrow_mut() = endpoint.clone();
        self.entries.borrow_mut().clear();
        self.clear_list();
        self.tools.set_sensitive(endpoint.is_some());
        self.path.set_sensitive(endpoint.is_some());
        if let Some(endpoint) = endpoint {
            self.heading.set_text(&format!("文件 · {}", endpoint.label));
            let path = self
                .locations
                .borrow()
                .get(&endpoint.server_id)
                .cloned()
                .unwrap_or_else(|| ".".into());
            self.load(path);
        } else {
            self.heading.set_text("文件");
            self.path.set_text("");
            self.info.set_text("选择远程服务器后浏览文件");
        }
    }
    fn clear_list(&self) {
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
    }
    fn selected(&self) -> Vec<Entry> {
        self.list
            .selected_rows()
            .iter()
            .filter_map(|r| self.entries.borrow().get(r.index() as usize).cloned())
            .collect()
    }
    pub fn refresh(self: &Rc<Self>) {
        let path = self.path.text().to_string();
        if !path.is_empty() {
            self.load(path);
        }
    }
    fn load(self: &Rc<Self>, path: String) {
        let Some(endpoint) = self.endpoint.borrow().clone() else {
            return;
        };
        if let Some(cancel) = self.browsing.borrow_mut().take() {
            cancel.store(true, Ordering::Relaxed);
        }
        let cancel = Arc::new(AtomicBool::new(false));
        *self.browsing.borrow_mut() = Some(cancel.clone());
        let generation = self.generation.get() + 1;
        self.generation.set(generation);
        self.info.set_text("正在读取目录…");
        self.tools.set_sensitive(false);
        self.list.set_sensitive(false);
        let weak = Rc::downgrade(self);
        let id = endpoint.server_id.clone();
        background(
            move || transfer::browse(endpoint, path, cancel),
            move |result| {
                let Some(p) = weak.upgrade() else {
                    return;
                };
                if p.closed.get() || p.generation.get() != generation {
                    return;
                }
                p.tools.set_sensitive(true);
                p.list.set_sensitive(true);
                match result {
                    Ok((path, mut entries)) => {
                        entries.sort_by(|a, b| {
                            b.is_dir
                                .cmp(&a.is_dir)
                                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
                        });
                        p.path.set_text(&path);
                        p.locations.borrow_mut().insert(id, path);
                        p.clear_list();
                        for entry in &entries {
                            let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
                            margins(&row, 6);
                            row.append(&gtk::Image::from_icon_name(if entry.is_symlink {
                                "emblem-symbolic-link-symbolic"
                            } else if entry.is_dir {
                                "folder-symbolic"
                            } else {
                                "text-x-generic-symbolic"
                            }));
                            let name = label(&entry.name, "body");
                            name.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
                            name.set_hexpand(true);
                            name.set_max_width_chars(24);
                            name.set_tooltip_text(Some(&entry.name));
                            row.append(&name);
                            if !entry.is_dir {
                                row.append(&label(&size_text(entry.size), "dim-label"));
                            }
                            p.list.append(&row);
                        }
                        p.info
                            .set_text(&format!("{} 个项目 · 双击打开目录", entries.len()));
                        *p.entries.borrow_mut() = entries;
                    }
                    Err(error) => p.info.set_text(&format!("读取失败：{error}")),
                }
            },
        );
    }
    fn upload(self: &Rc<Self>, directory: bool) {
        let (Some(endpoint), Some(window)) =
            (self.endpoint.borrow().clone(), self.window.upgrade())
        else {
            return;
        };
        let location = self.path.text().to_string();
        if location.is_empty() {
            return;
        }
        let chooser = gtk::FileDialog::builder()
            .title(format!("上传到 {} · {}", endpoint.label, location))
            .accept_label("上传")
            .modal(false)
            .build();
        let weak = Rc::downgrade(self);
        let complete = move |result: Result<Vec<gio::File>, glib::Error>| {
            let Some(p) = weak.upgrade().filter(|p| !p.closed.get()) else {
                return;
            };
            match result {
                Ok(files) => {
                    let paths: Option<Vec<_>> = files.iter().map(|file| file.path()).collect();
                    let Some(paths) = paths else {
                        p.info.set_text("请选择本机上的文件或目录。");
                        return;
                    };
                    for path in paths {
                        p.upload_path(endpoint.clone(), location.clone(), path);
                    }
                }
                Err(error) => p.file_dialog_error(error),
            }
        };
        if directory {
            chooser.select_folder(Some(&window), None::<&gio::Cancellable>, move |result| {
                complete(result.map(|file| vec![file]));
            });
        } else {
            chooser.open_multiple(Some(&window), None::<&gio::Cancellable>, move |result| {
                complete(result.map(|files| {
                    files
                        .iter::<gio::File>()
                        .map(|file| file.expect("FileDialog returns files"))
                        .collect()
                }));
            });
        }
    }
    fn upload_path(self: &Rc<Self>, endpoint: Endpoint, location: String, source: PathBuf) {
        if let Some(name) = source.file_name().and_then(|s| s.to_str()) {
            let target = remote_join(&location, name);
            self.submit(endpoint, Action::Upload { source, target }, false);
        }
    }
    fn download(self: &Rc<Self>) {
        let selected = self.selected();
        if selected.is_empty() {
            return;
        }
        let (Some(endpoint), Some(window)) =
            (self.endpoint.borrow().clone(), self.window.upgrade())
        else {
            return;
        };
        let location = self.path.text().to_string();
        let chooser = gtk::FileDialog::builder()
            .title(format!(
                "下载 {} 的 {} 个项目到",
                endpoint.label,
                selected.len()
            ))
            .accept_label("下载")
            .modal(false)
            .build();
        let weak = Rc::downgrade(self);
        chooser.select_folder(Some(&window), None::<&gio::Cancellable>, move |result| {
            let Some(p) = weak.upgrade().filter(|p| !p.closed.get()) else {
                return;
            };
            match result {
                Ok(file) => {
                    let Some(folder) = file.path() else {
                        p.info.set_text("请选择本机上的下载目录。");
                        return;
                    };
                    for entry in &selected {
                        p.submit(
                            endpoint.clone(),
                            Action::Download {
                                source: remote_join(&location, &entry.name),
                                target: folder.join(&entry.name),
                            },
                            false,
                        );
                    }
                }
                Err(error) => p.file_dialog_error(error),
            }
        });
    }
    fn file_dialog_error(&self, error: glib::Error) {
        if !error.matches(gtk::DialogError::Dismissed)
            && !error.matches(gtk::DialogError::Cancelled)
        {
            self.info
                .set_text(&format!("无法打开文件选择窗口：{error}"));
        }
    }
    fn mkdir(self: &Rc<Self>) {
        let (Some(endpoint), Some(window)) =
            (self.endpoint.borrow().clone(), self.window.upgrade())
        else {
            return;
        };
        let location = self.path.text().to_string();
        let weak = Rc::downgrade(self);
        prompt(
            &window,
            "新建目录",
            &format!("{} · {location}", endpoint.label),
            "",
            move |name| {
                if let Some(p) = weak.upgrade() {
                    if p.valid_name(&name) {
                        p.submit(
                            endpoint.clone(),
                            Action::Mkdir {
                                path: remote_join(&location, &name),
                            },
                            false,
                        );
                    }
                }
            },
        );
    }
    fn rename(self: &Rc<Self>) {
        let selected = self.selected();
        if selected.len() != 1 {
            return;
        }
        let (Some(endpoint), Some(window)) =
            (self.endpoint.borrow().clone(), self.window.upgrade())
        else {
            return;
        };
        let location = self.path.text().to_string();
        let source = remote_join(&location, &selected[0].name);
        let weak = Rc::downgrade(self);
        prompt(
            &window,
            "重命名",
            &format!("{} · {location}", endpoint.label),
            &selected[0].name,
            move |name| {
                if let Some(p) = weak.upgrade() {
                    if p.valid_name(&name) {
                        p.submit(
                            endpoint.clone(),
                            Action::Rename {
                                source: source.clone(),
                                target: remote_join(&location, &name),
                            },
                            false,
                        );
                    }
                }
            },
        );
    }
    fn valid_name(&self, name: &str) -> bool {
        if name.is_empty()
            || name == "."
            || name == ".."
            || name.contains('/')
            || name.contains('\0')
        {
            if let Some(w) = self.window.upgrade() {
                message(&w, "无效名称", "名称不能为空，也不能包含 / 或空字符。");
            }
            false
        } else {
            true
        }
    }
    fn delete(self: &Rc<Self>) {
        let selected = self.selected();
        if selected.is_empty() {
            return;
        }
        let (Some(endpoint), Some(window)) =
            (self.endpoint.borrow().clone(), self.window.upgrade())
        else {
            return;
        };
        let location = self.path.text().to_string();
        let weak = Rc::downgrade(self);
        confirm(
            &window,
            "删除所选项目？",
            &format!(
                "{} · {location}\n将删除 {} 个文件或空目录，无法撤销。",
                endpoint.label,
                selected.len()
            ),
            move || {
                if let Some(p) = weak.upgrade() {
                    for entry in &selected {
                        p.submit(
                            endpoint.clone(),
                            Action::Delete {
                                path: remote_join(&location, &entry.name),
                            },
                            false,
                        );
                    }
                }
            },
        );
    }
    fn submit(self: &Rc<Self>, endpoint: Endpoint, action: Action, overwrite: bool) {
        if self.closed.get() {
            return;
        }
        let id = self.next.get();
        self.next.set(id + 1);
        let task = Task {
            id,
            endpoint,
            action,
            overwrite,
            cancel: Arc::new(AtomicBool::new(false)),
        };
        let row = gtk::Box::new(gtk::Orientation::Vertical, 5);
        row.add_css_class("transfer-job");
        let title = label(&task_title(&task), "body");
        title.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        title.set_max_width_chars(30);
        title.set_tooltip_text(Some(&task_title(&task)));
        row.append(&title);
        let detail = label("等待中", "dim-label");
        detail.set_wrap(true);
        row.append(&detail);
        let progress = gtk::ProgressBar::new();
        row.append(&progress);
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        let cancel = gtk::Button::with_label("取消");
        let retry = gtk::Button::with_label("重试");
        let overwrite_button = gtk::Button::with_label("覆盖");
        let save_as = gtk::Button::with_label("另存为");
        for button in [&cancel, &retry, &overwrite_button, &save_as] {
            actions.append(button);
        }
        retry.set_visible(false);
        overwrite_button.set_visible(false);
        save_as.set_visible(false);
        row.append(&actions);
        self.queue.append(&row);
        let weak = Rc::downgrade(self);
        cancel.connect_clicked(move |_| {
            if let Some(p) = weak.upgrade() {
                if let Some(job) = p.jobs.borrow_mut().get_mut(&id) {
                    job.task.cancel.store(true, Ordering::Relaxed);
                    let finished = job.state == "conflict";
                    job.state = if finished { "cancelled" } else { "cancelling" }.into();
                    job.detail.set_text(if finished {
                        "已取消"
                    } else {
                        "正在取消…"
                    });
                    job.cancel.set_visible(false);
                    job.retry.set_visible(finished);
                    job.overwrite.set_visible(false);
                    job.save_as.set_visible(false);
                }
            }
        });
        let weak = Rc::downgrade(self);
        retry.connect_clicked(move |_| {
            if let Some(p) = weak.upgrade() {
                p.retry(id, false, None);
            }
        });
        let weak = Rc::downgrade(self);
        overwrite_button.connect_clicked(move |_| {
            if let Some(p) = weak.upgrade() {
                if let Some(window) = p.window.upgrade() {
                    let weak = Rc::downgrade(&p);
                    confirm(
                        &window,
                        "覆盖同名文件？",
                        "目标位置的同名文件将被替换。",
                        move || {
                            if let Some(p) = weak.upgrade() {
                                p.retry(id, true, None);
                            }
                        },
                    );
                }
            }
        });
        let weak = Rc::downgrade(self);
        save_as.connect_clicked(move |_| {
            if let Some(p) = weak.upgrade() {
                let task = p.jobs.borrow().get(&id).map(|j| j.task.clone());
                if let (Some(task), Some(window)) = (task, p.window.upgrade()) {
                    let target = task_target(&task.action);
                    let weak = Rc::downgrade(&p);
                    prompt(
                        &window,
                        "另存为",
                        "请输入新的完整目标路径",
                        &target,
                        move |target| {
                            if !target.is_empty() {
                                if let Some(p) = weak.upgrade() {
                                    p.retry(id, false, Some(target));
                                }
                            }
                        },
                    );
                }
            }
        });
        self.jobs.borrow_mut().insert(
            id,
            Job {
                task: task.clone(),
                state: "queued".into(),
                row,
                detail,
                progress,
                retry,
                overwrite: overwrite_button,
                save_as,
                cancel,
            },
        );
        if self.sender.send(task).is_err() {
            self.updated(Update::Finished(id, Err("传输工作线程不可用".into())));
        }
    }
    fn retry(self: &Rc<Self>, id: u64, overwrite: bool, target: Option<String>) {
        let task = {
            let mut jobs = self.jobs.borrow_mut();
            let Some(job) = jobs.get_mut(&id) else {
                return;
            };
            if job.state == "running" || job.state == "queued" {
                return;
            }
            job.state = "superseded".into();
            job.row.set_visible(false);
            job.task.clone()
        };
        let mut action = task.action;
        if let Some(target) = target {
            match &mut action {
                Action::Upload { target: out, .. } => *out = target,
                Action::Download { target: out, .. } => *out = PathBuf::from(target),
                Action::Rename { target: out, .. } => *out = target,
                _ => {}
            }
        }
        self.submit(task.endpoint, action, overwrite);
    }
    fn updated(self: &Rc<Self>, event: Update) {
        let mut refresh = false;
        {
            let mut jobs = self.jobs.borrow_mut();
            match event {
                Update::Started(id) => {
                    if let Some(job) = jobs.get_mut(&id) {
                        if !job.task.cancel.load(Ordering::Relaxed) {
                            job.state = "running".into();
                            job.detail.set_text("连接与传输中…");
                        }
                    }
                }
                Update::Progress(id, done, total) => {
                    if let Some(job) = jobs.get_mut(&id) {
                        if job.state == "running" {
                            job.progress.set_fraction(if total > 0 {
                                done as f64 / total as f64
                            } else {
                                0.0
                            });
                            job.detail.set_text(&format!(
                                "{} / {}",
                                size_text(done),
                                size_text(total)
                            ));
                        }
                    }
                }
                Update::Finished(id, result) => {
                    if let Some(job) = jobs.get_mut(&id) {
                        job.cancel.set_visible(false);
                        if result.is_err() && job.task.cancel.load(Ordering::Relaxed) {
                            job.state = "cancelled".into();
                            job.detail.set_text("已取消");
                            job.retry.set_visible(true);
                        } else {
                            match result {
                                Ok(()) => {
                                    job.state = "done".into();
                                    job.progress.set_fraction(1.0);
                                    job.detail.set_text("已完成");
                                    refresh = self
                                        .endpoint
                                        .borrow()
                                        .as_ref()
                                        .map(|e| e.server_id == job.task.endpoint.server_id)
                                        .unwrap_or(false);
                                }
                                Err(error) => {
                                    let conflict = error.starts_with("CONFLICT:");
                                    job.state = if conflict { "conflict" } else { "failed" }.into();
                                    job.detail.set_text(&error);
                                    job.retry.set_visible(!conflict);
                                    job.overwrite.set_visible(
                                        conflict
                                            && matches!(
                                                job.task.action,
                                                Action::Upload { .. } | Action::Download { .. }
                                            ),
                                    );
                                    job.save_as.set_visible(conflict);
                                    job.cancel.set_visible(conflict);
                                }
                            }
                        }
                    }
                }
            }
        }
        if refresh {
            self.refresh();
        }
    }
    pub fn active_count(&self, server: Option<&str>) -> usize {
        self.jobs
            .borrow()
            .values()
            .filter(|job| {
                matches!(
                    job.state.as_str(),
                    "queued" | "running" | "conflict" | "cancelling"
                ) && server
                    .map(|s| s == job.task.endpoint.server_id)
                    .unwrap_or(true)
            })
            .count()
    }
    pub fn shutdown(&self) {
        self.closed.set(true);
        if let Some(cancel) = self.browsing.borrow_mut().take() {
            cancel.store(true, Ordering::Relaxed);
        }
        for job in self.jobs.borrow().values() {
            job.task.cancel.store(true, Ordering::Relaxed);
        }
    }
    pub fn is_busy(&self) -> bool {
        self.workers.load(Ordering::SeqCst) != 0
    }
}
fn remote_join(directory: &str, name: &str) -> String {
    format!("{}/{}", directory.trim_end_matches('/'), name)
}
fn size_text(size: u64) -> String {
    if size < 1024 {
        format!("{size} B")
    } else if size < 1024 * 1024 {
        format!("{:.1} KB", size as f64 / 1024.0)
    } else if size < 1024 * 1024 * 1024 {
        format!("{:.1} MB", size as f64 / 1048576.0)
    } else {
        format!("{:.1} GB", size as f64 / 1073741824.0)
    }
}
fn task_target(action: &Action) -> String {
    match action {
        Action::Upload { target, .. } | Action::Rename { target, .. } => target.clone(),
        Action::Download { target, .. } => target.to_string_lossy().into_owned(),
        Action::Mkdir { path } | Action::Delete { path } => path.clone(),
    }
}
fn task_title(task: &Task) -> String {
    let verb = match task.action {
        Action::Upload { .. } => "上传",
        Action::Download { .. } => "下载",
        Action::Mkdir { .. } => "新建目录",
        Action::Rename { .. } => "重命名",
        Action::Delete { .. } => "删除",
    };
    format!(
        "{verb} · {} · {}",
        task.endpoint.label,
        task_target(&task.action)
    )
}

#[cfg(test)]
pub fn verify_file_choosers(window: &gtk::ApplicationWindow) {
    use std::time::Instant;
    let context = glib::MainContext::default();
    let settle = || {
        for _ in 0..30 {
            while context.pending() {
                context.iteration(false);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    };
    let wait = |condition: &dyn Fn() -> bool| {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !condition() {
            assert!(Instant::now() < deadline, "file chooser/transfer timed out");
            settle();
        }
    };
    let chooser = || {
        gtk::Window::list_toplevels()
            .into_iter()
            .filter(|widget| widget.is_visible())
            .find_map(|widget| widget.downcast::<gtk::FileChooserDialog>().ok())
            .expect("file chooser must remain open until the user responds")
    };
    let remote = tempfile::tempdir().unwrap();
    let local = tempfile::tempdir().unwrap();
    let endpoint = Endpoint {
        server_id: "chooser-test".into(),
        label: "SFTP test".into(),
        argv: vec![
            "/usr/lib/openssh/sftp-server".into(),
            "-d".into(),
            remote.path().to_string_lossy().into_owned(),
        ],
        environment: vec![],
    };
    let panel = FilePanel::new(window);
    *panel.endpoint.borrow_mut() = Some(endpoint.clone());
    panel.path.set_text(remote.path().to_str().unwrap());
    panel.tools.set_sensitive(true);
    let upload = panel
        .tools
        .first_child()
        .unwrap()
        .downcast::<gtk::Button>()
        .unwrap();
    upload.emit_clicked();
    settle();
    chooser().response(gtk::ResponseType::Cancel);
    settle();
    assert!(
        panel.jobs.borrow().is_empty(),
        "cancel must not enqueue a transfer"
    );
    let source = local.path().join("upload.txt");
    std::fs::write(&source, b"upload chooser roundtrip").unwrap();
    upload.emit_clicked();
    settle();
    let selected = chooser();
    selected.set_file(&gio::File::for_path(&source)).unwrap();
    settle();
    panel.path.set_text("/must-not-rebind-an-open-chooser");
    *panel.endpoint.borrow_mut() = None;
    selected.response(gtk::ResponseType::Accept);
    wait(&|| !panel.jobs.borrow().is_empty() && panel.active_count(None) == 0);
    assert_eq!(
        std::fs::read(remote.path().join("upload.txt")).unwrap(),
        b"upload chooser roundtrip"
    );
    *panel.endpoint.borrow_mut() = Some(endpoint);
    panel.path.set_text(remote.path().to_str().unwrap());
    let directory = local.path().join("folder");
    std::fs::create_dir(&directory).unwrap();
    std::fs::write(directory.join("child.txt"), b"directory upload").unwrap();
    panel.upload(true);
    settle();
    let selected = chooser();
    selected.set_file(&gio::File::for_path(&directory)).unwrap();
    settle();
    selected.response(gtk::ResponseType::Accept);
    wait(&|| panel.jobs.borrow().len() == 2 && panel.active_count(None) == 0);
    assert_eq!(
        std::fs::read(remote.path().join("folder/child.txt")).unwrap(),
        b"directory upload"
    );
    panel.load(remote.path().to_string_lossy().into_owned());
    wait(&|| {
        panel
            .entries
            .borrow()
            .iter()
            .any(|entry| entry.name == "upload.txt")
    });
    let index = panel
        .entries
        .borrow()
        .iter()
        .position(|entry| entry.name == "upload.txt")
        .unwrap();
    panel
        .list
        .select_row(panel.list.row_at_index(index as i32).as_ref());
    panel.download();
    settle();
    let selected = chooser();
    let destination = tempfile::tempdir().unwrap();
    selected
        .set_file(&gio::File::for_path(destination.path()))
        .unwrap();
    settle();
    selected.response(gtk::ResponseType::Accept);
    wait(&|| panel.jobs.borrow().len() == 3 && panel.active_count(None) == 0);
    assert_eq!(
        std::fs::read(destination.path().join("upload.txt")).unwrap(),
        b"upload chooser roundtrip"
    );
    panel.shutdown();
    wait(&|| !panel.is_busy());
}
