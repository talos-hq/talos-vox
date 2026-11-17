//! `voxtype context [--watch]`: what the focus probe reports right now.

use voxtype::context;

fn line(probe: Option<&dyn context::FocusProbe>) -> String {
    let app = probe.and_then(|p| p.snapshot());
    serde_json::json!({
        "probe": probe.map(|p| p.name()),
        "app_id": app.as_ref().map(|a| a.app_id.as_str()),
        "title": app.as_ref().and_then(|a| a.title.as_deref()),
    })
    .to_string()
}

pub(crate) fn run_context(watch: bool) {
    let probe = context::detect();
    let probe = probe.as_deref();
    let mut last = line(probe);
    println!("{last}");
    if !watch || probe.is_none() {
        return;
    }
    loop {
        std::thread::sleep(std::time::Duration::from_millis(500));
        let next = line(probe);
        if next != last {
            println!("{next}");
            last = next;
        }
    }
}
