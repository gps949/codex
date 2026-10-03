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
