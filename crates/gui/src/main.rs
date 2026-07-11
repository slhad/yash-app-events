fn main() -> eframe::Result {
    let options = eframe::NativeOptions::default();
    eframe::run_native(
        "yash-app-events",
        options,
        Box::new(|creation| Ok(Box::new(yash_app_events::App::new(creation)))),
    )
}
