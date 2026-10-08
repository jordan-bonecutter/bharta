use gtk::{gdk, prelude::*};

pub fn column(spacing: i32) -> gtk::Box {
    gtk::Box::new(gtk::Orientation::Vertical, spacing)
}
pub fn row(spacing: i32) -> gtk::Box {
    gtk::Box::new(gtk::Orientation::Horizontal, spacing)
}
pub fn label(text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.set_xalign(0.0);
    label.set_max_width_chars(42);
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    label
}
pub fn heading(text: &str) -> gtk::Label {
    let label = label(text);
    label.add_css_class("heading");
    label
}
pub fn icon_button(icon: &str, tooltip: &str) -> gtk::Button {
    let button = gtk::Button::from_icon_name(icon);
    button.set_tooltip_text(Some(tooltip));
    button.add_css_class("flat");
    button
}
pub fn clear(container: &gtk::Box) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
}
pub fn scroll(child: &impl IsA<gtk::Widget>, max_height: i32) -> gtk::ScrolledWindow {
    gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .propagate_natural_height(true)
        .max_content_height(max_height)
        .child(child)
        .build()
}
pub fn texture(pixels: &tiny_skia::Pixmap) -> gdk::MemoryTexture {
    gdk::MemoryTexture::new(
        pixels.width() as i32,
        pixels.height() as i32,
        gdk::MemoryFormat::R8g8b8a8Premultiplied,
        &gtk::glib::Bytes::from_owned(pixels.data().to_vec()),
        pixels.width() as usize * 4,
    )
}
pub fn section(text: &str) -> gtk::Label {
    let label = label(text);
    label.add_css_class("section-title");
    label
}
pub fn error_label() -> gtk::Label {
    let label = label("");
    label.set_wrap(true);
    label.set_ellipsize(gtk::pango::EllipsizeMode::None);
    label.set_max_width_chars(40);
    label.add_css_class("error");
    label.set_visible(false);
    label
}
pub fn set_error(label: &gtk::Label, error: &str) {
    label.set_text(error);
    label.set_visible(!error.is_empty());
}

pub fn status_button(button: &gtk::Button, icon: &str, text: &str) {
    if let Some(content) = button.child().and_downcast::<gtk::Box>() {
        if let Some(image) = content.first_child().and_downcast::<gtk::Image>() {
            image.set_icon_name(Some(icon));
        }
        if let Some(label) = content.last_child().and_downcast::<gtk::Label>() {
            label.set_text(text);
        }
    } else {
        let content = row(6);
        let image = gtk::Image::from_icon_name(icon);
        content.append(&image);
        let label = label(text);
        label.set_max_width_chars(22);
        content.append(&label);
        button.set_child(Some(&content));
    }
}

pub fn draw_battery(area: &gtk::DrawingArea, level: u8, charging: bool) {
    area.set_draw_func(move |area, cr, _, h| {
        #[allow(deprecated)]
        let color = area.style_context().color();
        cr.set_source_rgba(
            color.red() as f64,
            color.green() as f64,
            color.blue() as f64,
            0.7,
        );
        let y = (h as f64 - 10.0) / 2.0;
        cr.set_line_width(1.0);
        cr.rectangle(0.5, y + 0.5, 21.0, 10.0);
        let _ = cr.stroke();
        cr.rectangle(23.0, y + 3.0, 2.0, 4.0);
        let _ = cr.fill();
        if charging {
            cr.set_source_rgb(0.19, 0.65, 0.37);
            cr.move_to(13.0, y - 1.0);
            cr.line_to(7.0, y + 6.0);
            cr.line_to(11.0, y + 6.0);
            cr.line_to(9.0, y + 12.0);
            cr.line_to(16.0, y + 4.0);
            cr.line_to(12.0, y + 4.0);
            cr.close_path();
        } else {
            cr.set_source_rgba(
                color.red() as f64,
                color.green() as f64,
                color.blue() as f64,
                1.0,
            );
            cr.rectangle(2.5, y + 2.5, 17.0 * level as f64 / 100.0, 6.0);
        }
        let _ = cr.fill();
    });
}

pub fn text_button(text: &str) -> gtk::Button {
    let button = gtk::Button::new();
    button.set_child(Some(&label(text)));
    button
}
