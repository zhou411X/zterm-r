use gtk::prelude::*;
use std::sync::mpsc;

pub fn icon(name: &str, tip: &str) -> gtk::Button {
    let button = gtk::Button::from_icon_name(name);
    button.set_tooltip_text(Some(tip));
    button.add_css_class("flat");
    button
}

pub fn label(text: &str, class: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.set_xalign(0.0);
    label.add_css_class(class);
    label
}

pub fn margins(widget: &impl IsA<gtk::Widget>, value: i32) {
    widget.set_margin_start(value);
    widget.set_margin_end(value);
    widget.set_margin_top(value);
    widget.set_margin_bottom(value);
}

pub fn message(parent: &gtk::ApplicationWindow, title: &str, detail: &str) {
    let dialog = gtk::MessageDialog::builder()
        .transient_for(parent)
        .modal(false)
        .message_type(gtk::MessageType::Error)
        .buttons(gtk::ButtonsType::Close)
        .text(title)
        .secondary_text(detail)
        .build();
    dialog.connect_response(|dialog, _| dialog.close());
    dialog.present();
}

pub fn confirm(
    parent: &gtk::ApplicationWindow,
    title: &str,
    detail: &str,
    yes: impl Fn() + 'static,
) {
    let dialog = gtk::MessageDialog::builder()
        .transient_for(parent)
        .modal(true)
        .message_type(gtk::MessageType::Question)
        .buttons(gtk::ButtonsType::None)
        .text(title)
        .secondary_text(detail)
        .build();
    dialog.add_button("取消", gtk::ResponseType::Cancel);
    dialog.add_button("确认", gtk::ResponseType::Accept);
    dialog.set_default_response(gtk::ResponseType::Cancel);
    dialog.connect_response(move |dialog, response| {
        dialog.close();
        if response == gtk::ResponseType::Accept {
            yes();
        }
    });
    dialog.present();
}

pub fn background<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
    done: impl FnOnce(T) + 'static,
) {
    // Keep the main loop alive until the worker has cleaned up its subprocesses,
    // including when its owning window has already closed.
    let mut hold = gio::Application::default().map(|app| app.hold());
    let (sender, receiver) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let _ = sender.send(work());
    });
    let mut done = Some(done);
    glib::timeout_add_local(
        std::time::Duration::from_millis(40),
        move || match receiver.try_recv() {
            Ok(value) => {
                done.take().unwrap()(value);
                hold.take();
                glib::ControlFlow::Break
            }
            Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
            Err(mpsc::TryRecvError::Disconnected) => {
                hold.take();
                glib::ControlFlow::Break
            }
        },
    );
}

pub fn prompt(
    parent: &gtk::ApplicationWindow,
    title: &str,
    detail: &str,
    initial: &str,
    accept: impl Fn(String) + 'static,
) {
    let dialog = gtk::Dialog::builder()
        .title(title)
        .transient_for(parent)
        .modal(false)
        .default_width(440)
        .build();
    dialog.add_button("取消", gtk::ResponseType::Cancel);
    dialog.add_button("确认", gtk::ResponseType::Accept);
    let area = dialog.content_area();
    margins(&area, 20);
    area.set_spacing(12);
    let explanation = label(detail, "dim-label");
    explanation.set_wrap(true);
    area.append(&explanation);
    let entry = gtk::Entry::new();
    entry.set_text(initial);
    area.append(&entry);
    dialog.connect_response(move |dialog, response| {
        if response == gtk::ResponseType::Accept {
            accept(entry.text().to_string());
        }
        dialog.close();
    });
    dialog.present();
}
