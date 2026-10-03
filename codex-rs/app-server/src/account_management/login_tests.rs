use super::*;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn manager_login_shutdown_waits_for_credential_job_and_prevents_new_reservations()
-> anyhow::Result<()> {
    let home = tempfile::TempDir::new()?;
    let config = codex_core::config::ConfigBuilder::default()
        .codex_home(home.path().to_path_buf())
        .build()
        .await?;
    let manager = AccountManager::new(config);
    let cancel = CancellationToken::new();
    let task_cancel = cancel.clone();
    let (started, start) = tokio::sync::oneshot::channel();
    let (release, released) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        task_cancel.cancelled().await;
        started.send(()).unwrap();
        released.await.unwrap();
    });
    manager.logins.lock().await.insert(
        "fixture-job".into(),
        LoginJob {
            progress: LoginProgress {
                operation_id: "fixture-job".into(),
                profile_id: Some("fixture-profile".into()),
                verification_url: None,
                user_code: None,
                status: "waiting".into(),
                message: "Saving login".into(),
            },
            cancel,
            task: Some(task),
        },
    );
    let shutting_down = Arc::clone(&manager);
    let mut shutdown = tokio::spawn(async move { shutting_down.shutdown_logins().await });
    start.await?;
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), &mut shutdown)
            .await
            .is_err()
    );
    assert!(
        manager
            .start_login(/*profile_id*/ None, Some("Late login".into()))
            .await
            .is_err()
    );
    assert_eq!(manager.store().load_profile_records()?, Vec::new());
    release.send(()).unwrap();
    shutdown.await?;
    Ok(())
}
