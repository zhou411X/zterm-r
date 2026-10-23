use crate::{model, widgets::*};
use gtk::{gdk, prelude::*};
use serde::{Deserialize, Serialize};
use std::{cell::Cell, collections::BTreeMap, path::PathBuf, rc::Rc};

pub const SHORTCUTS: &[(&str, &str, &str)] = &[
    ("new_session", "新建会话", "<Control><Shift>t"),
    ("close_session", "关闭会话", "<Control><Shift>w"),
    ("copy", "复制", "<Control><Shift>c"),
    ("paste", "粘贴", "<Control><Shift>v"),
    ("search", "搜索终端", "<Control><Shift>f"),
    ("next_tab", "下一个标签页", "<Control>Page_Down"),
    ("previous_tab", "上一个标签页", "<Control>Page_Up"),
    ("zoom_in", "放大字体", "<Control>equal"),
    ("zoom_out", "缩小字体", "<Control>minus"),
    ("zoom_reset", "重置字体大小", "<Control>0"),
    ("tab_1", "切换到标签页 1", "<Alt>1"),
    ("tab_2", "切换到标签页 2", "<Alt>2"),
    ("tab_3", "切换到标签页 3", "<Alt>3"),
    ("tab_4", "切换到标签页 4", "<Alt>4"),
    ("tab_5", "切换到标签页 5", "<Alt>5"),
    ("tab_6", "切换到标签页 6", "<Alt>6"),
    ("tab_7", "切换到标签页 7", "<Alt>7"),
    ("tab_8", "切换到标签页 8", "<Alt>8"),
    ("tab_9", "切换到标签页 9", "<Alt>9"),
];

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
    pub scrollback_lines: u32,
    pub shortcuts: BTreeMap<String, String>,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            scrollback_lines: 9999,
            shortcuts: SHORTCUTS
                .iter()
                .map(|(id, _, key)| (id.to_string(), key.to_string()))
                .collect(),
        }
    }
}

fn path() -> PathBuf {
    model::config_dir()
        .parent()
        .unwrap()
        .join("zterm-r/preferences.json")
}

pub fn shortcut_key(value: &str) -> Option<(gdk::Key, gdk::ModifierType)> {
    gtk::accelerator_parse(value).map(|(key, mods)| {
        (
            key.to_lower(),
            mods & gtk::accelerator_get_default_mod_mask(),
        )
    })
}

fn allowed(key: gdk::Key, mods: gdk::ModifierType) -> bool {
    gtk::accelerator_valid(key, mods)
        && (mods.intersects(
            gdk::ModifierType::CONTROL_MASK
                | gdk::ModifierType::ALT_MASK
                | gdk::ModifierType::SUPER_MASK
                | gdk::ModifierType::META_MASK,
        ) || (gdk::Key::F1..=gdk::Key::F35).contains(&key))
}

impl Preferences {
    pub fn load() -> Result<Self, String> {
        let data = match std::fs::read(path()) {
            Ok(data) => data,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e.to_string()),
        };
        let mut value: Self = serde_json::from_slice(&data).map_err(|e| e.to_string())?;
        for (id, _, key) in SHORTCUTS {
            value
                .shortcuts
                .entry(id.to_string())
                .or_insert_with(|| key.to_string());
        }
        value.validate()?;
        Ok(value)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.scrollback_lines > 1_000_000 {
            return Err("滚动缓冲区须为 0–1000000 行。".into());
        }
        let mut used = Vec::new();
        for (id, title, _) in SHORTCUTS {
            let Some(value) = self.shortcuts.get(*id) else {
                continue;
            };
            if value.is_empty() {
                continue;
            }
            let (key, mods) = shortcut_key(value)
                .filter(|(key, mods)| allowed(*key, *mods))
                .ok_or_else(|| {
                    format!("{title}：快捷键无效，请使用带 Ctrl、Alt 或 Super 的组合键，或功能键。")
                })?;
            if let Some((_, _, other)) = used.iter().find(|(k, m, _)| *k == key && *m == mods) {
                return Err(format!("“{title}”与“{other}”的快捷键冲突。"));
            }
            used.push((key, mods, *title));
        }
        Ok(())
    }

    pub fn save(&self) -> Result<(), String> {
        self.validate()?;
        model::save_json(&path(), self)
    }

    pub fn action_for(&self, key: gdk::Key, mods: gdk::ModifierType) -> Option<&str> {
        let key = key.to_lower();
        let mods = mods & gtk::accelerator_get_default_mod_mask();
        SHORTCUTS
            .iter()
            .find_map(|(id, _, _)| {
                (self.shortcuts.get(*id).and_then(|s| shortcut_key(s)) == Some((key, mods)))
                    .then_some(*id)
            })
            .or_else(|| {
                // Preserve Ctrl++ as an alias of the default Ctrl+= zoom binding.
                (key == gdk::Key::plus
                    && (mods == gdk::ModifierType::CONTROL_MASK
                        || mods
                            == (gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::SHIFT_MASK))
                    && self.shortcuts.get("zoom_in").map(String::as_str) == Some("<Control>equal"))
                .then_some("zoom_in")
            })
    }

    pub fn label_for(&self, id: &str) -> String {
        self.shortcuts
            .get(id)
            .map(|s| display_shortcut(s))
            .unwrap_or_else(|| "未设置".into())
    }
}

fn display_shortcut(value: &str) -> String {
    shortcut_key(value)
        .map(|(key, mods)| gtk::accelerator_get_label(key, mods).to_string())
        .unwrap_or_else(|| "未设置".into())
}

pub fn dialog(
    parent: &gtk::ApplicationWindow,
    initial: Preferences,
    shortcuts_page: bool,
    save: impl Fn(Preferences) + 'static,
) -> gtk::Dialog {
    let dialog = gtk::Dialog::builder()
        .title("设置")
        .transient_for(parent)
        .modal(true)
        .default_width(580)
        .default_height(560)
        .build();
    dialog.add_button("取消", gtk::ResponseType::Cancel);
    dialog.add_button("保存", gtk::ResponseType::Accept);
    let area = dialog.content_area();
    margins(&area, 16);
    area.set_spacing(12);
    let notebook = gtk::Notebook::new();
    notebook.set_vexpand(true);
    let terminal = gtk::Box::new(gtk::Orientation::Vertical, 16);
    margins(&terminal, 16);
    terminal.append(&label("滚动缓冲区（行）", "heading"));
    let lines = gtk::SpinButton::with_range(0.0, 1_000_000.0, 100.0);
    lines.set_increments(1.0, 1000.0);
    lines.set_numeric(true);
    lines.set_value(initial.scrollback_lines as f64);
    terminal.append(&lines);
    let explanation = label("默认 9999 行；0 表示不保留滚动历史。保存后应用于所有会话。\n减少缓冲区会丢弃超出行数的旧内容。", "dim-label");
    explanation.set_wrap(true);
    terminal.append(&explanation);
    terminal.append(&label("终端响铃：已关闭", "dim-label"));
    notebook.append_page(&terminal, Some(&gtk::Label::new(Some("终端"))));
    let keys = gtk::Box::new(gtk::Orientation::Vertical, 10);
    margins(&keys, 16);
    let hint = label(
        "点击快捷键后按下新组合。Esc 取消录入，Backspace/Delete 清除。",
        "dim-label",
    );
    hint.set_wrap(true);
    keys.append(&hint);
    let reset = gtk::Button::with_label("恢复默认快捷键");
    keys.append(&reset);
    let mut fields = Vec::new();
    for (id, title, default) in SHORTCUTS {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 16);
        let title = label(title, "body");
        title.set_hexpand(true);
        row.append(&title);
        let value = Rc::new(std::cell::RefCell::new(
            initial
                .shortcuts
                .get(*id)
                .cloned()
                .unwrap_or_else(|| default.to_string()),
        ));
        let button = gtk::Button::with_label(&display_shortcut(&value.borrow()));
        button.set_width_request(210);
        let capturing = Rc::new(Cell::new(false));
        let active = capturing.clone();
        button.connect_clicked(move |button| {
            active.set(true);
            button.set_label("请按下快捷键…");
            button.grab_focus();
        });
        let controller = gtk::EventControllerKey::new();
        controller.set_propagation_phase(gtk::PropagationPhase::Capture);
        let button_weak = button.downgrade();
        let stored = value.clone();
        let active = capturing.clone();
        controller.connect_key_pressed(move |_, key, _, mods| {
            if !active.get() {
                return glib::Propagation::Proceed;
            }
            let Some(button) = button_weak.upgrade() else {
                return glib::Propagation::Proceed;
            };
            if key == gdk::Key::Escape {
                active.set(false);
            } else if key == gdk::Key::BackSpace || key == gdk::Key::Delete {
                stored.borrow_mut().clear();
                active.set(false);
            } else {
                let mods = mods & gtk::accelerator_get_default_mod_mask();
                if !allowed(key, mods) {
                    return glib::Propagation::Stop;
                }
                *stored.borrow_mut() = gtk::accelerator_name(key.to_lower(), mods).to_string();
                active.set(false);
            }
            button.set_label(&display_shortcut(&stored.borrow()));
            glib::Propagation::Stop
        });
        button.add_controller(controller);
        let stored = value.clone();
        let active = capturing.clone();
        button.connect_has_focus_notify(move |button| {
            if !button.has_focus() {
                active.set(false);
                button.set_label(&display_shortcut(&stored.borrow()));
            }
        });
        row.append(&button);
        keys.append(&row);
        fields.push((id.to_string(), value, button, capturing));
    }
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&keys)
        .build();
    notebook.append_page(&scroller, Some(&gtk::Label::new(Some("快捷键"))));
    notebook.set_current_page(Some(u32::from(shortcuts_page)));
    area.append(&notebook);
    let error = label("", "error");
    error.set_wrap(true);
    area.append(&error);
    let fields = Rc::new(fields);
    let resetting = fields.clone();
    reset.connect_clicked(move |_| {
        let defaults = Preferences::default();
        for (id, value, button, active) in resetting.iter() {
            *value.borrow_mut() = defaults.shortcuts[id].clone();
            active.set(false);
            button.set_label(&display_shortcut(&value.borrow()));
        }
    });
    dialog.connect_response(move |dialog, response| {
        if response == gtk::ResponseType::Accept {
            lines.update();
            let value = Preferences {
                scrollback_lines: lines.value_as_int() as u32,
                shortcuts: fields
                    .iter()
                    .map(|(id, value, _, _)| (id.clone(), value.borrow().clone()))
                    .collect(),
            };
            if let Err(e) = value.save() {
                error.set_text(&e);
                return;
            }
            save(value);
        }
        dialog.close();
    });
    dialog.present();
    dialog
}

#[cfg(test)]
pub fn verify(parent: &gtk::ApplicationWindow) {
    fn widgets(root: &gtk::Widget) -> Vec<gtk::Widget> {
        let mut found = vec![root.clone()];
        let mut child = root.first_child();
        while let Some(widget) = child {
            found.extend(widgets(&widget));
            child = widget.next_sibling();
        }
        found
    }
    let defaults = Preferences::default();
    assert_eq!(Preferences::load().unwrap(), defaults);
    defaults.validate().unwrap();
    let mut invalid = defaults.clone();
    invalid
        .shortcuts
        .insert("search".into(), "<Shift><Control>t".into());
    assert!(invalid.save().unwrap_err().contains("冲突"));
    assert!(!path().exists());
    invalid.shortcuts.insert("search".into(), "a".into());
    assert!(invalid.validate().is_err());
    invalid = defaults.clone();
    invalid.scrollback_lines = 1_000_001;
    assert!(invalid.validate().is_err());
    let saved = Rc::new(std::cell::RefCell::new(None));
    let output = saved.clone();
    let dialog = dialog(parent, defaults.clone(), false, move |value| {
        *output.borrow_mut() = Some(value)
    });
    let all = widgets(dialog.upcast_ref());
    let spin = all
        .iter()
        .find_map(|w| w.clone().downcast::<gtk::SpinButton>().ok())
        .unwrap();
    spin.set_value(4321.0);
    let notebook = all
        .iter()
        .find_map(|w| w.clone().downcast::<gtk::Notebook>().ok())
        .unwrap();
    notebook.set_current_page(Some(1));
    let button = all
        .iter()
        .find_map(|w| {
            w.clone()
                .downcast::<gtk::Button>()
                .ok()
                .filter(|b| b.label().as_deref() == Some(&defaults.label_for("search")))
        })
        .unwrap();
    button.emit_clicked();
    let controllers = button.observe_controllers();
    let key = (0..controllers.n_items())
        .find_map(|i| {
            controllers
                .item(i)
                .unwrap()
                .downcast::<gtk::EventControllerKey>()
                .ok()
        })
        .unwrap();
    key.emit_by_name::<bool>(
        "key-pressed",
        &[&gdk::Key::F6, &0u32, &gdk::ModifierType::empty()],
    );
    assert_eq!(button.label().as_deref(), Some("F6"));
    dialog.response(gtk::ResponseType::Accept);
    let loaded = Preferences::load().unwrap();
    assert_eq!(loaded.scrollback_lines, 4321);
    assert_eq!(
        loaded.action_for(gdk::Key::F6, gdk::ModifierType::empty()),
        Some("search")
    );
    assert_eq!(loaded, saved.borrow().as_ref().unwrap().clone());
    let dialog = dialog_for_cancel(parent, loaded.clone());
    let spin = widgets(dialog.upcast_ref())
        .into_iter()
        .find_map(|w| w.downcast::<gtk::SpinButton>().ok())
        .unwrap();
    spin.set_value(0.0);
    dialog.response(gtk::ResponseType::Cancel);
    assert_eq!(Preferences::load().unwrap(), loaded);
    defaults.save().unwrap();
}

#[cfg(test)]
fn dialog_for_cancel(parent: &gtk::ApplicationWindow, value: Preferences) -> gtk::Dialog {
    dialog(parent, value, false, |_| {
        panic!("Cancel must not save settings")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn defaults_and_config_roundtrip() {
        let default: Preferences = serde_json::from_str("{}").unwrap();
        assert_eq!(default.scrollback_lines, 9999);
        assert_eq!(default.shortcuts.len(), SHORTCUTS.len());
        let mut custom = default;
        custom.scrollback_lines = 0;
        custom.shortcuts.insert("copy".into(), String::new());
        let json = serde_json::to_string(&custom).unwrap();
        assert_eq!(serde_json::from_str::<Preferences>(&json).unwrap(), custom);
    }
}
