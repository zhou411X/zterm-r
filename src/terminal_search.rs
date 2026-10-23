use crate::widgets::*;
use gtk::prelude::*;
use std::{cell::RefCell, rc::Rc};
use vte::prelude::*;

pub struct TerminalSearch {
    pub bar: gtk::Box,
    entry: gtk::SearchEntry,
    ignore_case: gtk::CheckButton,
    result: gtk::Label,
    terminal: vte::Terminal,
    pattern: RefCell<Option<(String, bool)>>,
}

impl TerminalSearch {
    pub fn new(terminal: &vte::Terminal) -> Rc<Self> {
        let bar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        margins(&bar, 8);
        bar.set_visible(false);
        let entry = gtk::SearchEntry::new();
        entry.set_placeholder_text(Some("搜索当前终端（含滚动历史）"));
        entry.set_hexpand(true);
        entry.set_tooltip_text(Some("按行查找并高亮每个匹配行的首个结果"));
        let ignore_case = gtk::CheckButton::with_label("忽略大小写");
        ignore_case.set_active(true);
        let previous = icon("go-up-symbolic", "上一处 · Shift+Enter");
        let next = icon("go-down-symbolic", "下一处 · Enter");
        let result = label("", "dim-label");
        let close = icon("window-close-symbolic", "关闭搜索 · Esc");
        bar.append(&entry);
        bar.append(&ignore_case);
        bar.append(&previous);
        bar.append(&next);
        bar.append(&result);
        bar.append(&close);
        let this = Rc::new(Self {
            bar,
            entry,
            ignore_case,
            result,
            terminal: terminal.clone(),
            pattern: RefCell::new(None),
        });
        let weak = Rc::downgrade(&this);
        this.entry.connect_search_changed(move |_| {
            if let Some(search) = weak.upgrade() {
                if search.bar.is_visible() {
                    search.find(false);
                }
            }
        });
        let weak = Rc::downgrade(&this);
        this.ignore_case.connect_toggled(move |_| {
            if let Some(search) = weak.upgrade() {
                search.find(false);
            }
        });
        for (button, backwards) in [(previous, true), (next, false)] {
            let weak = Rc::downgrade(&this);
            button.connect_clicked(move |_| {
                if let Some(search) = weak.upgrade() {
                    search.find(backwards);
                }
            });
        }
        let weak = Rc::downgrade(&this);
        close.connect_clicked(move |_| {
            if let Some(search) = weak.upgrade() {
                search.close();
            }
        });
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(&this);
        keys.connect_key_pressed(move |_, key, _, mods| {
            let Some(search) = weak.upgrade() else {
                return glib::Propagation::Proceed;
            };
            match key {
                gtk::gdk::Key::Escape => search.close(),
                gtk::gdk::Key::Return | gtk::gdk::Key::KP_Enter => {
                    search.find(mods.contains(gtk::gdk::ModifierType::SHIFT_MASK))
                }
                _ => return glib::Propagation::Proceed,
            }
            glib::Propagation::Stop
        });
        this.bar.add_controller(keys);
        this
    }

    pub fn open(&self) {
        self.bar.set_visible(true);
        self.entry.grab_focus();
        self.entry.select_region(0, -1);
        if self.pattern.borrow().is_none() {
            self.find(false);
        }
    }

    pub fn close(&self) {
        self.bar.set_visible(false);
        self.terminal.search_set_regex(None, 0);
        self.terminal.unselect_all();
        self.pattern.borrow_mut().take();
        self.result.set_text("");
        self.terminal.grab_focus();
    }

    fn find(&self, backwards: bool) {
        let text = self.entry.text().to_string();
        if text.is_empty() {
            self.terminal.search_set_regex(None, 0);
            self.terminal.unselect_all();
            self.pattern.borrow_mut().take();
            self.result.set_text("");
            return;
        }
        let pattern = (text, self.ignore_case.is_active());
        if self.pattern.borrow().as_ref() != Some(&pattern) {
            // VTE requires PCRE2_MULTILINE; inline flags enable Unicode caseless matching.
            // Quote literal input, including a literal PCRE quote terminator.
            let literal = pattern.0.replace("\\E", "\\E\\\\E\\Q");
            let expression = format!(
                "(*UTF)(*UCP){}\\Q{}\\E",
                if pattern.1 { "(?i)" } else { "" },
                literal
            );
            match vte::Regex::for_search(&expression, 0x00000400) {
                Ok(regex) => {
                    self.terminal.search_set_regex(Some(&regex), 0);
                    self.terminal.search_set_wrap_around(true);
                    *self.pattern.borrow_mut() = Some(pattern);
                }
                Err(e) => {
                    self.terminal.search_set_regex(None, 0);
                    self.terminal.unselect_all();
                    self.pattern.borrow_mut().take();
                    self.result.set_text(&format!("搜索失败：{e}"));
                    return;
                }
            }
        }
        let find = || {
            if backwards {
                self.terminal.search_find_previous()
            } else {
                self.terminal.search_find_next()
            }
        };
        // VTE may stop at the current match's line when wrapping and clear
        // the selection. The next search then starts from the buffer boundary.
        let found = find() || find();
        self.result.set_text(if found {
            "已高亮匹配"
        } else {
            "未找到"
        });
    }
}

#[cfg(test)]
pub fn verify(parent: &gtk::ApplicationWindow) {
    let terminal = vte::Terminal::new();
    terminal.set_scrollback_lines(9999);
    terminal.set_audible_bell(false);
    let search = TerminalSearch::new(&terminal);
    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.append(&search.bar);
    content.append(&terminal);
    let window = gtk::Window::builder()
        .transient_for(parent)
        .default_width(800)
        .default_height(300)
        .child(&content)
        .build();
    window.present();
    fn settle() {
        let context = glib::MainContext::default();
        for _ in 0..35 {
            while context.pending() {
                context.iteration(false);
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
    settle();
    terminal.feed("HistoryOnly\r\nNeedle\r\nneedle\r\nNEEDLE\r\n中文.[x]\\E éÉ\r\n".as_bytes());
    terminal.feed("filler\r\n".repeat(120).as_bytes());
    settle();
    search.open();
    settle();
    let check = |text: &str, expected: &str| {
        search.entry.set_text(text);
        settle();
        search.find(false);
        assert_eq!(
            terminal.text_selected(vte::Format::Text).as_deref(),
            Some(expected)
        );
        assert_eq!(search.result.text(), "已高亮匹配");
    };
    check("historyonly", "HistoryOnly");
    search.ignore_case.set_active(false);
    assert_eq!(search.result.text(), "未找到");
    assert!(!terminal.has_selection());
    check("HistoryOnly", "HistoryOnly");
    search.entry.set_text("needle");
    settle();
    search.find(false);
    assert_eq!(
        terminal.text_selected(vte::Format::Text).as_deref(),
        Some("needle")
    );
    search.ignore_case.set_active(true);
    let mut matches = std::collections::BTreeSet::new();
    for _ in 0..3 {
        search.find(false);
        matches.insert(
            terminal
                .text_selected(vte::Format::Text)
                .unwrap()
                .to_string(),
        );
    }
    assert_eq!(
        matches.len(),
        3,
        "case-insensitive search visits all three variants"
    );
    let current = terminal.text_selected(vte::Format::Text).unwrap();
    search.find(true);
    search.find(false);
    assert_eq!(terminal.text_selected(vte::Format::Text).unwrap(), current);
    check("中文.[x]\\E", "中文.[x]\\E");
    check("éé", "éÉ");
    search.entry.set_text("not-in-buffer");
    settle();
    assert_eq!(search.result.text(), "未找到");
    search.entry.set_text("");
    settle();
    assert!(!terminal.has_selection());
    check("HistoryOnly", "HistoryOnly");
    search.close();
    assert!(!terminal.has_selection());
    assert!(terminal.search_get_regex().is_none());
    assert!(!search.bar.is_visible());
    window.close();
}
