mod appearance;
mod credentials;
mod diagnostics;
mod files;
mod model;
mod preferences;
mod quota;
mod terminal_search;
mod transfer;
mod ui;
mod widgets;

use gtk::prelude::*;
use std::{cell::RefCell, rc::Rc};

fn main() -> glib::ExitCode {
    glib::set_prgname(Some("zterm-r"));
    if std::env::var_os("ZTERM_ASKPASS").is_some() {
        return askpass();
    }
    if std::env::args().any(|a| a == "--version") {
        println!("zterm-r {} (Rust / GTK4)", env!("CARGO_PKG_VERSION"));
        return glib::ExitCode::SUCCESS;
    }
    glib::set_application_name("zterm-r");
    let app = gtk::Application::builder()
        .application_id("local.zterm.r")
        .build();
    app.connect_activate(|app| {
        if let Some(window) = app.active_window() {
            window.present();
        } else {
            ui::Workbench::new(app);
        }
    });
    let _watchdog = diagnostics::start();
    app.run()
}

fn askpass() -> glib::ExitCode {
    let prompt = std::env::args().nth(1).unwrap_or_default();
    match credentials::saved_answer(&prompt) {
        Ok(Some(answer)) => {
            println!("{answer}");
            return glib::ExitCode::SUCCESS;
        }
        Err(_) | Ok(None) if std::env::var("ZTERM_SSH_BACKGROUND").as_deref() == Ok("1") => {
            return glib::ExitCode::FAILURE
        }
        _ => {}
    }
    if gtk::init().is_err() {
        return glib::ExitCode::FAILURE;
    }
    let is_confirm = std::env::var("SSH_ASKPASS_PROMPT").as_deref() == Ok("confirm");
    let dialog = gtk::Dialog::builder()
        .title("zterm-r · SSH 登录确认")
        .default_width(480)
        .build();
    dialog.add_button("取消", gtk::ResponseType::Cancel);
    dialog.add_button("确认", gtk::ResponseType::Accept);
    let area = dialog.content_area();
    widgets::margins(&area, 20);
    area.set_spacing(16);
    let text = widgets::label(&prompt, "body");
    text.set_wrap(true);
    text.set_selectable(true);
    area.append(&text);
    let entry = gtk::PasswordEntry::new();
    if !is_confirm {
        area.append(&entry);
    }
    let answer = Rc::new(RefCell::new(None));
    let saved = answer.clone();
    let main_loop = glib::MainLoop::new(None, false);
    let finished = main_loop.clone();
    dialog.connect_response(move |dialog, response| {
        if response == gtk::ResponseType::Accept {
            *saved.borrow_mut() = Some(if is_confirm {
                "yes".to_string()
            } else {
                entry.text().to_string()
            });
        }
        dialog.close();
        finished.quit();
    });
    dialog.present();
    main_loop.run();
    let result = answer.borrow_mut().take();
    if let Some(answer) = result {
        println!("{answer}");
        glib::ExitCode::SUCCESS
    } else {
        glib::ExitCode::FAILURE
    }
}
