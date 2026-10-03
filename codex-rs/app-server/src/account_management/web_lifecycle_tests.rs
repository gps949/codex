use super::*;
use pretty_assertions::assert_eq;

#[test]
fn last_tab_exit_waits_for_refresh_and_other_tabs() -> anyhow::Result<()> {
    let start = Instant::now();
    let lifecycle = WebLifecycle::new(start);
    let first = uuid::Uuid::new_v4().to_string();
    let second = uuid::Uuid::new_v4().to_string();
    lifecycle.renew(&first, start)?;
    lifecycle.renew(&second, start)?;
    lifecycle.leave(&first, start + Duration::from_secs(1))?;
    assert!(!lifecycle.check_idle(start + CLOSE_GRACE));
    lifecycle.leave(&second, start + Duration::from_secs(2))?;
    assert!(!lifecycle.check_idle(start + CLOSE_GRACE));
    let reloaded = uuid::Uuid::new_v4().to_string();
    lifecycle.renew(&reloaded, start + CLOSE_GRACE)?;
    assert!(!lifecycle.check_idle(start + CLOSE_GRACE * 2));
    lifecycle.leave(&reloaded, start + CLOSE_GRACE * 2)?;
    assert!(lifecycle.check_idle(start + CLOSE_GRACE * 3));
    Ok(())
}

#[test]
fn closed_tab_cannot_be_resurrected_by_delayed_heartbeat() -> anyhow::Result<()> {
    let start = Instant::now();
    let lifecycle = WebLifecycle::new(start);
    let tab = uuid::Uuid::new_v4().to_string();
    lifecycle.renew(&tab, start)?;
    lifecycle.leave(&tab, start + Duration::from_secs(1))?;
    assert!(
        lifecycle
            .renew(&tab, start + Duration::from_secs(2))
            .is_err()
    );
    assert!(lifecycle.check_idle(start + Duration::from_secs(1) + CLOSE_GRACE));
    assert!(lifecycle.shutdown().is_cancelled());
    Ok(())
}

#[test]
fn early_tab_exit_precedes_first_heartbeat() -> anyhow::Result<()> {
    let start = Instant::now();
    let lifecycle = WebLifecycle::new(start);
    let tab = uuid::Uuid::new_v4().to_string();
    lifecycle.leave(&tab, start)?;
    assert!(
        lifecycle
            .renew(&tab, start + Duration::from_secs(1))
            .is_err()
    );
    assert!(lifecycle.check_idle(start + CLOSE_GRACE));
    Ok(())
}

#[test]
fn frozen_tab_expires_but_heartbeat_preserves_hidden_tab() -> anyhow::Result<()> {
    let start = Instant::now();
    let lifecycle = WebLifecycle::new(start);
    let tab = uuid::Uuid::new_v4().to_string();
    lifecycle.renew(&tab, start)?;
    let fresh = start + LEASE_LIFETIME - Duration::from_secs(1);
    lifecycle.renew(&tab, fresh)?;
    assert!(!lifecycle.check_idle(start + LEASE_LIFETIME));
    assert!(!lifecycle.check_idle(fresh + LEASE_LIFETIME));
    assert!(lifecycle.check_idle(fresh + LEASE_LIFETIME + CLOSE_GRACE));
    Ok(())
}

#[test]
fn operation_drain_preserves_work_but_does_not_leave_idle_process() -> anyhow::Result<()> {
    let start = Instant::now();
    let lifecycle = WebLifecycle::new(start);
    let tab = uuid::Uuid::new_v4().to_string();
    lifecycle.renew(&tab, start)?;
    let activity = lifecycle.activity()?;
    lifecycle.leave(&tab, start)?;
    assert!(!lifecycle.check_idle(start + CLOSE_GRACE));
    drop(activity);
    assert!(lifecycle.check_idle(start + CLOSE_GRACE));
    Ok(())
}

#[test]
fn unopened_manager_has_bounded_lifetime_and_shutdown_rejects_new_tabs() {
    let start = Instant::now();
    let lifecycle = WebLifecycle::new(start);
    assert!(!lifecycle.check_idle(start + STARTUP_LIFETIME - Duration::from_secs(1)));
    assert!(lifecycle.check_idle(start + STARTUP_LIFETIME));
    assert!(
        lifecycle
            .renew(&uuid::Uuid::new_v4().to_string(), start)
            .is_err()
    );
}

#[test]
fn invalid_and_excessive_tab_ids_do_not_grow_state() -> anyhow::Result<()> {
    let start = Instant::now();
    let lifecycle = WebLifecycle::new(start);
    assert!(lifecycle.renew("not-a-tab", start).is_err());
    for _ in 0..MAX_TABS {
        lifecycle.renew(&uuid::Uuid::new_v4().to_string(), start)?;
    }
    assert!(
        lifecycle
            .renew(&uuid::Uuid::new_v4().to_string(), start)
            .is_err()
    );
    assert_eq!(lifecycle.inner.lock().unwrap().tabs.len(), MAX_TABS);
    Ok(())
}
