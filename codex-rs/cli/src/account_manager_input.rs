use std::io::Write;

pub(super) async fn prompt(label: &str, default: &str) -> anyhow::Result<String> {
    print!(
        "{label}{}: ",
        if default.is_empty() {
            String::new()
        } else {
            format!(" [{default}]")
        }
    );
    std::io::stdout().flush()?;
    let default = default.to_owned();
    tokio::task::spawn_blocking(move || {
        let mut value = String::new();
        anyhow::ensure!(
            std::io::stdin().read_line(&mut value)? != 0,
            "Terminal input closed"
        );
        Ok(if value.trim().is_empty() {
            default
        } else {
            value.trim().to_string()
        })
    })
    .await?
}

pub(super) async fn secret_prompt() -> anyhow::Result<String> {
    print!("API key (hidden): ");
    std::io::stdout().flush()?;
    tokio::task::spawn_blocking(|| {
        crossterm::terminal::enable_raw_mode()?;
        struct Restore;
        impl Drop for Restore {
            fn drop(&mut self) {
                let _ = crossterm::terminal::disable_raw_mode();
            }
        }
        let _restore = Restore;
        let mut result = String::new();
        loop {
            match crossterm::event::read()? {
                crossterm::event::Event::Key(key)
                    if key.kind != crossterm::event::KeyEventKind::Release =>
                {
                    match key.code {
                        crossterm::event::KeyCode::Enter => break,
                        crossterm::event::KeyCode::Esc => anyhow::bail!("Key entry cancelled"),
                        crossterm::event::KeyCode::Char('c' | 'd')
                            if key
                                .modifiers
                                .contains(crossterm::event::KeyModifiers::CONTROL) =>
                        {
                            anyhow::bail!("Key entry cancelled")
                        }
                        crossterm::event::KeyCode::Char('u')
                            if key
                                .modifiers
                                .contains(crossterm::event::KeyModifiers::CONTROL) =>
                        {
                            result.clear()
                        }
                        crossterm::event::KeyCode::Backspace => {
                            result.pop();
                        }
                        crossterm::event::KeyCode::Char(ch)
                            if !ch.is_control()
                                && !key
                                    .modifiers
                                    .contains(crossterm::event::KeyModifiers::CONTROL) =>
                        {
                            anyhow::ensure!(
                                result.len() + ch.len_utf8() <= 16384,
                                "API key is too long"
                            );
                            result.push(ch);
                        }
                        _ => {}
                    }
                }
                crossterm::event::Event::Paste(paste) => {
                    anyhow::ensure!(
                        result.len() + paste.len() <= 16384 && !paste.chars().any(char::is_control),
                        "Invalid API key paste"
                    );
                    result.push_str(&paste);
                }
                _ => {}
            }
        }
        println!();
        Ok(result)
    })
    .await?
}

/// Keeps the displayed account numbers stable while device-login progress changes.
pub(super) async fn dashboard_prompt(
    manager: &std::sync::Arc<codex_app_server::account_management::AccountManager>,
    jobs: &[codex_app_server::account_management::LoginProgress],
) -> anyhow::Result<String> {
    use crossterm::event::Event;
    use crossterm::event::EventStream;
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEventKind;
    use crossterm::event::KeyModifiers;
    use futures::StreamExt;
    use std::io::IsTerminal;
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return prompt("Choose", "").await;
    }
    crossterm::terminal::enable_raw_mode()?;
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            let _ = crossterm::terminal::disable_raw_mode();
        }
    }
    let _restore = Restore;
    let mut events = EventStream::new();
    let mut value = String::new();
    let mut previous = serde_json::to_string(jobs)?;
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(2));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    print!("Choose: ");
    std::io::stdout().flush()?;
    loop {
        tokio::select! {
            event = events.next() => {
                let event = event.ok_or_else(|| anyhow::anyhow!("Terminal input closed"))??;
                match event {
                    Event::Key(key) if key.kind != KeyEventKind::Release => {
                        match key.code {
                            KeyCode::Enter => { print!("\r\n"); return Ok(value.trim().to_string()); }
                            KeyCode::Esc => return Ok("q".into()),
                            KeyCode::Char('c' | 'd') if key.modifiers.contains(KeyModifiers::CONTROL) => return Ok("q".into()),
                            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => value.clear(),
                            KeyCode::Backspace => { value.pop(); }
                            KeyCode::Char(ch) if !ch.is_control() && !key.modifiers.contains(KeyModifiers::CONTROL) && value.len() < 256 => value.push(ch),
                            _ => {}
                        }
                    }
                    Event::Paste(paste) if value.len() + paste.len() <= 256 && !paste.chars().any(char::is_control) => value.push_str(&paste),
                    _ => {}
                }
            }
            _ = tick.tick(), if !jobs.is_empty() => {
                let latest = manager.login_progress().await;
                let signature = serde_json::to_string(&latest)?;
                if signature != previous {
                    crossterm::execute!(std::io::stdout(), crossterm::cursor::MoveToColumn(0), crossterm::terminal::Clear(crossterm::terminal::ClearType::CurrentLine))?;
                    for (index, original) in jobs.iter().enumerate() {
                        let Some(job) = latest.iter().find(|job| job.operation_id == original.operation_id) else { continue; };
                        print!("\r\nLogin {} · {}", index + 1, super::clean(&job.message));
                        if let Some(url) = &job.verification_url { print!("\r\nOpen: {}", super::clean(url)); }
                        if let Some(code) = &job.user_code { print!("\r\nVerification code: {}", super::clean(code)); }
                    }
                    print!("\r\nPress Enter to update the account list.\r\n");
                    previous = signature;
                }
            }
        }
        crossterm::execute!(
            std::io::stdout(),
            crossterm::cursor::MoveToColumn(0),
            crossterm::terminal::Clear(crossterm::terminal::ClearType::CurrentLine)
        )?;
        let columns = crossterm::terminal::size().map_or(80, |(columns, _)| usize::from(columns));
        let mut visible = super::clean(&value);
        let budget = columns.saturating_sub(10);
        while unicode_width::UnicodeWidthStr::width(visible.as_str()) > budget {
            visible.remove(0);
        }
        print!("Choose: {visible}");
        std::io::stdout().flush()?;
    }
}
