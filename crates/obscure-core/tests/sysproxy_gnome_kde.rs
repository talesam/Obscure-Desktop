//! System proxy with GNOME + a broken KDE writer: GNOME is applied, the
//! backup is persisted, and `restore` brings GNOME back. Runs against an
//! in-memory GSettings backend and a fake `kwriteconfig6`, so the real
//! desktop is never touched. One test per binary: it sets process env.

mod common;

use obscure_core::sysproxy::SysProxy;

#[tokio::test]
async fn kde_failure_keeps_gnome_and_backup() {
    let Some(env) = common::FakeDesktop::new(true) else {
        eprintln!("org.gnome.system.proxy schema not installed; skipping");
        return;
    };
    let sp = SysProxy::new(env.state_file());
    let before = common::gnome_mode();
    assert_ne!(before, "manual");

    sp.apply(2080)
        .await
        .expect("GNOME applied despite KDE failure");
    assert!(sp.is_dirty(), "backup must be on disk");
    assert_eq!(common::gnome_mode(), "manual");

    sp.restore().await.expect("restore");
    assert!(!sp.is_dirty());
    assert_eq!(common::gnome_mode(), before);
}
