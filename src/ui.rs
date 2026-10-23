use crate::{
    appearance::{self, Background},
    credentials,
    files::FilePanel,
    model::{self, Appearance, Config, Server},
    widgets::*,
};
use gtk::{gdk, prelude::*};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    rc::Rc,
};
use vte::prelude::*;

struct Session {
    terminal: vte::Terminal,
    page: gtk::Box,
    title: gtk::Label,
    background: Background,
    server: String,
    pid: Cell<Option<i32>>,
    pending: Cell<bool>,
    closed: Cell<bool>,
    status: RefCell<String>,
}

impl Session {
    fn active(&self) -> bool {
        self.pending.get() || self.pid.get().is_some()
    }
    fn dispose(&self) {
        self.closed.set(true);
        if let Some(pid) = self.pid.take() {
            // VTE owns and reaps the child. SIGHUP closes its interactive session.
            unsafe {
                libc::kill(pid, libc::SIGHUP);
            }
        }
    }
}

struct Group {
    notebook: gtk::Notebook,
    sessions: RefCell<Vec<Rc<Session>>>,
}

pub struct Workbench {
    pub window: gtk::ApplicationWindow,
    config: RefCell<Config>,
    appearance: RefCell<Appearance>,
    config_valid: Cell<bool>,
    revision: Cell<u64>,
    current: RefCell<String>,
    groups: RefCell<HashMap<String, Rc<Group>>>,
    stack: gtk::Stack,
    list: gtk::ListBox,
    rows: RefCell<HashMap<String, gtk::ListBoxRow>>,
    search: gtk::SearchEntry,
    status: gtk::Label,
    quota: gtk::Label,
    quota_pending: Cell<bool>,
    subtitle: gtk::Label,
    file_panel: Rc<FilePanel>,
    file_reveal: gtk::Revealer,
    provider: gtk::CssProvider,
    background: Background,
    closing: Cell<bool>,
    dialogs: RefCell<HashMap<String, gtk::Window>>,
}

impl Workbench {
    pub fn new(app: &gtk::Application) -> Rc<Self> {
        let loaded = Config::load();
        let load_error = loaded.as_ref().err().cloned();
        let config = loaded.unwrap_or_else(|_| Config {
            aliases: vec![],
            servers: Default::default(),
        });
        let loaded_appearance = Appearance::load();
        let appearance_error = loaded_appearance.as_ref().err().cloned();
        let settings = loaded_appearance.unwrap_or_default();
        let window = gtk::ApplicationWindow::builder()
            .application(app)
            .title("zterm-r")
            .default_width(1240)
            .default_height(800)
            .build();
        window.add_css_class("zterm");
        let header = gtk::HeaderBar::new();
        let brand = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        brand.append(&gtk::Image::from_icon_name("utilities-terminal-symbolic"));
        let titles = gtk::Box::new(gtk::Orientation::Vertical, 1);
        titles.append(&label("zterm-r", "title"));
        let subtitle = label("终端与远程工作空间", "dim-label");
        titles.append(&subtitle);
        brand.append(&titles);
        header.set_title_widget(Some(&brand));
        let sidebar_toggle = icon("sidebar-show-symbolic", "显示或隐藏服务器栏");
        header.pack_start(&sidebar_toggle);
        let new_button = icon("list-add-symbolic", "新建会话 · Ctrl+Shift+T");
        header.pack_start(&new_button);
        let settings_button = icon("preferences-system-symbolic", "外观设置");
        header.pack_end(&settings_button);
        let file_button = icon("folder-symbolic", "显示或隐藏文件面板");
        header.pack_end(&file_button);
        window.set_titlebar(Some(&header));

        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let paned = gtk::Paned::new(gtk::Orientation::Horizontal);
        paned.set_vexpand(true);
        paned.set_position(244);
        paned.set_shrink_start_child(false);
        paned.set_resize_start_child(false);
        let sidebar = gtk::Box::new(gtk::Orientation::Vertical, 14);
        sidebar.add_css_class("sidebar");
        sidebar.set_size_request(200, -1);
        margins(&sidebar, 14);
        let search = gtk::SearchEntry::new();
        search.set_placeholder_text(Some("搜索服务器或 IP"));
        sidebar.append(&search);
        let list = gtk::ListBox::new();
        list.add_css_class("navigation-sidebar");
        list.set_selection_mode(gtk::SelectionMode::Single);
        let scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&list)
            .build();
        sidebar.append(&scroll);
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let add = icon("list-add-symbolic", "添加服务器");
        let edit = icon("document-edit-symbolic", "编辑服务器");
        let copy = icon("edit-copy-symbolic", "复制服务器");
        let remove = icon("user-trash-symbolic", "移除服务器");
        actions.append(&add);
        actions.append(&edit);
        actions.append(&copy);
        actions.append(&remove);
        sidebar.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        sidebar.append(&actions);
        paned.set_start_child(Some(&sidebar));
        let stack = gtk::Stack::new();
        stack.set_hexpand(true);
        stack.set_vexpand(true);
        let welcome = gtk::Box::new(gtk::Orientation::Vertical, 16);
        welcome.set_valign(gtk::Align::Center);
        welcome.set_halign(gtk::Align::Center);
        let welcome_icon = gtk::Image::from_icon_name("utilities-terminal-symbolic");
        welcome_icon.set_pixel_size(64);
        welcome.append(&welcome_icon);
        welcome.append(&label("从这里开始工作", "welcome-title"));
        welcome.append(&label(
            "本地终端与远程服务器，保持在同一个工作空间。",
            "dim-label",
        ));
        let local_button = gtk::Button::with_label("打开本地终端");
        local_button.add_css_class("suggested-action");
        local_button.set_halign(gtk::Align::Center);
        welcome.append(&local_button);
        welcome.append(&label(
            "Ctrl+Shift+T 新建会话   ·   Alt+1…9 切换标签",
            "dim-label",
        ));
        stack.add_named(&welcome, Some("welcome"));
        let file_panel = FilePanel::new(&window);
        let file_reveal = gtk::Revealer::builder()
            .transition_type(gtk::RevealerTransitionType::SlideLeft)
            .reveal_child(false)
            .child(&file_panel.widget)
            .build();
        let file_pane = gtk::Paned::new(gtk::Orientation::Horizontal);
        file_pane.set_start_child(Some(&stack));
        file_pane.set_end_child(Some(&file_reveal));
        file_pane.set_resize_end_child(false);
        file_pane.set_shrink_end_child(false);
        paned.set_end_child(Some(&file_pane));
        root.append(&paned);
        let statusbar = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        statusbar.add_css_class("statusbar");
        let status = label("就绪", "dim-label");
        status.set_hexpand(true);
        let reconnect = icon("view-refresh-symbolic", "重新连接当前会话");
        statusbar.append(&reconnect);
        statusbar.append(&status);
        let quota = label("Codex · 点击刷新额度", "dim-label");
        quota.set_ellipsize(gtk::pango::EllipsizeMode::End);
        quota.set_max_width_chars(55);
        statusbar.append(&quota);
        let quota_refresh = icon("emblem-synchronizing-symbolic", "刷新 Codex 订阅额度");
        statusbar.append(&quota_refresh);
        let hint = label("UTF-8", "dim-label");
        statusbar.append(&hint);
        root.append(&statusbar);
        let background = Background::new(&root, false);
        window.set_child(Some(&background.widget()));
        let provider = gtk::CssProvider::new();
        gtk::style_context_add_provider_for_display(
            &WidgetExt::display(&window),
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
        let this = Rc::new(Self {
            window,
            config: RefCell::new(config),
            appearance: RefCell::new(settings),
            config_valid: Cell::new(load_error.is_none()),
            revision: Cell::new(0),
            current: RefCell::new(String::new()),
            groups: RefCell::new(HashMap::new()),
            stack,
            list,
            rows: RefCell::new(HashMap::new()),
            search,
            status,
            quota,
            quota_pending: Cell::new(false),
            subtitle,
            file_panel,
            file_reveal,
            provider,
            background,
            closing: Cell::new(false),
            dialogs: RefCell::new(HashMap::new()),
        });
        this.apply_appearance(this.appearance.borrow().clone());
        this.rebuild_servers();
        sidebar_toggle.connect_clicked(move |_| {
            sidebar.set_visible(!sidebar.is_visible());
        });
        let weak = Rc::downgrade(&this);
        new_button.connect_clicked(move |_| {
            if let Some(w) = weak.upgrade() {
                w.new_session();
            }
        });
        let weak = Rc::downgrade(&this);
        local_button.connect_clicked(move |_| {
            if let Some(w) = weak.upgrade() {
                w.select_server("");
            }
        });
        let weak = Rc::downgrade(&this);
        add.connect_clicked(move |_| {
            if let Some(w) = weak.upgrade() {
                w.server_dialog(None);
            }
        });
        let weak = Rc::downgrade(&this);
        edit.connect_clicked(move |_| {
            if let Some(w) = weak.upgrade() {
                let id = w.current.borrow().clone();
                if !id.is_empty() {
                    w.server_dialog(Some(id));
                }
            }
        });
        let weak = Rc::downgrade(&this);
        copy.connect_clicked(move |_| {
            if let Some(w) = weak.upgrade() {
                let id = w.current.borrow().clone();
                w.copy_server(&id);
            }
        });
        let weak = Rc::downgrade(&this);
        remove.connect_clicked(move |_| {
            if let Some(w) = weak.upgrade() {
                let id = w.current.borrow().clone();
                w.remove_server(&id);
            }
        });
        let weak = Rc::downgrade(&this);
        reconnect.connect_clicked(move |_| {
            if let Some(w) = weak.upgrade() {
                w.reconnect();
            }
        });
        let weak = Rc::downgrade(&this);
        quota_refresh.connect_clicked(move |_| {
            if let Some(w) = weak.upgrade() {
                w.refresh_quota();
            }
        });
        let weak = Rc::downgrade(&this);
        file_button.connect_clicked(move |_| {
            if let Some(w) = weak.upgrade() {
                w.file_reveal
                    .set_reveal_child(!w.file_reveal.reveals_child());
                if w.file_reveal.reveals_child() {
                    w.update_files();
                }
            }
        });
        let weak = Rc::downgrade(&this);
        settings_button.connect_clicked(move |_| {
            if let Some(w) = weak.upgrade() {
                let preview = Rc::downgrade(&w);
                let save = Rc::downgrade(&w);
                appearance::dialog(
                    &w.window,
                    w.appearance.borrow().clone(),
                    move |settings| {
                        if let Some(w) = preview.upgrade() {
                            w.apply_appearance(settings);
                        }
                    },
                    move |settings| {
                        if let Some(w) = save.upgrade() {
                            *w.appearance.borrow_mut() = settings.clone();
                            w.apply_appearance(settings);
                        }
                    },
                );
            }
        });
        let weak = Rc::downgrade(&this);
        this.search.connect_search_changed(move |_| {
            if let Some(w) = weak.upgrade() {
                w.filter_servers();
            }
        });
        let weak = Rc::downgrade(&this);
        this.list.connect_row_activated(move |_, row| {
            if let Some(w) = weak.upgrade() {
                let id = w
                    .rows
                    .borrow()
                    .iter()
                    .find(|(_, r)| *r == row)
                    .map(|(id, _)| id.clone());
                if let Some(id) = id {
                    w.select_server(&id);
                }
            }
        });
        this.list.set_activate_on_single_click(true);
        this.keyboard();
        let weak = Rc::downgrade(&this);
        this.window.connect_close_request(move |_| {
            let Some(w) = weak.upgrade() else {
                return glib::Propagation::Proceed;
            };
            if w.closing.get() {
                return glib::Propagation::Proceed;
            }
            let count = w.active_count();
            let transfers = w.file_panel.active_count(None);
            if count + transfers == 0 {
                w.shutdown();
                return glib::Propagation::Proceed;
            }
            let weak = Rc::downgrade(&w);
            confirm(
                &w.window,
                "关闭 zTerm？",
                &format!(
                    "还有 {count} 个活动会话、{transfers} 个文件任务。关闭将结束会话并取消任务。"
                ),
                move || {
                    if let Some(w) = weak.upgrade() {
                        w.shutdown();
                        w.closing.set(true);
                        w.window.close();
                    }
                },
            );
            glib::Propagation::Stop
        });
        let keep_alive = RefCell::new(Some(this.clone()));
        this.window.connect_destroy(move |_| {
            keep_alive.borrow_mut().take();
        });
        this.window.present();
        if let Some(error) = load_error {
            message(
                &this.window,
                "读取服务器配置失败",
                &format!("{error}\n修复配置后重新启动；本次运行不会覆盖原配置。"),
            );
        }
        if let Some(error) = appearance_error {
            message(&this.window, "读取外观配置失败", &error);
        }
        this
    }

    fn name(&self, id: &str) -> String {
        if id.is_empty() {
            "本地终端".into()
        } else {
            self.config
                .borrow()
                .servers
                .get(id)
                .map(|p| p.name.clone())
                .unwrap_or_else(|| id.to_string())
        }
    }
    fn active_count(&self) -> usize {
        self.groups
            .borrow()
            .values()
            .map(|g| g.sessions.borrow().iter().filter(|s| s.active()).count())
            .sum()
    }
    fn shutdown(&self) {
        self.file_panel.shutdown();
        if let Some(app) = self.window.application() {
            let mut hold = Some(app.hold());
            let panel = self.file_panel.clone();
            glib::timeout_add_local(std::time::Duration::from_millis(50), move || {
                if panel.is_busy() {
                    glib::ControlFlow::Continue
                } else {
                    hold.take();
                    glib::ControlFlow::Break
                }
            });
        }
        for g in self.groups.borrow().values() {
            for s in g.sessions.borrow().iter() {
                s.dispose();
            }
        }
    }
    fn current_session(&self) -> Option<Rc<Session>> {
        let groups = self.groups.borrow();
        let g = groups.get(self.current.borrow().as_str())?;
        let page = g.notebook.nth_page(g.notebook.current_page())?;
        let result = g.sessions.borrow().iter().find(|s| s.page == page).cloned();
        result
    }
    fn render_status(&self) {
        let id = self.current.borrow().clone();
        let name = self.name(&id);
        let state = self
            .current_session()
            .map(|s| s.status.borrow().clone())
            .unwrap_or_else(|| "尚未连接".into());
        self.status.set_text(&format!("{name}  ·  {state}"));
        self.subtitle.set_text(&name);
        self.window.set_title(Some(&format!("{name} — zterm-r")));
    }
    fn apply_appearance(&self, settings: Appearance) {
        if settings.background_opacity < 1.0 || !settings.background_image.is_empty() {
            self.window.add_css_class("zterm-backdrop");
        } else {
            self.window.remove_css_class("zterm-backdrop");
        }
        self.provider.load_from_data(&appearance::css(&settings));
        self.background.update(&settings);
        for group in self.groups.borrow().values() {
            for s in group.sessions.borrow().iter() {
                appearance::apply_terminal(&s.terminal, &settings);
                s.background.update(&settings);
            }
        }
    }
    fn filter_servers(&self) {
        let text = self.search.text().to_lowercase();
        let config = self.config.borrow();
        for (id, row) in self.rows.borrow().iter() {
            let haystack = config
                .servers
                .get(id)
                .map(|s| format!("{} {} {}", s.name, s.host, s.username))
                .unwrap_or_else(|| self.name(id));
            row.set_visible(text.is_empty() || haystack.to_lowercase().contains(&text));
        }
    }
    fn rebuild_servers(self: &Rc<Self>) {
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        self.rows.borrow_mut().clear();
        let mut ids = vec![String::new()];
        ids.extend(self.config.borrow().aliases.clone());
        for id in ids {
            let row = gtk::ListBoxRow::new();
            let content = gtk::Box::new(gtk::Orientation::Horizontal, 10);
            margins(&content, 10);
            content.append(&gtk::Image::from_icon_name(if id.is_empty() {
                "utilities-terminal-symbolic"
            } else {
                "network-server-symbolic"
            }));
            let texts = gtk::Box::new(gtk::Orientation::Vertical, 3);
            let name = label(&self.name(&id), "server-name");
            name.set_ellipsize(gtk::pango::EllipsizeMode::End);
            name.set_max_width_chars(20);
            name.set_hexpand(true);
            texts.append(&name);
            let detail = self
                .config
                .borrow()
                .servers
                .get(&id)
                .map(|s| format!("{}@{}:{}", s.username, s.host, s.port))
                .unwrap_or_else(|| {
                    if id.is_empty() {
                        "当前计算机".into()
                    } else {
                        "SSH 配置".into()
                    }
                });
            let detail_label = label(&detail, "dim-label");
            detail_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
            detail_label.set_max_width_chars(23);
            texts.append(&detail_label);
            content.append(&texts);
            row.set_child(Some(&content));
            row.set_tooltip_text(Some(&detail));
            self.list.append(&row);
            self.rows.borrow_mut().insert(id.clone(), row.clone());
            if !id.is_empty() {
                let gesture = gtk::GestureClick::new();
                gesture.set_button(3);
                let weak = Rc::downgrade(self);
                let key = id.clone();
                let target = row.downgrade();
                gesture.connect_pressed(move |_, _, x, y| {
                    if let (Some(w), Some(target)) = (weak.upgrade(), target.upgrade()) {
                        w.server_menu(&target, &key, x, y);
                    }
                });
                row.add_controller(gesture);
                let drag = gtk::DragSource::new();
                drag.set_actions(gdk::DragAction::MOVE);
                let key = id.clone();
                drag.connect_prepare(move |_, _, _| {
                    Some(gdk::ContentProvider::for_value(&key.to_value()))
                });
                row.add_controller(drag);
                let drop = gtk::DropTarget::new(String::static_type(), gdk::DragAction::MOVE);
                let weak = Rc::downgrade(self);
                let key = id.clone();
                drop.connect_drop(move |_, value, _, _| {
                    let Ok(source) = value.get::<String>() else {
                        return false;
                    };
                    if let Some(w) = weak.upgrade() {
                        return w.reorder(&source, &key);
                    }
                    false
                });
                row.add_controller(drop);
            }
        }
        if let Some(row) = self.rows.borrow().get(self.current.borrow().as_str()) {
            self.list.select_row(Some(row));
        }
        self.filter_servers();
    }
    fn commit_config(&self, config: Config) -> Result<(), String> {
        if !self.config_valid.get() {
            return Err("原配置读取失败，请先修复配置文件。".into());
        }
        config.save()?;
        *self.config.borrow_mut() = config;
        self.revision.set(self.revision.get() + 1);
        Ok(())
    }
    fn reorder(self: &Rc<Self>, source: &str, target: &str) -> bool {
        let mut config = self.config.borrow().clone();
        let Some(from) = config.aliases.iter().position(|s| s == source) else {
            return false;
        };
        if !config.aliases.iter().any(|s| s == target) || source == target {
            return false;
        }
        let value = config.aliases.remove(from);
        let to = config.aliases.iter().position(|s| s == target).unwrap();
        config.aliases.insert(to, value);
        match self.commit_config(config) {
            Ok(()) => {
                self.rebuild_servers();
                true
            }
            Err(e) => {
                message(&self.window, "排序保存失败", &e);
                false
            }
        }
    }
    fn server_menu(self: &Rc<Self>, row: &gtk::ListBoxRow, id: &str, x: f64, y: f64) {
        let pop = gtk::Popover::new();
        pop.set_parent(row);
        pop.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        let menu = gtk::Box::new(gtk::Orientation::Vertical, 4);
        margins(&menu, 6);
        for (label, action) in [
            ("编辑服务器", 0),
            ("移除服务器", 1),
            ("复制服务器", 2),
            ("浏览服务器文件", 3),
        ] {
            let button = gtk::Button::with_label(label);
            button.add_css_class("flat");
            menu.append(&button);
            let weak = Rc::downgrade(self);
            let key = id.to_string();
            let pop = pop.clone();
            button.connect_clicked(move |_| {
                pop.popdown();
                if let Some(w) = weak.upgrade() {
                    match action {
                        0 => w.server_dialog(Some(key.clone())),
                        1 => w.remove_server(&key),
                        2 => w.copy_server(&key),
                        _ => {
                            w.select_server(&key);
                            w.file_reveal.set_reveal_child(true);
                            w.update_files();
                        }
                    }
                }
            });
        }
        pop.set_child(Some(&menu));
        pop.connect_closed(|pop| pop.unparent());
        pop.popup();
    }
    fn update_files(&self) {
        let id = self.current.borrow().clone();
        if id.is_empty() {
            self.file_panel.set_endpoint(None);
            return;
        }
        match crate::transfer::endpoint_for(&id, self.config.borrow().servers.get(&id)) {
            Ok(endpoint) => self.file_panel.set_endpoint(Some(endpoint)),
            Err(e) => message(&self.window, "文件连接失败", &e),
        }
    }
    fn select_server(self: &Rc<Self>, id: &str) {
        *self.current.borrow_mut() = id.to_string();
        self.quota.set_text("Codex · 点击刷新额度");
        if let Some(row) = self.rows.borrow().get(id) {
            self.list.select_row(Some(row));
        }
        let needs_session = self
            .groups
            .borrow()
            .get(id)
            .map_or(true, |g| g.notebook.n_pages() == 0);
        if needs_session {
            self.new_session();
        } else {
            self.stack.set_visible_child_name(&format!("server:{id}"));
        }
        if self.file_reveal.reveals_child() {
            self.update_files();
        }
        self.render_status();
        if let Some(s) = self.current_session() {
            s.terminal.grab_focus();
        }
    }
    fn new_session(self: &Rc<Self>) {
        let id = self.current.borrow().clone();
        let group = if let Some(g) = self.groups.borrow().get(&id) {
            g.clone()
        } else {
            let notebook = gtk::Notebook::new();
            notebook.set_scrollable(true);
            notebook.set_show_border(false);
            notebook.add_css_class("sessions");
            let weak = Rc::downgrade(self);
            notebook.connect_switch_page(move |_, _, _| {
                let weak = weak.clone();
                glib::idle_add_local_once(move || {
                    if let Some(w) = weak.upgrade() {
                        w.render_status();
                    }
                });
            });
            let weak = Rc::downgrade(self);
            let key = id.clone();
            notebook.connect_page_reordered(move |_, _, _| {
                if let Some(w) = weak.upgrade() {
                    w.renumber(&key);
                }
            });
            self.stack
                .add_named(&notebook, Some(&format!("server:{id}")));
            Rc::new(Group {
                notebook,
                sessions: RefCell::new(vec![]),
            })
        };
        self.groups.borrow_mut().insert(id.clone(), group.clone());
        let terminal = vte::Terminal::new();
        terminal.set_hexpand(true);
        terminal.set_vexpand(true);
        terminal.set_scrollback_lines(20000);
        terminal.set_scroll_on_output(false);
        terminal.set_scroll_on_keystroke(true);
        terminal.set_mouse_autohide(true);
        appearance::apply_terminal(&terminal, &self.appearance.borrow());
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .child(&terminal)
            .build();
        let backdrop = Background::new(&scroller, true);
        backdrop.update(&self.appearance.borrow());
        let page = gtk::Box::new(gtk::Orientation::Vertical, 0);
        page.append(&backdrop.widget());
        let tab = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        tab.set_hexpand(false);
        tab.set_size_request(180, -1);
        let title = label("连接中…", "session-title");
        title.set_hexpand(false);
        title.set_xalign(0.0);
        title.set_ellipsize(gtk::pango::EllipsizeMode::End);
        title.set_width_chars(16);
        title.set_max_width_chars(22);
        tab.append(&title);
        let close = icon("window-close-symbolic", "关闭会话");
        tab.append(&close);
        let session = Rc::new(Session {
            terminal,
            page,
            title,
            background: backdrop,
            server: id.clone(),
            pid: Cell::new(None),
            pending: Cell::new(false),
            closed: Cell::new(false),
            status: RefCell::new("连接中".into()),
        });
        let index = group.notebook.append_page(&session.page, Some(&tab));
        group.notebook.page(&session.page).set_tab_expand(false);
        group.notebook.page(&session.page).set_tab_fill(false);
        group.notebook.set_tab_reorderable(&session.page, true);
        group.sessions.borrow_mut().push(session.clone());
        group.notebook.set_current_page(Some(index));
        self.stack.set_visible_child_name(&format!("server:{id}"));
        let weak = Rc::downgrade(self);
        let s = Rc::downgrade(&session);
        close.connect_clicked(move |_| {
            if let (Some(w), Some(s)) = (weak.upgrade(), s.upgrade()) {
                w.close_session(s);
            }
        });
        let weak = Rc::downgrade(self);
        let s = Rc::downgrade(&session);
        session.terminal.connect_child_exited(move |_, _| {
            if let Some(s) = s.upgrade() {
                s.pid.set(None);
                s.pending.set(false);
                *s.status.borrow_mut() = "已结束".into();
                if let Some(w) = weak.upgrade() {
                    w.renumber(&s.server);
                    w.render_status();
                }
            }
        });
        let weak = Rc::downgrade(self);
        let s = Rc::downgrade(&session);
        session.terminal.connect_window_title_notify(move |_| {
            if let (Some(w), Some(s)) = (weak.upgrade(), s.upgrade()) {
                w.renumber(&s.server);
            }
        });
        self.terminal_menu(&session);
        self.spawn(&session);
        self.renumber(&id);
        session.terminal.grab_focus();
    }
    fn spawn(self: &Rc<Self>, session: &Rc<Session>) {
        if session.pending.get() {
            return;
        }
        let profile = self.config.borrow().servers.get(&session.server).cloned();
        let argv = if cfg!(test)
            && std::env::var_os("ZTERM_SMOKE_TEST").is_some()
            && session.server.is_empty()
        {
            Ok(vec![
                "/bin/sh".into(),
                "-c".into(),
                "printf 'zterm-r PTY ready\\n'; read -r reply; test \"$reply\" = 'zterm-roundtrip'"
                    .into(),
            ])
        } else if session.server.is_empty() {
            Ok(vec![
                std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".into()),
                "-l".into(),
            ])
        } else {
            model::ssh_command(&session.server, profile.as_ref())
        };
        let argv = match argv {
            Ok(a) => a,
            Err(e) => {
                *session.status.borrow_mut() = e.clone();
                message(&self.window, "无法启动会话", &e);
                return;
            }
        };
        let mut env: HashMap<String, String> = std::env::vars().collect();
        // SSH askpass settings belong only to this connection, never a local shell.
        for key in [
            "ZTERM_ASKPASS",
            "ZTERM_SERVER_ID",
            "ZTERM_SSH_HOST",
            "ZTERM_SSH_USER",
            "ZTERM_SSH_BACKGROUND",
            "SSH_ASKPASS_REQUIRE",
        ] {
            env.remove(key);
        }
        env.insert("TERM".into(), "xterm-256color".into());
        env.insert("COLORTERM".into(), "truecolor".into());
        if !session.server.is_empty() {
            env.extend(credentials::ssh_environment(
                &session.server,
                profile.as_ref(),
                false,
            ));
        }
        let env: Vec<String> = env.into_iter().map(|(k, v)| format!("{k}={v}")).collect();
        session.pending.set(true);
        *session.status.borrow_mut() = "正在启动".into();
        let weak = Rc::downgrade(self);
        let s = Rc::downgrade(session);
        let working_directory = glib::home_dir();
        session.terminal.spawn_async(
            vte::PtyFlags::DEFAULT,
            working_directory.to_str(),
            &argv.iter().map(String::as_str).collect::<Vec<_>>(),
            &env.iter().map(String::as_str).collect::<Vec<_>>(),
            glib::SpawnFlags::SEARCH_PATH,
            || {},
            15000,
            None::<&gio::Cancellable>,
            move |result| {
                if let Some(s) = s.upgrade() {
                    s.pending.set(false);
                    match result {
                        Ok(pid) => {
                            if s.closed.get() {
                                unsafe {
                                    libc::kill(pid.0, libc::SIGHUP);
                                }
                            } else {
                                s.pid.set(Some(pid.0));
                                *s.status.borrow_mut() = "运行中".into();
                            }
                        }
                        Err(e) => {
                            *s.status.borrow_mut() = format!("启动失败：{e}");
                        }
                    }
                    if let Some(w) = weak.upgrade() {
                        w.renumber(&s.server);
                        w.render_status();
                    }
                }
            },
        );
        self.render_status();
    }
    fn renumber(&self, id: &str) {
        if let Some(g) = self.groups.borrow().get(id) {
            for s in g.sessions.borrow().iter() {
                let n = g.notebook.page_num(&s.page).unwrap_or(0) + 1;
                let raw = s
                    .terminal
                    .property::<Option<String>>("window-title")
                    .filter(|title| !title.trim().is_empty())
                    .unwrap_or_else(|| {
                        if id.is_empty() {
                            "Shell".into()
                        } else {
                            "SSH".into()
                        }
                    });
                let activity = if s.active() { raw } else { "已结束".into() };
                let codex = s.active()
                    && activity
                        .split(|c: char| !(c.is_alphanumeric() || "_./-".contains(c)))
                        .any(|word| word.eq_ignore_ascii_case("codex"));
                s.title.set_text(&format!(
                    "{n}  {}{activity}",
                    if codex { "[CODEX] " } else { "" }
                ));
                s.title.set_tooltip_text(Some(&activity));
            }
        }
    }
    fn close_session(self: &Rc<Self>, session: Rc<Session>) {
        if session.active() {
            let weak = Rc::downgrade(self);
            confirm(
                &self.window,
                "关闭此会话？",
                "此操作会结束当前终端会话。",
                move || {
                    if let Some(w) = weak.upgrade() {
                        w.remove_session(&session);
                    }
                },
            );
        } else {
            self.remove_session(&session);
        }
    }
    fn remove_session(&self, s: &Rc<Session>) {
        s.dispose();
        if let Some(g) = self.groups.borrow().get(&s.server) {
            if let Some(index) = g.notebook.page_num(&s.page) {
                g.notebook.remove_page(Some(index));
            }
            g.sessions.borrow_mut().retain(|x| !Rc::ptr_eq(x, s));
        }
        self.renumber(&s.server);
        self.render_status();
    }
    fn reconnect(self: &Rc<Self>) {
        let Some(s) = self.current_session() else {
            self.new_session();
            return;
        };
        if s.active() {
            let weak = Rc::downgrade(self);
            confirm(
                &self.window,
                "重新连接？",
                "当前会话将被关闭，并使用最新连接设置新建会话。",
                move || {
                    if let Some(w) = weak.upgrade() {
                        w.remove_session(&s);
                        w.new_session();
                    }
                },
            );
        } else {
            self.remove_session(&s);
            self.new_session();
        }
    }
    fn terminal_menu(self: &Rc<Self>, s: &Rc<Session>) {
        let click = gtk::GestureClick::new();
        click.set_button(3);
        let terminal = s.terminal.downgrade();
        click.connect_pressed(move |_, _, x, y| {
            let Some(terminal) = terminal.upgrade() else {
                return;
            };
            let pop = gtk::Popover::new();
            pop.set_parent(&terminal);
            pop.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
            let menu = gtk::Box::new(gtk::Orientation::Vertical, 4);
            for (title, action) in [("复制", 0), ("粘贴", 1), ("全选", 2)] {
                let b = gtk::Button::with_label(title);
                b.add_css_class("flat");
                if action == 0 {
                    b.set_sensitive(terminal.has_selection());
                }
                let t = terminal.clone();
                let p = pop.clone();
                b.connect_clicked(move |_| {
                    match action {
                        0 => t.copy_clipboard_format(vte::Format::Text),
                        1 => t.paste_clipboard(),
                        _ => t.select_all(),
                    };
                    p.popdown();
                });
                menu.append(&b);
            }
            pop.set_child(Some(&menu));
            pop.connect_closed(|p| p.unparent());
            pop.popup();
        });
        s.terminal.add_controller(click);
    }
    fn keyboard(self: &Rc<Self>) {
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(self);
        keys.connect_key_pressed(move |_, key, _, mods| {
            let Some(w) = weak.upgrade() else {
                return glib::Propagation::Proceed;
            };
            let ctrl = mods.contains(gdk::ModifierType::CONTROL_MASK);
            let shift = mods.contains(gdk::ModifierType::SHIFT_MASK);
            let alt = mods.contains(gdk::ModifierType::ALT_MASK);
            let key = key.to_lower();
            if ctrl && shift && key == gdk::Key::t {
                w.new_session();
                return glib::Propagation::Stop;
            }
            let Some(s) = w.current_session() else {
                return glib::Propagation::Proceed;
            };
            if ctrl && shift {
                match key {
                    gdk::Key::c => s.terminal.copy_clipboard_format(vte::Format::Text),
                    gdk::Key::v => s.terminal.paste_clipboard(),
                    gdk::Key::w => w.close_session(s.clone()),
                    _ => return glib::Propagation::Proceed,
                }
                return glib::Propagation::Stop;
            }
            if alt {
                if let Some(n) = key.to_unicode().and_then(|c| c.to_digit(10)) {
                    if (1..=9).contains(&n) {
                        if let Some(g) = w.groups.borrow().get(&s.server) {
                            if n <= g.notebook.n_pages() {
                                g.notebook.set_current_page(Some(n - 1));
                            }
                        }
                        return glib::Propagation::Stop;
                    }
                }
            }
            if ctrl
                && [
                    gdk::Key::plus,
                    gdk::Key::equal,
                    gdk::Key::minus,
                    gdk::Key::_0,
                ]
                .contains(&key)
            {
                let scale = if key == gdk::Key::_0 {
                    1.0
                } else {
                    (s.terminal.font_scale() + if key == gdk::Key::minus { -0.1 } else { 0.1 })
                        .clamp(0.5, 2.5)
                };
                s.terminal.set_font_scale(scale);
                return glib::Propagation::Stop;
            }
            if ctrl && (key == gdk::Key::Page_Up || key == gdk::Key::Page_Down) {
                if let Some(g) = w.groups.borrow().get(&s.server) {
                    let count = g.notebook.n_pages();
                    if count > 0 {
                        let n = g.notebook.current_page().unwrap_or(0);
                        g.notebook
                            .set_current_page(Some(if key == gdk::Key::Page_Down {
                                (n + 1) % count
                            } else {
                                (n + count - 1) % count
                            }));
                    }
                }
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        self.window.add_controller(keys);
    }
    fn server_dialog(self: &Rc<Self>, id: Option<String>) {
        let key = id.clone().unwrap_or_else(|| "new-server".into());
        if let Some(dialog) = self.dialogs.borrow().get(&key) {
            dialog.present();
            return;
        }
        if let Some(id) = id.as_ref() {
            let profile = self.config.borrow().servers.get(id).cloned();
            if let Some(profile) = profile {
                self.editor(Some(id.clone()), profile);
            } else {
                let weak = Rc::downgrade(self);
                let id = id.clone();
                let lookup = id.clone();
                background(
                    move || model::legacy_server_profile(&lookup),
                    move |result| {
                        if let Some(w) = weak.upgrade() {
                            match result {
                                Ok(profile) => w.editor(Some(id), profile),
                                Err(e) => message(&w.window, "读取 SSH 配置失败", &e),
                            }
                        }
                    },
                );
            }
        } else {
            self.editor(
                None,
                Server {
                    name: String::new(),
                    host: String::new(),
                    username: std::env::var("USER").unwrap_or_default(),
                    port: 22,
                    password_saved: false,
                    credential_id: None,
                },
            );
        }
    }
    fn editor(self: &Rc<Self>, id: Option<String>, profile: Server) {
        let key = id.clone().unwrap_or_else(|| "new-server".into());
        if let Some(dialog) = self.dialogs.borrow().get(&key) {
            dialog.present();
            return;
        }
        let dialog = gtk::Window::builder()
            .title(if id.is_some() {
                "编辑服务器"
            } else {
                "添加服务器"
            })
            .transient_for(&self.window)
            .default_width(480)
            .resizable(false)
            .build();
        dialog.add_css_class("zterm-ui");
        let area = gtk::Box::new(gtk::Orientation::Vertical, 14);
        margins(&area, 24);
        dialog.set_child(Some(&area));
        area.append(&label("服务器连接", "welcome-title"));
        area.append(&label("使用系统 SSH 配置、密钥与钥匙环。", "dim-label"));
        let grid = gtk::Grid::builder()
            .column_spacing(18)
            .row_spacing(12)
            .build();
        area.append(&grid);
        let name = gtk::Entry::new();
        name.set_text(&profile.name);
        name.set_placeholder_text(Some("可选，默认使用 IP"));
        let host = gtk::Entry::new();
        host.set_text(&profile.host);
        host.set_placeholder_text(Some("IPv4 或 IPv6 地址"));
        let user = gtk::Entry::new();
        user.set_text(&profile.username);
        let port = gtk::SpinButton::with_range(1.0, 65535.0, 1.0);
        port.set_value(f64::from(profile.port));
        let password = gtk::PasswordEntry::new();
        password.set_show_peek_icon(true);
        password.set_hexpand(true);
        for (index, (title, entry)) in [
            ("显示名称", name.clone().upcast::<gtk::Widget>()),
            ("IP 地址", host.clone().upcast()),
            ("用户名", user.clone().upcast()),
            ("端口", port.clone().upcast()),
            ("密码", password.clone().upcast()),
        ]
        .into_iter()
        .enumerate()
        {
            grid.attach(&label(title, "body"), 0, index as i32, 1, 1);
            entry.set_hexpand(true);
            grid.attach(&entry, 1, index as i32, 1, 1);
        }
        let clear = gtk::CheckButton::with_label("清除已保存的密码");
        clear.set_visible(profile.password_saved);
        area.append(&clear);
        area.append(&label(
            if profile.password_saved {
                "密码留空保留原密码；新密码保存到系统钥匙环。"
            } else {
                "密码留空使用密钥登录；密码不会写入配置文件。"
            },
            "dim-label",
        ));
        let error = label("", "error");
        error.set_wrap(true);
        area.append(&error);
        let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        buttons.set_halign(gtk::Align::End);
        let cancel = gtk::Button::with_label("取消");
        let save = gtk::Button::with_label("保存");
        save.add_css_class("suggested-action");
        buttons.append(&cancel);
        buttons.append(&save);
        area.append(&buttons);
        let d = dialog.clone();
        cancel.connect_clicked(move |_| d.close());
        let busy = Rc::new(Cell::new(false));
        let b = busy.clone();
        dialog.connect_close_request(move |_| {
            if b.get() {
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
        let weak = Rc::downgrade(self);
        let d = dialog.clone();
        let original = profile.clone();
        let busy_save = busy.clone();
        save.connect_clicked(move |button| {
            let Some(w) = weak.upgrade() else {
                return;
            };
            if id.is_some() && w.file_panel.active_count(id.as_deref()) != 0 {
                error.set_text("请等待文件任务完成或取消任务后，再编辑服务器。");
                return;
            }
            let mut candidate = Server {
                name: name.text().trim().to_string(),
                host: host.text().trim().to_string(),
                username: user.text().trim().to_string(),
                port: port.value_as_int() as u16,
                password_saved: original.password_saved,
                credential_id: original.credential_id.clone(),
            };
            if candidate.name.is_empty() {
                candidate.name = candidate.host.clone();
            }
            if let Err(e) = candidate.validate() {
                error.set_text(&e);
                return;
            }
            let secret = password.text().to_string();
            let clear_secret = clear.is_active();
            if clear_secret && !secret.is_empty() {
                error.set_text("请在设置新密码和清除密码之间选择一项。");
                return;
            }
            let server_id = id
                .clone()
                .unwrap_or_else(|| format!("server-{}", uuid::Uuid::new_v4().simple()));
            let revision = w.revision.get();
            let editing_existing = id.is_some();
            let old_credential = if original.password_saved {
                Some(
                    original
                        .credential_id
                        .clone()
                        .unwrap_or_else(|| server_id.clone()),
                )
            } else {
                None
            };
            let mut config = w.config.borrow().clone();
            if !config.aliases.contains(&server_id) {
                config.aliases.push(server_id.clone());
            }
            if clear_secret {
                candidate.password_saved = false;
                candidate.credential_id = None;
            }
            let staged = if secret.is_empty() {
                None
            } else {
                Some(format!("credential-{}", uuid::Uuid::new_v4().simple()))
            };
            if let Some(ref credential) = staged {
                candidate.password_saved = true;
                candidate.credential_id = Some(credential.clone());
            }
            let title = candidate.name.clone();
            config.servers.insert(server_id.clone(), candidate);
            button.set_sensitive(false);
            cancel.set_sensitive(false);
            busy_save.set(true);
            error.set_text("正在保存…");
            let weak = Rc::downgrade(&w);
            let staged_worker = staged.clone();
            let button = button.clone();
            let cancel = cancel.clone();
            let busy = busy_save.clone();
            let error = error.clone();
            let dialog = d.clone();
            background(
                move || {
                    if let Some(credential) = staged_worker {
                        credentials::save_password(&credential, &title, &secret)
                    } else {
                        Ok(())
                    }
                },
                move |result| {
                    busy.set(false);
                    button.set_sensitive(true);
                    cancel.set_sensitive(true);
                    let Some(w) = weak.upgrade() else {
                        return;
                    };
                    let result = result.and_then(|_| {
                        if w.revision.get() != revision {
                            Err("服务器列表已更改，请重新保存。".into())
                        } else if editing_existing
                            && w.file_panel.active_count(Some(&server_id)) != 0
                        {
                            Err("文件任务已启动，请等待任务结束后重新保存。".into())
                        } else {
                            w.commit_config(config)
                        }
                    });
                    match result {
                        Ok(()) => {
                            dialog.close();
                            w.rebuild_servers();
                            w.render_status();
                            if w.file_reveal.reveals_child() {
                                w.update_files();
                            }
                            if let Some(old) = old_credential {
                                if clear_secret || staged.is_some() {
                                    background(move || credentials::delete_password(&old), |_| {});
                                }
                            }
                        }
                        Err(e) => {
                            error.set_text(&e);
                            if let Some(staged) = staged {
                                background(move || credentials::delete_password(&staged), |_| {});
                            }
                        }
                    }
                },
            );
        });
        let weak = Rc::downgrade(self);
        let close_key = key.clone();
        dialog.connect_destroy(move |_| {
            if let Some(w) = weak.upgrade() {
                w.dialogs.borrow_mut().remove(&close_key);
            }
        });
        self.dialogs.borrow_mut().insert(key, dialog.clone());
        dialog.present();
    }
    fn remove_server(self: &Rc<Self>, id: &str) {
        if id.is_empty() {
            return;
        }
        if self.file_panel.active_count(Some(id)) > 0 {
            message(
                &self.window,
                "服务器仍有文件任务",
                "请先完成或取消文件任务。",
            );
            return;
        }
        let weak = Rc::downgrade(self);
        let id = id.to_string();
        confirm(
            &self.window,
            "移除服务器？",
            &format!("将移除 {} 的配置并结束该服务器的所有会话。", self.name(&id)),
            move || {
                let Some(w) = weak.upgrade() else {
                    return;
                };
                if w.file_panel.active_count(Some(&id)) > 0 {
                    message(
                        &w.window,
                        "无法移除",
                        "该服务器有新的文件任务，请先完成或取消。",
                    );
                    return;
                }
                let mut config = w.config.borrow().clone();
                let profile = config.servers.remove(&id);
                config.aliases.retain(|a| a != &id);
                if let Err(e) = w.commit_config(config) {
                    message(&w.window, "移除失败", &e);
                    return;
                }
                if let Some(group) = w.groups.borrow_mut().remove(&id) {
                    for session in group.sessions.borrow().iter() {
                        session.dispose();
                    }
                    w.stack.remove(&group.notebook);
                }
                let is_current = *w.current.borrow() == id;
                if is_current {
                    *w.current.borrow_mut() = String::new();
                    w.stack.set_visible_child_name("welcome");
                    w.file_panel.set_endpoint(None);
                }
                if let Some(p) = profile {
                    if p.password_saved {
                        let credential = p.credential_id.unwrap_or_else(|| id.clone());
                        let weak = Rc::downgrade(&w);
                        background(
                            move || credentials::delete_password(&credential),
                            move |r| {
                                if let (Err(e), Some(w)) = (r, weak.upgrade()) {
                                    message(&w.window, "钥匙环清理失败", &e);
                                }
                            },
                        );
                    }
                }
                w.rebuild_servers();
                w.render_status();
            },
        );
    }
    fn copy_server(self: &Rc<Self>, id: &str) {
        if id.is_empty() {
            return;
        }
        let id = id.to_string();
        let profile = self.config.borrow().servers.get(&id).cloned();
        let weak = Rc::downgrade(self);
        let source = id.clone();
        background(
            move || {
                let p = match profile {
                    Some(p) => p,
                    None => model::legacy_server_profile(&source)?,
                };
                let password = if p.password_saved {
                    credentials::lookup_password(
                        p.credential_id.as_deref().unwrap_or(&source),
                        false,
                    )?
                } else {
                    None
                };
                Ok::<_, String>((p, password))
            },
            move |result| {
                let Some(w) = weak.upgrade() else {
                    return;
                };
                let (mut profile, password) = match result {
                    Ok(value) => value,
                    Err(e) => {
                        message(&w.window, "复制失败", &e);
                        return;
                    }
                };
                let base = profile.name.clone();
                let mut name = format!("{base} 副本");
                let mut n = 2;
                while w.config.borrow().servers.values().any(|s| s.name == name) {
                    name = format!("{base} 副本 {n}");
                    n += 1;
                }
                profile.name = name.chars().take(80).collect();
                profile.password_saved = password.is_some();
                let new_id = format!("server-{}", uuid::Uuid::new_v4().simple());
                profile.credential_id = password.as_ref().map(|_| new_id.clone());
                let mut config = w.config.borrow().clone();
                config.aliases.push(new_id.clone());
                config.servers.insert(new_id.clone(), profile.clone());
                let revision = w.revision.get();
                let weak = Rc::downgrade(&w);
                let credential = new_id.clone();
                background(
                    move || {
                        if let Some(password) = password {
                            credentials::save_password(&credential, &profile.name, &password)
                        } else {
                            Ok(())
                        }
                    },
                    move |result| {
                        if let Some(w) = weak.upgrade() {
                            let result = result.and_then(|_| {
                                if w.revision.get() != revision {
                                    Err("服务器列表已更改，请重试复制。".into())
                                } else {
                                    w.commit_config(config)
                                }
                            });
                            match result {
                                Ok(()) => w.rebuild_servers(),
                                Err(e) => {
                                    message(&w.window, "复制失败", &e);
                                    background(
                                        move || credentials::delete_password(&new_id),
                                        |_| {},
                                    );
                                }
                            }
                        }
                    },
                );
            },
        );
    }
    fn refresh_quota(self: &Rc<Self>) {
        if self.quota_pending.replace(true) {
            return;
        }
        let id = self.current.borrow().clone();
        let profile = self.config.borrow().servers.get(&id).cloned();
        let source = id.clone();
        let weak = Rc::downgrade(self);
        self.quota.set_text("Codex · 查询中…");
        background(
            move || crate::quota::fetch(&source, profile.as_ref()),
            move |result| {
                if let Some(w) = weak.upgrade() {
                    w.quota_pending.set(false);
                    if *w.current.borrow() == id {
                        let text = result.unwrap_or_else(|e| format!("Codex · {e}"));
                        w.quota.set_text(&text);
                        w.quota.set_tooltip_text(Some(&text));
                    }
                }
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn wait_for(mut condition: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        let context = glib::MainContext::default();
        while !condition() {
            assert!(Instant::now() < deadline, "GTK/PTY operation timed out");
            while context.pending() {
                context.iteration(false);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn settle_layout() {
        let context = glib::MainContext::default();
        for _ in 0..35 {
            while context.pending() {
                context.iteration(false);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn find_button(widget: &gtk::Widget, tooltip: &str) -> Option<gtk::Button> {
        if widget.tooltip_text().as_deref() == Some(tooltip) {
            return widget.clone().downcast().ok();
        }
        let mut child = widget.first_child();
        while let Some(widget) = child {
            if let Some(button) = find_button(&widget, tooltip) {
                return Some(button);
            }
            child = widget.next_sibling();
        }
        None
    }

    fn verify_sidebar_layout(w: &Workbench) {
        let toggle = find_button(w.window.upcast_ref(), "显示或隐藏服务器栏").unwrap();
        let content = w.stack.parent().unwrap();
        let split = content.parent().unwrap().downcast::<gtk::Paned>().unwrap();
        for width in [244, 320] {
            split.set_position(width);
            settle_layout();
            let expanded = content.compute_bounds(&split).unwrap();
            toggle.emit_clicked();
            settle_layout();
            let collapsed = content.compute_bounds(&split).unwrap();
            assert!(
                collapsed.x() <= 1.0,
                "collapsed sidebar leaves {} pixels unused",
                collapsed.x()
            );
            assert!((collapsed.width() - split.width() as f32).abs() <= 1.0);
            toggle.emit_clicked();
            settle_layout();
            let restored = content.compute_bounds(&split).unwrap();
            assert!(
                (restored.x() - expanded.x()).abs() <= 1.0,
                "sidebar width must survive collapse"
            );
        }
        split.set_position(244);
    }

    fn verify_new_button(w: &Workbench) {
        settle_layout();
        let header = w.window.titlebar().unwrap();
        let toggle = find_button(&header, "显示或隐藏服务器栏").unwrap();
        let new_button = find_button(&header, "新建会话 · Ctrl+Shift+T").unwrap();
        assert_eq!(
            toggle.next_sibling().as_ref(),
            Some(new_button.upcast_ref())
        );
        let plus = new_button.compute_bounds(&header).unwrap();
        let toggle_bounds = toggle.compute_bounds(&header).unwrap();
        assert!(plus.x() >= toggle_bounds.x() + toggle_bounds.width());
        let add = find_button(w.window.upcast_ref(), "添加服务器").unwrap();
        let edit = find_button(w.window.upcast_ref(), "编辑服务器").unwrap();
        assert_eq!(add.next_sibling().as_ref(), Some(edit.upcast_ref()));
        let sidebar = add.parent().unwrap().parent().unwrap();
        assert_eq!(sidebar.first_child().as_ref(), Some(w.search.upcast_ref()));
        let groups = w.groups.borrow();
        let group = groups.get("").unwrap();
        assert!(find_button(group.notebook.upcast_ref(), "新建会话 · Ctrl+Shift+T").is_none());
        assert!(group.notebook.action_widget(gtk::PackType::End).is_none());
        assert!(group.notebook.action_widget(gtk::PackType::Start).is_none());
    }

    #[test]
    fn gtk_workspace_real_pty_and_config_roundtrip() {
        // Run this test under Xvfb with --test-threads=1. No real server or login shell.
        let root = tempfile::tempdir().unwrap();
        let previous = std::env::var_os("XDG_CONFIG_HOME");
        std::env::set_var("XDG_CONFIG_HOME", root.path());
        std::env::set_var("ZTERM_SMOKE_TEST", "1");
        let mut config = Config::default();
        for (id, name, host) in [
            ("dev", "开发服务器", "192.0.2.10"),
            ("build", "构建服务器", "192.0.2.11"),
        ] {
            config.aliases.push(id.into());
            config.servers.insert(
                id.into(),
                Server {
                    name: name.into(),
                    host: host.into(),
                    username: "developer".into(),
                    port: 22,
                    password_saved: false,
                    credential_id: None,
                },
            );
        }
        config.save().unwrap();
        glib::set_prgname(Some("zterm-r-test"));
        gtk::init().expect("GUI test requires a display: use xvfb-run");
        let app = gtk::Application::builder()
            .application_id("local.zterm.r.Test")
            .flags(gio::ApplicationFlags::NON_UNIQUE)
            .build();
        app.register(None::<&gio::Cancellable>).unwrap();
        let w = Workbench::new(&app);
        assert_eq!(w.rows.borrow().len(), 3);
        w.select_server("");
        let first = w.current_session().unwrap();
        wait_for(|| first.pid.get().is_some());
        let exit_code = Rc::new(Cell::new(None));
        let observed = exit_code.clone();
        first
            .terminal
            .connect_child_exited(move |_, code| observed.set(Some(code)));
        first.terminal.feed_child(b"zterm-roundtrip\n");
        wait_for(|| !first.active());
        assert_eq!(exit_code.get(), Some(0));
        assert_eq!(first.status.borrow().as_str(), "已结束");
        verify_new_button(&w);
        let add = find_button(w.window.upcast_ref(), "新建会话 · Ctrl+Shift+T").unwrap();
        add.emit_clicked();
        let second = w.current_session().unwrap();
        wait_for(|| second.pid.get().is_some());
        assert_eq!(w.groups.borrow().get("").unwrap().notebook.n_pages(), 2);
        verify_new_button(&w);
        w.groups
            .borrow()
            .get("")
            .unwrap()
            .notebook
            .reorder_child(&second.page, Some(0));
        assert!(second.title.text().starts_with('1'));
        verify_new_button(&w);
        w.remove_session(&first);
        verify_new_button(&w);
        assert_eq!(w.groups.borrow().get("").unwrap().notebook.n_pages(), 1);
        assert!(w.reorder("build", "dev"));
        assert_eq!(Config::load().unwrap().aliases, vec!["build", "dev"]);
        w.search.set_text("192.0.2.10");
        w.filter_servers();
        assert!(w.rows.borrow()["dev"].is_visible());
        assert!(!w.rows.borrow()["build"].is_visible());
        w.search.set_text("");
        w.filter_servers();
        // Exercise each palette and the existing file format on actual GTK/VTE objects.
        for (name, theme) in model::themes() {
            let settings = Appearance {
                theme: name,
                foreground: theme.foreground,
                background: theme.background,
                cursor: theme.cursor,
                ..Appearance::default()
            };
            w.apply_appearance(settings);
        }
        let theme = model::themes().remove("papercolor-light").unwrap();
        let settings = Appearance {
            theme: "papercolor-light".into(),
            foreground: theme.foreground,
            background: theme.background,
            cursor: theme.cursor,
            ..Appearance::default()
        };
        w.apply_appearance(settings);
        verify_sidebar_layout(&w);
        appearance::verify_dialog_layout(&w.window);
        crate::files::verify_file_choosers(&w.window);
        let context = glib::MainContext::default();
        for _ in 0..30 {
            while context.pending() {
                context.iteration(false);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        if let Some(path) = std::env::var_os("ZTERM_TEST_SCREENSHOT") {
            let paintable = gtk::WidgetPaintable::new(Some(&w.window));
            let snapshot = gtk::Snapshot::new();
            paintable.snapshot(
                &snapshot,
                f64::from(w.window.width()),
                f64::from(w.window.height()),
            );
            let node = snapshot.to_node().expect("window snapshot");
            let texture = w.window.renderer().unwrap().render_texture(&node, None);
            texture.save_to_png(path).unwrap();
        }
        second.terminal.feed_child(b"zterm-roundtrip\n");
        wait_for(|| !second.active());
        w.shutdown();
        w.closing.set(true);
        w.window.close();
        while context.pending() {
            context.iteration(false);
        }
        std::env::remove_var("ZTERM_SMOKE_TEST");
        if let Some(value) = previous {
            std::env::set_var("XDG_CONFIG_HOME", value);
        } else {
            std::env::remove_var("XDG_CONFIG_HOME");
        }
    }
}
