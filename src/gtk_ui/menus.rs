use super::*;
impl Shell {
    pub(super) fn apps_menu(
        self: &Rc<Self>,
        body: &gtk::Box,
        id: u64,
        apps: Vec<crate::launcher::Entry>,
    ) {
        clear(body);
        body.append(&heading("Applications"));
        let search = gtk::SearchEntry::new();
        search.set_placeholder_text(Some("Search applications"));
        body.append(&search);
        let weak = Rc::downgrade(self);
        search.connect_stop_search(move |_| {
            if let Some(s) = weak.upgrade() {
                s.close();
            }
        });
        let list = gtk::ListBox::new();
        list.set_selection_mode(gtk::SelectionMode::Single);
        body.append(&scroll(&list, 420));
        let apps = Rc::new(apps);
        for app in apps.iter() {
            let l = label(&app.name);
            l.set_margin_start(10);
            l.set_margin_end(10);
            l.set_margin_top(4);
            l.set_margin_bottom(4);
            list.append(&l);
        }
        let keys = gtk::EventControllerKey::new();
        let rows = list.clone();
        keys.connect_key_pressed(move |_, key, _, _| {
            let step = if key == gdk::Key::Down {
                1
            } else if key == gdk::Key::Up {
                -1
            } else {
                return glib::Propagation::Proceed;
            };
            let mut index = rows.selected_row().map(|r| r.index()).unwrap_or(-1) + step;
            while let Some(row) = rows.row_at_index(index) {
                if row.is_visible() {
                    rows.select_row(Some(&row));
                    row.grab_focus();
                    break;
                }
                index += step;
            }
            glib::Propagation::Stop
        });
        search.add_controller(keys);
        let entries = apps.clone();
        let rows = list.clone();
        let weak = Rc::downgrade(self);
        // SearchEntry delays search-changed; this in-memory filter should run
        // on every edit, including paste and deletion, without a debounce.
        search.connect_changed(move |search| {
            if let Some(s) = weak.upgrade()
                && let Some(m) = s.menu.borrow().as_ref().filter(|m| m.id == id)
            {
                m.pinned.set(!search.text().is_empty());
            }
            let query = search.text().to_lowercase();
            for (i, app) in entries.iter().enumerate() {
                if let Some(row) = rows.row_at_index(i as i32) {
                    row.set_visible(app.search.contains(&query));
                }
            }
            let mut child = rows.first_child();
            while let Some(row) = child {
                if row.is_visible() {
                    rows.select_row(row.downcast_ref::<gtk::ListBoxRow>());
                    break;
                }
                child = row.next_sibling();
            }
        });
        let weak = Rc::downgrade(self);
        list.connect_row_activated(move |_, row| {
            if let Some(s) = weak.upgrade() {
                s.services
                    .launch(id, apps[row.index() as usize].path.clone());
            }
        });
        let rows = list.clone();
        search.connect_activate(move |_| {
            if let Some(row) = rows.selected_row() {
                rows.emit_by_name::<()>("row-activated", &[&row]);
            }
        });
        list.select_row(list.row_at_index(0).as_ref());
        search.connect_map(|search| {
            search.grab_focus();
        });
        search.grab_focus();
    }
    pub(super) fn network_menu(
        self: &Rc<Self>,
        body: &gtk::Box,
        id: u64,
        snapshot: crate::network::Snapshot,
    ) {
        clear(body);
        let top = row(12);
        let title = heading("Wi-Fi");
        title.set_hexpand(true);
        top.append(&title);
        let switch = gtk::Switch::new();
        switch.set_active(snapshot.enabled);
        switch.set_valign(gtk::Align::Center);
        top.append(&switch);
        let refresh = icon_button("view-refresh-symbolic", "Scan networks");
        top.append(&refresh);
        body.append(&top);
        let weak = Rc::downgrade(self);
        switch.connect_state_set(move |_, on| {
            if let Some(s) = weak.upgrade() {
                s.services.network(id, services::Job::Radio(on));
            }
            glib::Propagation::Proceed
        });
        let weak = Rc::downgrade(self);
        refresh.connect_clicked(move |_| {
            if let Some(s) = weak.upgrade() {
                s.services.network(id, services::Job::Scan(true));
            }
        });
        let list = column(5);
        body.append(&scroll(&list, 360));
        if snapshot.networks.is_empty() {
            list.append(&label(if snapshot.enabled {
                "No networks found"
            } else {
                "Wi-Fi is off"
            }));
        }
        for n in snapshot.networks {
            let b = gtk::Button::new();
            let content = row(12);
            let name = label(&format!("{}{}", if n.active { "✓  " } else { "" }, n.ssid));
            name.set_hexpand(true);
            name.set_max_width_chars(32);
            let signal = label(&format!("{}%", n.signal));
            signal.add_css_class("dim-label");
            content.append(&name);
            content.append(&signal);
            b.set_child(Some(&content));
            list.append(&b);
            let weak = Rc::downgrade(self);
            let body = body.clone();
            b.connect_clicked(move |_| {
                if let Some(s) = weak.upgrade() {
                    s.window.set_keyboard_mode(KeyboardMode::Exclusive);
                    s.window.queue_draw();
                    clear(&body);
                    body.append(&heading(&n.ssid));
                    let back = text_button("Back to networks");
                    body.append(&back);
                    let weak = Rc::downgrade(&s);
                    back.connect_clicked(move |_| {
                        if let Some(s) = weak.upgrade() {
                            s.services.network(id, services::Job::Scan(false));
                        }
                    });
                    let password = gtk::PasswordEntry::new();
                    password.set_show_peek_icon(true);
                    password.set_placeholder_text(Some("Password"));
                    if !n.active && !n.security.is_empty() {
                        body.append(&password);
                    }
                    let connect = text_button(if n.active { "Disconnect" } else { "Connect" });
                    connect.add_css_class("suggested-action");
                    body.append(&connect);
                    let weak = Rc::downgrade(&s);
                    let n = n.clone();
                    connect.connect_clicked(move |b| {
                        b.set_sensitive(false);
                        if let Some(s) = weak.upgrade() {
                            if let Some(m) = s.menu.borrow().as_ref() {
                                m.pinned.set(true);
                            }
                            s.services.network(
                                id,
                                if n.active {
                                    services::Job::Disconnect(n.device.clone())
                                } else {
                                    services::Job::Connect(n.clone(), password.text().to_string())
                                },
                            );
                        }
                    });
                    if let Some(m) = s.menu.borrow().as_ref() {
                        m.pinned.set(true);
                    }
                }
            });
        }
        if let Some(m) = self.menu.borrow().as_ref() {
            m.pinned.set(false);
        }
    }
    pub(super) fn session_menu(self: &Rc<Self>, body: &gtk::Box, pinned: Rc<Cell<bool>>) {
        body.append(&heading("Session"));
        let lock = gtk::Button::with_label("Lock screen");
        body.append(&lock);
        let weak = Rc::downgrade(self);
        lock.connect_clicked(move |_| {
            let path = std::env::var("HOME").unwrap_or_default() + "/.local/bin/lock-session";
            let command = if std::path::Path::new(&path).exists() {
                format!("exec {}", serde_json::to_string(&path).unwrap())
            } else {
                "exec swaylock".into()
            };
            let _ = status::ipc(0, &command);
            if let Some(s) = weak.upgrade() {
                s.close();
            }
        });
        let logout = gtk::Button::with_label("Log out…");
        body.append(&logout);
        let confirm = Cell::new(false);
        logout.connect_clicked(move |b| {
            if confirm.replace(true) {
                let _ = status::ipc(0, "exit");
            } else {
                pinned.set(true);
                b.set_label("Confirm log out");
            }
        });
    }
    pub(super) fn preview_menu(
        &self,
        body: &gtk::Box,
        snapshot: crate::workspace_preview::Snapshot,
    ) {
        clear(body);
        body.append(&heading(&format!("Workspace {}", snapshot.name)));
        let canvas = super::preview::canvas(snapshot.rect, snapshot.windows);
        body.append(&canvas);
        if !snapshot.message.is_empty() {
            body.append(&label(&snapshot.message));
        }
    }
}
