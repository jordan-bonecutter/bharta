//! Capturable fixture windows for the isolated headless UI harness.
use gtk::prelude::*;
fn main() {
    assert_eq!(std::env::var("BHARTA_HEADLESS").as_deref(), Ok("1"));
    let app = gtk::Application::builder()
        .application_id("org.bharta.HeadlessFixture")
        .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
        .build();
    app.connect_activate(|app| {
        for title in [
            "Preview fixture — wide window",
            "Preview fixture — second window",
        ] {
            let window = gtk::ApplicationWindow::builder()
                .application(app)
                .title(title)
                .default_width(1400)
                .default_height(900)
                .child(&gtk::Label::new(Some(title)))
                .build();
            window.present();
        }
    });
    app.run_with_args::<&str>(&[]);
}
