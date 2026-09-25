slint::include_modules!();

fn main() {
    // Request fullscreen so the compositor gives us the actual screen size
    std::env::set_var("SLINT_FULLSCREEN", "1");
    // Disable HiDPI scaling — use physical pixels directly
    std::env::set_var("SLINT_SCALE_FACTOR", "1");

    let app = App::new().unwrap();

    // Wire up the increment button
    let weak = app.as_weak();
    app.on_increment(move || {
        let app = weak.unwrap();
        let new_val = app.get_counter() + 1;
        app.set_counter(new_val);
        app.set_status_text(format!("Count: {new_val}").into());
    });

    // Wire up the reset button
    let weak = app.as_weak();
    app.on_reset(move || {
        let app = weak.unwrap();
        app.set_counter(0);
        app.set_status_text("Counter reset!".into());
    });

    app.run().unwrap();
}
