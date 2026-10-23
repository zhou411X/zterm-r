use crate::model::{terminal_palette, themes, Appearance};
use gtk::gdk;
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, OnceLock};
use vte::prelude::*;

fn rgba(value: &str) -> gdk::RGBA {
    gdk::RGBA::parse(value).expect("validated built-in or user color")
}

pub fn apply_terminal(term: &vte::Terminal, settings: &Appearance) {
    term.set_audible_bell(false);
    term.set_font(Some(&gtk::pango::FontDescription::from_string(
        &settings.font,
    )));
    let foreground = rgba(&settings.foreground);
    let mut background = rgba(&settings.background);
    if settings.background_opacity < 1.0 || !settings.background_image.is_empty() {
        background.set_alpha(0.0);
    }
    let palette: Vec<_> = terminal_palette(&settings.theme)
        .iter()
        .map(|value| rgba(value))
        .collect();
    let palette_refs: Vec<_> = palette.iter().collect();
    term.set_colors(Some(&foreground), Some(&background), &palette_refs);
    term.set_color_cursor(Some(&rgba(&settings.cursor)));
    term.set_color_cursor_foreground(Some(&rgba(&settings.background)));
}

fn blend(background: &str, foreground: &str, amount: f64) -> String {
    let bg = rgba(background);
    let fg = rgba(foreground);
    format!(
        "#{:02x}{:02x}{:02x}",
        ((f64::from(bg.red()) + f64::from(fg.red() - bg.red()) * amount) * 255.0).round() as u8,
        ((f64::from(bg.green()) + f64::from(fg.green() - bg.green()) * amount) * 255.0).round()
            as u8,
        ((f64::from(bg.blue()) + f64::from(fg.blue() - bg.blue()) * amount) * 255.0).round() as u8
    )
}

pub fn css(settings: &Appearance) -> String {
    // GTK4 paints VTE's CSS background before its terminal content. The terminal
    // already draws its configured solid color; transparent modes reveal our image.
    const TERMINAL_CSS: &str =
        ".zterm vte-terminal, .zterm-ui vte-terminal { background: transparent; }";
    let name = if settings.interface_theme == "follow-terminal" {
        &settings.theme
    } else {
        &settings.interface_theme
    };
    if name == "system" {
        return TERMINAL_CSS.into();
    }
    let theme = themes().remove(name).expect("validated interface theme");
    let foreground = if name == "gnome-green-on-black" {
        "#d0d0d0"
    } else {
        &theme.foreground
    };
    let accent = if name == "ubuntu-default" {
        "#e95420".to_owned()
    } else if name == "gnome-green-on-black" {
        "#00af00".to_owned()
    } else {
        theme.accent.unwrap_or_else(|| theme.palette[4].clone())
    };
    let pairs = [
        ("@background", theme.background.clone()),
        ("@foreground", foreground.to_owned()),
        ("@surface", blend(&theme.background, foreground, 0.06)),
        ("@hover", blend(&theme.background, foreground, 0.12)),
        ("@border", blend(&theme.background, foreground, 0.25)),
        ("@selection", blend(&theme.background, &accent, 0.25)),
        ("@accent", accent),
        ("@opacity", settings.background_opacity.to_string()),
    ];
    let mut result = include_str!("../assets/style.css").to_owned();
    for (token, color) in pairs {
        result = result.replace(token, &color);
    }
    let dialog_css = result
        .replace(".zterm-backdrop", "@@BACKDROP@@")
        .replace(".zterm", ".zterm-ui")
        .replace("@@BACKDROP@@", ".zterm-backdrop");
    result.push_str(&dialog_css);
    result.push_str(TERMINAL_CSS);
    result
}

#[derive(Clone)]
pub struct Background {
    overlay: gtk::Overlay,
    base: gtk::Picture,
    image: gtk::Picture,
    generation: Rc<Cell<u64>>,
    state: Rc<RefCell<BackgroundState>>,
    terminal: bool,
}

#[derive(Default)]
struct BackgroundState {
    loading: bool,
    error: String,
    key: Option<(String, u8)>,
}

type Decoded = (Arc<Vec<u8>>, u32, u32);
type DecodeCell = Arc<OnceLock<Result<Decoded, String>>>;
static IMAGE_CACHE: OnceLock<Mutex<HashMap<(String, u8), DecodeCell>>> = OnceLock::new();
thread_local! {
    static TEXTURE_CACHE: RefCell<HashMap<(String, u8), gdk::MemoryTexture>> = RefCell::new(HashMap::new());
}

fn cached_image(key: &(String, u8)) -> Result<Decoded, String> {
    let cache = IMAGE_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let entry = {
        let mut cache = cache.lock().expect("background cache lock");
        // A few recent edits stay warm; repeated terminal backgrounds share one decode.
        if cache.len() >= 4 && !cache.contains_key(key) {
            cache.clear();
        }
        cache
            .entry(key.clone())
            .or_insert_with(|| Arc::new(OnceLock::new()))
            .clone()
    };
    entry
        .get_or_init(|| {
            decode_image(&key.0, key.1)
                .map(|(pixels, width, height)| (Arc::new(pixels), width, height))
        })
        .clone()
}

impl Background {
    pub fn new(child: &impl IsA<gtk::Widget>, terminal: bool) -> Self {
        let overlay = gtk::Overlay::new();
        let base = gtk::Picture::new();
        let image = gtk::Picture::new();
        base.set_can_target(false);
        base.set_content_fit(gtk::ContentFit::Fill);
        image.set_can_target(false);
        image.set_content_fit(gtk::ContentFit::Cover);
        overlay.set_child(Some(&base));
        overlay.add_overlay(&image);
        overlay.add_overlay(child);
        overlay.set_measure_overlay(child, true);
        Self {
            overlay,
            base,
            image,
            generation: Rc::new(Cell::new(0)),
            state: Rc::new(RefCell::new(BackgroundState::default())),
            terminal,
        }
    }

    pub fn widget(&self) -> gtk::Overlay {
        self.overlay.clone()
    }
    pub fn loading(&self) -> bool {
        self.state.borrow().loading
    }
    pub fn error(&self) -> String {
        self.state.borrow().error.clone()
    }

    pub fn update(&self, settings: &Appearance) {
        let color = if self.terminal || settings.interface_theme == "system" {
            settings.background.clone()
        } else {
            let name = if settings.interface_theme == "follow-terminal" {
                &settings.theme
            } else {
                &settings.interface_theme
            };
            // Keep the backdrop in step with the window-scoped interface palette.
            themes()[name].background.clone()
        };
        let parsed = rgba(&color);
        let pixel = [
            (parsed.red() * 255.0).round() as u8,
            (parsed.green() * 255.0).round() as u8,
            (parsed.blue() * 255.0).round() as u8,
            (settings.background_opacity * 255.0).round() as u8,
        ];
        let bytes = glib::Bytes::from(&pixel[..]);
        let texture = gdk::MemoryTexture::new(1, 1, gdk::MemoryFormat::R8g8b8a8, &bytes, 4);
        self.base.set_paintable(Some(&texture));
        self.image
            .set_opacity(settings.background_image_opacity * settings.background_opacity);
        let show_image = !settings.background_image.is_empty()
            && (!self.terminal || settings.background_on_terminal);
        self.image.set_visible(show_image);
        let key = (
            settings.background_image.clone(),
            settings.background_blur.round() as u8,
        );
        if self.state.borrow().key.as_ref() == Some(&key) {
            return;
        }
        {
            let mut state = self.state.borrow_mut();
            state.key = Some(key.clone());
            state.loading = !key.0.is_empty();
            state.error.clear();
        }
        self.generation.set(self.generation.get() + 1);
        let generation = self.generation.get();
        if key.0.is_empty() {
            self.image.set_paintable(None::<&gdk::Paintable>);
            return;
        }
        if let Some(texture) = TEXTURE_CACHE.with(|cache| cache.borrow().get(&key).cloned()) {
            self.image.set_paintable(Some(&texture));
            self.state.borrow_mut().loading = false;
            return;
        }
        let (sender, receiver) = mpsc::channel();
        let cache_key = key.clone();
        std::thread::spawn(move || {
            let _ = sender.send(cached_image(&cache_key));
        });
        let image = self.image.clone();
        let current = self.generation.clone();
        let state = self.state.clone();
        let texture_key = key.clone();
        glib::timeout_add_local(std::time::Duration::from_millis(40), move || {
            if current.get() != generation {
                return glib::ControlFlow::Break;
            }
            match receiver.try_recv() {
                Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
                result => {
                    let result = match result {
                        Ok(value) => value,
                        Err(_) => Err("背景图片处理线程意外退出".to_owned()),
                    };
                    let mut state = state.borrow_mut();
                    state.loading = false;
                    match result {
                        Ok((pixels, width, height)) => {
                            let texture = TEXTURE_CACHE.with(|cache| {
                                let mut cache = cache.borrow_mut();
                                if let Some(texture) = cache.get(&texture_key) {
                                    return texture.clone();
                                }
                                if cache.len() >= 4 {
                                    cache.clear();
                                }
                                let bytes = glib::Bytes::from(&pixels[..]);
                                let texture = gdk::MemoryTexture::new(
                                    width as i32,
                                    height as i32,
                                    gdk::MemoryFormat::R8g8b8a8,
                                    &bytes,
                                    width as usize * 4,
                                );
                                cache.insert(texture_key.clone(), texture.clone());
                                texture
                            });
                            image.set_paintable(Some(&texture));
                        }
                        Err(error) => {
                            image.set_paintable(None::<&gdk::Paintable>);
                            state.error = error;
                        }
                    }
                    glib::ControlFlow::Break
                }
            }
        });
    }
}

fn decode_image(path: &str, blur: u8) -> Result<(Vec<u8>, u32, u32), String> {
    let reader = image::io::Reader::open(path)
        .map_err(|e| e.to_string())?
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    if !matches!(
        reader.format(),
        Some(image::ImageFormat::Png | image::ImageFormat::Jpeg | image::ImageFormat::WebP)
    ) {
        return Err("请选择 PNG、JPEG 或 WebP 图片".into());
    }
    let (width, height) = reader.into_dimensions().map_err(|e| e.to_string())?;
    if u64::from(width) * u64::from(height) > 40_000_000 {
        return Err("图片超过 4000 万像素，请缩小后再试".into());
    }
    let mut image = image::open(Path::new(path))
        .map_err(|e| e.to_string())?
        .to_rgba8();
    if width.max(height) > 2560 {
        let ratio = 2560.0 / width.max(height) as f64;
        image = image::imageops::resize(
            &image,
            (width as f64 * ratio).round() as u32,
            (height as f64 * ratio).round() as u32,
            image::imageops::FilterType::Triangle,
        );
    }
    if blur > 0 {
        image = image::imageops::blur(&image, blur as f32);
    }
    let (width, height) = image.dimensions();
    Ok((image.into_raw(), width, height))
}

fn form_row(label: &str, widget: &impl IsA<gtk::Widget>) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    let caption = gtk::Label::new(Some(label));
    caption.set_xalign(0.0);
    caption.set_width_chars(17);
    row.append(&caption);
    widget.set_hexpand(true);
    row.append(widget);
    row
}

fn color_hex(color: gdk::RGBA) -> String {
    format!(
        "#{:02x}{:02x}{:02x}",
        (color.red() * 255.0).round() as u8,
        (color.green() * 255.0).round() as u8,
        (color.blue() * 255.0).round() as u8
    )
}

fn render_preview(terminal: &vte::Terminal, background: &Background, settings: &Appearance) {
    apply_terminal(terminal, settings);
    // Preview the chosen image even when it is only enabled for the main window.
    // The user's terminal-session preference remains unchanged.
    let mut preview = settings.clone();
    preview.background_on_terminal = true;
    background.update(&preview);
}

thread_local! {
    static DIALOGS: RefCell<HashMap<usize, glib::WeakRef<gtk::Window>>> = RefCell::new(HashMap::new());
}

pub fn dialog(
    parent: &gtk::ApplicationWindow,
    settings: Appearance,
    on_preview: impl Fn(Appearance) + 'static,
    on_save: impl Fn(Appearance) + 'static,
) {
    let parent_id = parent.as_ptr() as usize;
    if let Some(existing) = DIALOGS.with(|dialogs| {
        dialogs
            .borrow()
            .get(&parent_id)
            .and_then(glib::WeakRef::upgrade)
    }) {
        if existing.transient_for().as_ref() == Some(parent.upcast_ref::<gtk::Window>()) {
            existing.present();
            return;
        }
    }
    let window = gtk::Window::builder()
        .title("外观设置")
        .transient_for(parent)
        .modal(false)
        .default_width(660)
        .default_height(690)
        .build();
    if let Some(application) = parent.application() {
        window.set_application(Some(&application));
    }
    DIALOGS.with(|dialogs| {
        dialogs.borrow_mut().insert(parent_id, window.downgrade());
    });
    window.add_css_class("zterm");
    let original = settings.clone();
    let value = Rc::new(RefCell::new(settings));
    let preview_callback: Rc<dyn Fn(Appearance)> = Rc::new(on_preview);
    let save_callback: Rc<dyn Fn(Appearance)> = Rc::new(on_save);
    let saved = Rc::new(Cell::new(false));

    let root = gtk::Box::new(gtk::Orientation::Vertical, 12);
    root.set_margin_top(16);
    root.set_margin_bottom(16);
    root.set_margin_start(18);
    root.set_margin_end(18);
    window.set_child(Some(&root));
    let scroll = gtk::ScrolledWindow::new();
    scroll.set_vexpand(true);
    scroll.set_overlay_scrolling(false);
    scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    root.append(&scroll);
    let controls = gtk::Box::new(gtk::Orientation::Vertical, 10);
    controls.set_margin_end(8);
    scroll.set_child(Some(&controls));
    let tabs = gtk::Notebook::new();
    tabs.add_css_class("appearance-tabs");
    let colors = gtk::Box::new(gtk::Orientation::Vertical, 10);
    let effects = gtk::Box::new(gtk::Orientation::Vertical, 10);
    for page in [&colors, &effects] {
        crate::widgets::margins(page, 12);
    }
    tabs.append_page(&colors, Some(&gtk::Label::new(Some("字体与配色"))));
    tabs.append_page(&effects, Some(&gtk::Label::new(Some("背景效果"))));
    controls.append(&tabs);

    let font = gtk::FontButton::new();
    font.set_font(&value.borrow().font);
    colors.append(&form_row("字体与字号", &font));
    let theme = gtk::ComboBoxText::new();
    for (key, item) in themes() {
        theme.append(Some(&key), &item.name);
    }
    theme.set_active_id(Some(&value.borrow().theme));
    colors.append(&form_row("终端配色", &theme));
    let interface = gtk::ComboBoxText::new();
    interface.append(Some("follow-terminal"), "跟随终端配色");
    interface.append(Some("system"), "跟随系统外观");
    for (key, item) in themes() {
        interface.append(Some(&key), &item.name);
    }
    interface.set_active_id(Some(&value.borrow().interface_theme));
    colors.append(&form_row("界面主题", &interface));
    let foreground = gtk::ColorButton::new();
    foreground.set_rgba(&rgba(&value.borrow().foreground));
    colors.append(&form_row("文字颜色", &foreground));
    let background = gtk::ColorButton::new();
    background.set_rgba(&rgba(&value.borrow().background));
    colors.append(&form_row("背景颜色", &background));
    let cursor = gtk::ColorButton::new();
    cursor.set_rgba(&rgba(&value.borrow().cursor));
    colors.append(&form_row("光标颜色", &cursor));

    let image_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let choose_image = gtk::Button::with_label("选择图片…");
    choose_image.set_hexpand(true);
    let clear_image = gtk::Button::with_label("清除");
    image_row.append(&choose_image);
    image_row.append(&clear_image);
    effects.append(&form_row("背景图片", &image_row));
    let image_name = gtk::Label::new(None);
    image_name.set_xalign(0.0);
    image_name.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    image_name.set_max_width_chars(60);
    image_name.set_text(&value.borrow().background_image);
    effects.append(&image_name);

    let opacity = gtk::Scale::with_range(gtk::Orientation::Horizontal, 15.0, 100.0, 1.0);
    opacity.set_value(value.borrow().background_opacity * 100.0);
    effects.append(&form_row("背景不透明度 %", &opacity));
    let image_opacity = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 100.0, 1.0);
    image_opacity.set_value(value.borrow().background_image_opacity * 100.0);
    effects.append(&form_row("图片强度 %", &image_opacity));
    let blur = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 40.0, 1.0);
    blur.set_value(value.borrow().background_blur);
    effects.append(&form_row("图片毛玻璃（0 为关闭）", &blur));
    for scale in [&opacity, &image_opacity, &blur] {
        scale.set_draw_value(true);
        scale.set_digits(0);
    }
    let image_on_terminal = gtk::CheckButton::with_label("背景图片同时应用到终端");
    image_on_terminal.set_active(value.borrow().background_on_terminal);
    effects.append(&image_on_terminal);
    let hint = gtk::Label::new(Some(
        "下方实时预览背景图片、强度与毛玻璃效果；勾选后应用到终端会话。\n毛玻璃仅模糊图片，文字保持清晰。取消恢复原样，保存后应用到所有会话。",
    ));
    hint.set_xalign(0.0);
    hint.set_wrap(true);
    hint.add_css_class("dim-label");
    controls.append(&hint);

    let terminal = vte::Terminal::new();
    terminal.set_size(60, 5);
    terminal.set_size_request(-1, 130);
    terminal.set_vexpand(false);
    terminal.set_input_enabled(false);
    terminal.set_cursor_blink_mode(vte::CursorBlinkMode::Off);
    terminal.feed(b"\xe7\xbb\x88\xe7\xab\xaf\xe9\xa2\x84\xe8\xa7\x88  AaBb 0123456789\r\n\x1b[32muser@workbench\x1b[0m:~$ ls\r\n\x1b[34mworkspace/\x1b[0m  \x1b[33mnotes.txt\x1b[0m\r\n$ ");
    let preview_background = Background::new(&terminal, true);
    controls.append(&preview_background.widget());
    let error = gtk::Label::new(None);
    error.set_xalign(0.0);
    error.set_wrap(true);
    error.add_css_class("error");
    root.append(&error);
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    actions.set_halign(gtk::Align::End);
    let defaults = gtk::Button::with_label("恢复默认");
    let cancel = gtk::Button::with_label("取消");
    let save = gtk::Button::with_label("保存并应用");
    save.add_css_class("suggested-action");
    for button in [&defaults, &cancel, &save] {
        actions.append(button);
    }
    root.append(&actions);

    let refresh: Rc<dyn Fn()> = {
        let value = value.clone();
        let terminal = terminal.clone();
        let preview_background = preview_background.clone();
        let preview_callback = preview_callback.clone();
        let error = error.clone();
        Rc::new(move || {
            let settings = value.borrow().clone();
            render_preview(&terminal, &preview_background, &settings);
            preview_callback(settings);
            error.set_text(&preview_background.error());
        })
    };
    render_preview(&terminal, &preview_background, &value.borrow());

    {
        let value = value.clone();
        let refresh = refresh.clone();
        font.connect_font_set(move |button| {
            if let Some(font) = button.font() {
                value.borrow_mut().font = font.to_string();
                refresh();
            }
        });
    }
    {
        let value = value.clone();
        let refresh = refresh.clone();
        let foreground = foreground.clone();
        let background = background.clone();
        let cursor = cursor.clone();
        theme.connect_changed(move |combo| {
            if let Some(name) = combo.active_id() {
                let theme = themes()
                    .remove(name.as_str())
                    .expect("selected built-in theme");
                {
                    let mut settings = value.borrow_mut();
                    settings.theme = name.to_string();
                    settings.foreground = theme.foreground.clone();
                    settings.background = theme.background.clone();
                    settings.cursor = theme.cursor.clone();
                }
                foreground.set_rgba(&rgba(&theme.foreground));
                background.set_rgba(&rgba(&theme.background));
                cursor.set_rgba(&rgba(&theme.cursor));
                refresh();
            }
        });
    }
    {
        let value = value.clone();
        let refresh = refresh.clone();
        interface.connect_changed(move |combo| {
            if let Some(name) = combo.active_id() {
                value.borrow_mut().interface_theme = name.to_string();
                refresh();
            }
        });
    }
    for (button, field) in [(&foreground, 0), (&background, 1), (&cursor, 2)] {
        let value = value.clone();
        let refresh = refresh.clone();
        button.connect_color_set(move |button| {
            let color = color_hex(button.rgba());
            let mut settings = value.borrow_mut();
            match field {
                0 => settings.foreground = color,
                1 => settings.background = color,
                _ => settings.cursor = color,
            }
            drop(settings);
            refresh();
        });
    }
    for (scale, field) in [(&opacity, 0), (&image_opacity, 1), (&blur, 2)] {
        let value = value.clone();
        let refresh = refresh.clone();
        scale.connect_value_changed(move |scale| {
            let mut settings = value.borrow_mut();
            match field {
                0 => settings.background_opacity = scale.value() / 100.0,
                1 => settings.background_image_opacity = scale.value() / 100.0,
                _ => settings.background_blur = scale.value().round(),
            }
            drop(settings);
            refresh();
        });
    }
    {
        let value = value.clone();
        let refresh = refresh.clone();
        image_on_terminal.connect_toggled(move |button| {
            value.borrow_mut().background_on_terminal = button.is_active();
            refresh();
        });
    }
    {
        let value = value.clone();
        let refresh = refresh.clone();
        let image_name = image_name.clone();
        clear_image.connect_clicked(move |_| {
            value.borrow_mut().background_image.clear();
            image_name.set_text("");
            refresh();
        });
    }
    {
        let value = value.clone();
        let refresh = refresh.clone();
        let image_name = image_name.clone();
        let window = window.downgrade();
        let error = error.clone();
        choose_image.connect_clicked(move |_| {
            let Some(window) = window.upgrade() else {
                return;
            };
            let filter = gtk::FileFilter::new();
            filter.set_name(Some("PNG、JPEG 或 WebP 图片"));
            for mime in ["image/png", "image/jpeg", "image/webp"] {
                filter.add_mime_type(mime);
            }
            let filters = gio::ListStore::new::<gtk::FileFilter>();
            filters.append(&filter);
            let chooser = gtk::FileDialog::builder()
                .title("选择背景图片")
                .accept_label("选择")
                .filters(&filters)
                .modal(true)
                .build();
            let value = value.clone();
            let refresh = refresh.clone();
            let image_name = image_name.clone();
            let error = error.clone();
            let owner = window.downgrade();
            chooser.open(Some(&window), None::<&gio::Cancellable>, move |result| {
                if owner.upgrade().is_none() {
                    return;
                }
                match result {
                    Ok(file) => {
                        if let Some(path) = file.path() {
                            let name = path.to_string_lossy().into_owned();
                            value.borrow_mut().background_image = name.clone();
                            image_name.set_text(&name);
                            refresh();
                        }
                    }
                    Err(e)
                        if e.matches(gtk::DialogError::Dismissed)
                            || e.matches(gtk::DialogError::Cancelled) => {}
                    Err(e) => error.set_text(&format!("无法打开图片选择窗口：{e}")),
                }
            });
        });
    }
    {
        let window = window.downgrade();
        cancel.connect_clicked(move |_| {
            if let Some(window) = window.upgrade() {
                window.close();
            }
        });
    }
    {
        let defaults_value = Appearance::default();
        let value = value.clone();
        let refresh = refresh.clone();
        let font = font.clone();
        let theme = theme.clone();
        let interface = interface.clone();
        let foreground = foreground.clone();
        let background = background.clone();
        let cursor = cursor.clone();
        let opacity = opacity.clone();
        let image_opacity = image_opacity.clone();
        let blur = blur.clone();
        let image_on_terminal = image_on_terminal.clone();
        let image_name = image_name.clone();
        defaults.connect_clicked(move |_| {
            *value.borrow_mut() = defaults_value.clone();
            font.set_font(&defaults_value.font);
            theme.set_active_id(Some(&defaults_value.theme));
            interface.set_active_id(Some(&defaults_value.interface_theme));
            foreground.set_rgba(&rgba(&defaults_value.foreground));
            background.set_rgba(&rgba(&defaults_value.background));
            cursor.set_rgba(&rgba(&defaults_value.cursor));
            opacity.set_value(defaults_value.background_opacity * 100.0);
            image_opacity.set_value(defaults_value.background_image_opacity * 100.0);
            blur.set_value(defaults_value.background_blur);
            image_on_terminal.set_active(defaults_value.background_on_terminal);
            image_name.set_text("");
            refresh();
        });
    }
    {
        let value = value.clone();
        let error = error.clone();
        let window = window.downgrade();
        let saved = saved.clone();
        let save_callback = save_callback.clone();
        save.connect_clicked(move |_| {
            let settings = value.borrow().clone();
            if preview_background.loading() {
                error.set_text("正在处理背景图片…");
                return;
            }
            if !preview_background.error().is_empty() {
                error.set_text(&preview_background.error());
                return;
            }
            if let Err(message) = settings.validate().and_then(|_| settings.save()) {
                error.set_text(&format!("保存失败：{message}"));
                return;
            }
            saved.set(true);
            save_callback(settings);
            if let Some(window) = window.upgrade() {
                window.close();
            }
        });
    }
    window.connect_close_request(move |_| {
        DIALOGS.with(|dialogs| {
            dialogs.borrow_mut().remove(&parent_id);
        });
        if !saved.get() {
            preview_callback(original.clone());
        }
        glib::Propagation::Proceed
    });
    window.present();
}

#[cfg(test)]
pub fn verify_dialog_layout(parent: &gtk::ApplicationWindow) {
    let previewed = Rc::new(RefCell::new(Appearance::default()));
    let observed = previewed.clone();
    dialog(
        parent,
        Appearance::default(),
        move |settings| *observed.borrow_mut() = settings,
        |_| {},
    );
    let window = DIALOGS.with(|dialogs| {
        dialogs.borrow()[&(parent.as_ptr() as usize)]
            .upgrade()
            .unwrap()
    });
    let context = glib::MainContext::default();
    let settle = || {
        for _ in 0..30 {
            while context.pending() {
                context.iteration(false);
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    };
    settle();
    let root = window.child().unwrap();
    let scroll = root
        .first_child()
        .unwrap()
        .downcast::<gtk::ScrolledWindow>()
        .unwrap();
    let controls = scroll
        .child()
        .unwrap()
        .downcast::<gtk::Viewport>()
        .unwrap()
        .child()
        .unwrap();
    let tabs = controls
        .first_child()
        .unwrap()
        .downcast::<gtk::Notebook>()
        .unwrap();
    assert_eq!(tabs.n_pages(), 2);
    assert_eq!(
        tabs.tab_label_text(&tabs.nth_page(Some(0)).unwrap())
            .as_deref(),
        Some("字体与配色")
    );
    assert_eq!(
        tabs.tab_label_text(&tabs.nth_page(Some(1)).unwrap())
            .as_deref(),
        Some("背景效果")
    );
    tabs.set_current_page(Some(1));
    settle();
    fn find_choose(widget: &gtk::Widget) -> Option<gtk::Button> {
        if let Ok(button) = widget.clone().downcast::<gtk::Button>() {
            if button.label().as_deref() == Some("选择图片…") {
                return Some(button);
            }
        }
        let mut child = widget.first_child();
        while let Some(widget) = child {
            if let Some(button) = find_choose(&widget) {
                return Some(button);
            }
            child = widget.next_sibling();
        }
        None
    }
    find_choose(&controls).unwrap().emit_clicked();
    settle();
    let chooser = gtk::Window::list_toplevels()
        .into_iter()
        .find_map(|widget| widget.downcast::<gtk::FileChooserDialog>().ok())
        .expect("background image chooser must stay open until response");
    chooser.response(gtk::ResponseType::Cancel);
    settle();
    assert!(previewed.borrow().background_image.is_empty());
    let images = tempfile::tempdir().unwrap();
    let image_path = images.path().join("background.png");
    image::RgbaImage::from_fn(64, 32, |x, y| {
        if (x / 8 + y / 8) % 2 == 0 {
            image::Rgba([32, 72, 128, 255])
        } else {
            image::Rgba([240, 160, 48, 255])
        }
    })
    .save(&image_path)
    .unwrap();
    find_choose(&controls).unwrap().emit_clicked();
    settle();
    let chooser = gtk::Window::list_toplevels()
        .into_iter()
        .filter(|widget| widget.is_visible())
        .find_map(|widget| widget.downcast::<gtk::FileChooserDialog>().ok())
        .unwrap();
    chooser.set_file(&gio::File::for_path(&image_path)).unwrap();
    settle();
    chooser.response(gtk::ResponseType::Accept);
    settle();
    assert_eq!(
        previewed.borrow().background_image,
        image_path.to_string_lossy()
    );
    let preview = controls
        .last_child()
        .unwrap()
        .downcast::<gtk::Overlay>()
        .unwrap();
    let picture = preview
        .first_child()
        .unwrap()
        .next_sibling()
        .unwrap()
        .downcast::<gtk::Picture>()
        .unwrap();
    assert!(
        picture.is_visible() && picture.paintable().is_some(),
        "selected image must be visible in the preview"
    );
    assert!(
        !previewed.borrow().background_on_terminal,
        "preview must not change the user's terminal setting"
    );
    let pixels = || {
        let paintable = gtk::WidgetPaintable::new(Some(&preview));
        let snapshot = gtk::Snapshot::new();
        paintable.snapshot(
            &snapshot,
            f64::from(preview.width()),
            f64::from(preview.height()),
        );
        let texture = window
            .renderer()
            .unwrap()
            .render_texture(&snapshot.to_node().unwrap(), None);
        let stride = texture.width() as usize * 4;
        let mut bytes = vec![0; stride * texture.height() as usize];
        texture.download(&mut bytes, stride);
        bytes
    };
    let effects = tabs.nth_page(Some(1)).unwrap();
    let mut scales = Vec::new();
    let mut row = effects.first_child();
    while let Some(widget) = row {
        if let Some(scale) = widget
            .last_child()
            .and_then(|w| w.downcast::<gtk::Scale>().ok())
        {
            scales.push(scale);
        }
        row = widget.next_sibling();
    }
    assert_eq!(scales.len(), 3);
    scales[2].set_value(0.0);
    scales[1].set_value(100.0);
    settle();
    let sharp = pixels();
    scales[1].set_value(0.0);
    settle();
    assert!(
        sharp != pixels(),
        "image intensity must update the rendered preview"
    );
    scales[1].set_value(100.0);
    scales[0].set_value(50.0);
    settle();
    assert!(
        sharp != pixels(),
        "background opacity must update the rendered preview"
    );
    scales[0].set_value(100.0);
    scales[2].set_value(12.0);
    settle();
    assert!(
        sharp != pixels(),
        "blur must update the image without recreating the dialog"
    );
    tabs.set_current_page(Some(0));
    settle();
    assert!(
        preview.is_visible() && picture.is_visible(),
        "both tabs share the live image preview"
    );
    tabs.set_current_page(Some(1));
    settle();
    assert_eq!(
        root.last_child()
            .unwrap()
            .prev_sibling()
            .unwrap()
            .downcast::<gtk::Label>()
            .unwrap()
            .text(),
        ""
    );
    for height in [690, 480] {
        window.set_default_size(660, height);
        scroll.vadjustment().set_value(0.0);
        settle();
        assert!(
            scroll.height() >= height - 180,
            "appearance controls have no usable viewport: {}",
            scroll.height()
        );
        let first = effects
            .first_child()
            .unwrap()
            .compute_bounds(&scroll)
            .unwrap();
        assert!(first.y() >= 0.0 && first.y() + first.height() <= scroll.height() as f32);
        let actions = root.last_child().unwrap().compute_bounds(&window).unwrap();
        assert!(actions.y() + actions.height() <= window.height() as f32);
        if let Some(path) = std::env::var_os("ZTERM_APPEARANCE_SCREENSHOT") {
            let mut path = std::path::PathBuf::from(path);
            if height == 480 {
                path.set_extension("small.png");
            }
            let paintable = gtk::WidgetPaintable::new(Some(&window));
            let snapshot = gtk::Snapshot::new();
            paintable.snapshot(
                &snapshot,
                f64::from(window.width()),
                f64::from(window.height()),
            );
            let texture = window
                .renderer()
                .unwrap()
                .render_texture(&snapshot.to_node().unwrap(), None);
            texture.save_to_png(path).unwrap();
        }
        let adjustment = scroll.vadjustment();
        adjustment.set_value(adjustment.upper() - adjustment.page_size());
        settle();
        let last = controls
            .last_child()
            .unwrap()
            .compute_bounds(&scroll)
            .unwrap();
        assert!(
            last.y() >= -1.0 && last.y() + last.height() <= scroll.height() as f32 + 1.0,
            "scrolling must expose the entire preview"
        );
    }
    let clear = effects
        .first_child()
        .unwrap()
        .last_child()
        .unwrap()
        .last_child()
        .unwrap()
        .downcast::<gtk::Button>()
        .unwrap();
    clear.emit_clicked();
    settle();
    assert!(
        !picture.is_visible() && picture.paintable().is_none(),
        "clearing the image must clear the preview"
    );
    window.close();
    assert!(
        previewed.borrow().background_image.is_empty(),
        "cancelling appearance restores the original background"
    );
    assert_eq!(
        previewed.borrow().background_blur,
        Appearance::default().background_blur
    );
}
