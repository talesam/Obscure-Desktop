//! Shared tokio runtime for all I/O (Xray process, HTTP, D-Bus). UI work
//! stays on the GLib main loop; results cross over via channels or by
//! awaiting `JoinHandle`s from `glib::spawn_future_local`.

use std::sync::OnceLock;

use tokio::runtime::Runtime;

pub fn runtime() -> &'static Runtime {
    static RUNTIME: OnceLock<Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .thread_name("obscure-io")
            .build()
            .expect("tokio runtime")
    })
}
