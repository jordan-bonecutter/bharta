use super::*;
pub(super) struct Music {
    pub root: gtk::Box,
    cover: gtk::Picture,
    title: gtk::Label,
    artist: gtk::Label,
    previous: gtk::Button,
    toggle: gtk::Button,
    next: gtk::Button,
    art_url: RefCell<String>,
}
impl Music {
    fn placeholder() -> gdk::MemoryTexture {
        let mut pixels = tiny_skia::Pixmap::new(192, 192).unwrap();
        crate::icons::draw(
            &mut pixels,
            crate::icons::Icon::Music,
            64.0,
            64.0,
            64.0,
            1.0,
            tiny_skia::Color::from_rgba8(150, 152, 160, 180),
        );
        texture(&pixels)
    }
    pub fn new(shell: &Rc<Shell>) -> Self {
        let root = column(10);
        let cover = gtk::Picture::for_paintable(&Self::placeholder());
        cover.set_can_shrink(true);
        cover.set_size_request(192, 192);
        cover.set_halign(gtk::Align::Center);
        cover.add_css_class("cover");
        let artwork = row(0);
        artwork.set_halign(gtk::Align::Center);
        artwork.append(&cover);
        root.append(&artwork);
        let title = heading("");
        title.set_max_width_chars(40);
        root.append(&title);
        let artist = label("");
        artist.set_max_width_chars(40);
        artist.add_css_class("dim-label");
        root.append(&artist);
        let controls = row(18);
        controls.set_halign(gtk::Align::Center);
        let previous = icon_button("media-skip-backward-symbolic", "Previous");
        let toggle = icon_button("media-playback-start-symbolic", "Play / pause");
        let next = icon_button("media-skip-forward-symbolic", "Next");
        for (button, control) in [
            (&previous, media::Control::Previous),
            (&toggle, media::Control::Toggle),
            (&next, media::Control::Next),
        ] {
            controls.append(button);
            let weak = Rc::downgrade(shell);
            button.connect_clicked(move |_| {
                if let Some(s) = weak.upgrade() {
                    let player = s.extras.borrow().track.as_ref().map(|t| t.player.clone());
                    if let Some(player) = player {
                        let _ = s.services.media.send(media::Request::Control {
                            panel_id: s.serial.get(),
                            player,
                            control,
                        });
                    }
                }
            });
        }
        root.append(&controls);
        Self {
            root,
            cover,
            title,
            artist,
            previous,
            toggle,
            next,
            art_url: RefCell::new(String::new()),
        }
    }
    pub fn update(&self, extras: &media::Extras) {
        if let Some(track) = &extras.track {
            self.title.set_text(&track.title);
            self.artist.set_text(&track.artist);
            self.previous.set_sensitive(track.can_previous);
            self.next.set_sensitive(track.can_next);
            self.toggle.set_sensitive(track.can_toggle);
            self.toggle.set_icon_name(if track.playing {
                "media-playback-pause-symbolic"
            } else {
                "media-playback-start-symbolic"
            });
            if let Some(art) = extras.artwork.as_ref().filter(|a| a.url == track.art_url) {
                if *self.art_url.borrow() != art.url {
                    let snapshot = gtk::Snapshot::new();
                    texture(&art.pixels).snapshot(&snapshot, 192.0, 192.0);
                    let paintable =
                        snapshot.to_paintable(Some(&gtk::graphene::Size::new(192.0, 192.0)));
                    self.cover.set_paintable(paintable.as_ref());
                    self.art_url.replace(art.url.clone());
                }
            } else if !self.art_url.borrow().is_empty() {
                self.cover.set_paintable(Some(&Self::placeholder()));
                self.art_url.borrow_mut().clear();
            }
        } else {
            self.title.set_text("Nothing playing");
            self.artist.set_text("");
            for b in [&self.previous, &self.toggle, &self.next] {
                b.set_sensitive(false);
            }
        }
    }
}
