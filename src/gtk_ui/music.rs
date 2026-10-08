use super::*;
use crate::audio_meter;
use std::sync::{Arc, Mutex};
struct Equalizer {
    area: gtk::DrawingArea,
    levels: Arc<Mutex<[f64; 7]>>,
    capturing: Rc<Cell<bool>>,
}
pub(super) struct Music {
    pub root: gtk::Box,
    cover: gtk::Picture,
    title: gtk::Label,
    artist: gtk::Label,
    previous: gtk::Button,
    toggle: gtk::Button,
    next: gtk::Button,
    art_url: RefCell<String>,
    equalizer: gtk::DrawingArea,
    levels: Arc<Mutex<[f64; 7]>>,
    capture: RefCell<Option<audio_meter::Capture>>,
    capturing: Rc<Cell<bool>>,
}
impl Music {
    fn equalizer() -> Equalizer {
        let area = gtk::DrawingArea::new();
        area.set_content_width(40);
        area.set_content_height(18);
        area.set_valign(gtk::Align::Center);
        area.set_can_target(false);
        let levels = Arc::new(Mutex::new([0.0; 7]));
        let capturing = Rc::new(Cell::new(false));
        let measured = levels.clone();
        area.set_draw_func(move |area, cr, _, height| {
            #[allow(deprecated)]
            let color = area.style_context().color();
            cr.set_source_rgba(
                color.red() as f64,
                color.green() as f64,
                color.blue() as f64,
                0.8,
            );
            cr.set_line_width(3.0);
            cr.set_line_cap(gtk::cairo::LineCap::Round);
            let levels = measured.lock().map(|v| *v).unwrap_or([0.0; 7]);
            for (i, level) in levels.iter().enumerate() {
                let x = 2.0 + i as f64 * 6.0;
                let bottom = height as f64 - 2.0;
                cr.move_to(x, bottom);
                cr.line_to(x, bottom - (1.0 + level * 12.0));
                let _ = cr.stroke();
            }
        });
        let is_capturing = capturing.clone();
        area.add_tick_callback(move |area, _| {
            if area.is_mapped() && is_capturing.get() {
                area.queue_draw();
            }
            glib::ControlFlow::Continue
        });
        Equalizer {
            area,
            levels,
            capturing,
        }
    }
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
        title.set_hexpand(true);
        let title_row = row(12);
        let equalizer = Self::equalizer();
        title_row.append(&title);
        title_row.append(&equalizer.area);
        root.append(&title_row);
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
            equalizer: equalizer.area,
            levels: equalizer.levels,
            capture: RefCell::new(None),
            capturing: equalizer.capturing,
        }
    }
    pub fn update(&self, extras: &media::Extras) {
        let playing = extras.track.as_ref().is_some_and(|track| track.playing);
        if playing && self.capture.borrow().is_none() {
            let capture = audio_meter::Capture::start(self.levels.clone()).ok();
            self.capturing.set(capture.is_some());
            self.capture.replace(capture);
        } else if !playing {
            self.capturing.set(false);
            self.capture.borrow_mut().take();
            if let Ok(mut levels) = self.levels.lock() {
                *levels = [0.0; 7];
            }
        }
        self.equalizer.queue_draw();
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
