//! Explicit recovery from local-daemon incompatibility. Restart is managed-only,
//! requires a fresh confirmation, and is followed by one compatibility check, never a loop.

use crate::AppServerTarget;
use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;
use crate::bottom_pane::BottomPaneView;
use crate::bottom_pane::ListSelectionView;
use crate::bottom_pane::SelectionItem;
use crate::bottom_pane::SelectionViewParams;
use crate::daemon_startup;
use crate::daemon_startup::CompatibilityError;
use crate::keymap::RuntimeKeymap;
use crate::legacy_core::config::Config;
use crate::render::renderable::ColumnRenderable;
use crate::render::renderable::Renderable;
use crate::startup_draft::StartupDraft;
use crossterm::event::KeyCode;
use crossterm::event::KeyEventKind;
use crossterm::event::KeyModifiers;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Wrap;
use std::io;
use std::io::IsTerminal;
use tokio::sync::mpsc::unbounded_channel;
use tokio_stream::StreamExt;

pub(super) enum Recovery {
    UseDaemon,
    Independent(String),
    Unavailable(String),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum RecoveryMode {
    StartIndependently,
    OfferIndependent,
    KeepBackend,
}

pub(super) async fn check(
    startup: &mut StartupDraft,
    target: &AppServerTarget,
    config: &Config,
    cli_kv_overrides: &[(String, toml::Value)],
    mut managed_daemon: bool,
    mode: RecoveryMode,
) -> io::Result<Recovery> {
    let mut compatibility = startup
        .run_until(daemon_startup::compatibility_warning(
            target,
            config,
            cli_kv_overrides,
        ))
        .await?;
    // A daemon can accept the socket while it is still finishing startup or restarting.
    if matches!(
        &compatibility,
        Ok(Some(_))
            | Err(CompatibilityError {
                restart_features: None,
                ..
            })
    ) {
        compatibility = startup
            .run_until(async {
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                daemon_startup::compatibility_warning(target, config, cli_kv_overrides).await
            })
            .await?;
    }
    let issue = match compatibility {
        Ok(None) => return Ok(Recovery::UseDaemon),
        Ok(Some(warning)) => return Ok(Recovery::Unavailable(warning)),
        Err(issue) => issue,
    };
    // Preserve explicit invocation settings without changing shared daemon state.
    if mode == RecoveryMode::StartIndependently && issue.restart_features.is_some() {
        return Ok(Recovery::Independent(format!(
            "Independent session: {}.",
            issue.reason
        )));
    }
    if !(io::stdin().is_terminal() && io::stdout().is_terminal()) {
        return Err(io::Error::other(issue));
    }
    if !managed_daemon && issue.restart_features.is_some() {
        managed_daemon = startup
            .run_until(codex_app_server_daemon::run(
                codex_app_server_daemon::LifecycleCommand::Version,
            ))
            .await?
            .is_ok_and(|status| status.backend.is_some());
    }
    let allow_independent = mode != RecoveryMode::KeepBackend;
    if !allow_independent && (!managed_daemon || issue.restart_features.is_none()) {
        return Err(io::Error::other(issue));
    }
    startup.flush_pending_events().await?;
    let keymap = RuntimeKeymap::from_config(&config.tui_keymap).map_err(io::Error::other)?;
    let mut view = recovery_view(&issue, managed_daemon, allow_independent, &keymap);
    let mut chord_matcher = crate::keymap::KeyChordMatcher::default();
    let tui = startup.tui_mut();
    tui.discard_pending_input_before_interactive_screen()?;
    let selection = {
        let events = tui.event_stream();
        tokio::pin!(events);
        loop {
            let height = view.desired_height(tui.terminal.size()?.width);
            let hud = chord_matcher.guide(&keymap.chords, &tui.frame_requester());
            tui.draw_with_shortcut_hud(height, hud, |frame| {
                view.render(frame.area(), frame.buffer_mut())
            })?;
            let Some(event) = events.next().await else {
                break None;
            };
            tui.screen_size_for_event(&event)?;
            if tui.dismiss_shortcut_on_input(&event, &mut chord_matcher) {
                continue;
            }
            if let crate::tui::TuiEvent::Key(key) = event
                && matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat)
            {
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    && matches!(key.code, KeyCode::Char('c' | 'd'))
                {
                    break None;
                }
                let key = match chord_matcher.advance(
                    key,
                    &keymap.chords,
                    crate::keymap::KeymapContextSet::new(crate::keymap::KeymapContext::List),
                ) {
                    crate::keymap::KeyChordMatch::PassThrough => key,
                    crate::keymap::KeyChordMatch::Completed(key) => key,
                    crate::keymap::KeyChordMatch::Pending(_)
                    | crate::keymap::KeyChordMatch::Cancelled => {
                        tui.frame_requester().schedule_frame();
                        continue;
                    }
                    crate::keymap::KeyChordMatch::Ignored => continue,
                };
                view.handle_key_event(key);
            }
            if view.is_complete() {
                break view.take_last_selected_index();
            }
        }
    };
    tui.terminal.clear()?;
    match (selection, issue.restart_features.as_ref()) {
        (Some(0), _) if allow_independent => Ok(Recovery::Independent(format!(
            "Independent session: {}.",
            issue.reason
        ))),
        (Some(1), Some(features)) if managed_daemon => {
            tui.with_restored(crate::tui::TerminalHandoff::Restore, || async {
                crossterm::terminal::disable_raw_mode()?;
                codex_app_server_daemon::restart_with_features(features)
                    .await
                    .map_err(|err| {
                        io::Error::other(format!("{err:#}\n{}", daemon_startup::FAILURE_HINT))
                    })
            })
            .await?;
            startup
                .run_until(daemon_startup::compatibility_warning(
                    target,
                    config,
                    cli_kv_overrides,
                ))
                .await?
                .map(|warning| warning.map_or(Recovery::UseDaemon, Recovery::Unavailable))
                .map_err(io::Error::other)
        }
        (Some(_) | None, _) => Err(io::Error::other(issue)),
    }
}

fn recovery_view(
    issue: &CompatibilityError,
    managed_daemon: bool,
    allow_independent: bool,
    keymap: &RuntimeKeymap,
) -> ListSelectionView {
    let can_restart = managed_daemon && issue.restart_features.is_some();
    let mut header = ColumnRenderable::new();
    header.push(Line::from(
        if issue.restart_features.is_some() {
            "Background server has incompatible feature settings"
        } else {
            "Cannot use the background server"
        }
        .bold(),
    ));
    header.push(Paragraph::new(issue.reason.clone()).wrap(Wrap { trim: false }));
    if let Some(features) = &issue.restart_features {
        header.push(Paragraph::new(
            "These settings persist and can disable functionality for other clients. Restart may interrupt active or queued work."
        ).wrap(Wrap { trim: false }));
        header.push(Line::from(
            "Restart will use these shared feature settings:",
        ));
        let mut remaining = features.iter();
        for (name, enabled) in remaining.by_ref().take(3) {
            header.push(Line::from(format!("  {name} = {enabled}").dim()));
        }
        let count = remaining.len();
        if count > 0 {
            header.push(Line::from(format!("  and {count} more").dim()));
        }
    }
    let (tx, _rx) = unbounded_channel::<AppEvent>();
    ListSelectionView::new(
        SelectionViewParams {
            header: Box::new(header),
            initial_selected_idx: Some(2),
            items: vec![
                SelectionItem {
                    name: "Run without daemon this time".to_string(),
                    dismiss_on_select: true,
                    is_disabled: !allow_independent,
                    disabled_reason: (!allow_independent)
                        .then(|| "This request needs the shared background server.".to_string()),
                    ..Default::default()
                },
                SelectionItem {
                    name: "Restart with these settings".to_string(),
                    dismiss_on_select: true,
                    require_explicit_confirmation: true,
                    is_disabled: !can_restart,
                    disabled_reason: if !managed_daemon {
                        Some("This server is not managed by Codex.".to_string())
                    } else if issue.restart_features.is_none() {
                        Some("Restart cannot resolve this compatibility check.".to_string())
                    } else {
                        None
                    },
                    ..Default::default()
                },
                SelectionItem {
                    name: "Cancel".to_string(),
                    dismiss_on_select: true,
                    ..Default::default()
                },
            ],
            ..SelectionViewParams::picker()
        },
        AppEventSender::new(tx),
        keymap.list.clone(),
    )
}

#[cfg(test)]
#[path = "daemon_recovery_tests.rs"]
mod tests;
