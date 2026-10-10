//! KDE-only desktop whose `kwriteconfig6` fails halfway: `apply` returns the
//! error and rolls back what it already wrote, leaving the state clean.

mod common;

use obscure_core::sysproxy::SysProxy;

#[tokio::test]
async fn kde_only_failure_rolls_back() {
    let env = common::FakeDesktop::new(false).expect("fake desktop");
    let sp = SysProxy::new(env.state_file());

    assert!(sp.apply(2080).await.is_err());
    assert!(!sp.is_dirty(), "rolled back, nothing left to restore");
    let calls = env.kwrite_calls();
    let last_type = calls
        .lines()
        .rfind(|l| l.contains("--key ProxyType"))
        .expect("ProxyType written");
    assert!(last_type.ends_with(" 0"), "ProxyType restored: {calls}");
}
