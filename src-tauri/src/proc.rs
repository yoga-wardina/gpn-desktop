//! Child-process helper: on Windows, GUI apps spawning console tools (tasklist, netstat,
//! powershell) would flash a console window every time, so create them hidden.

use std::process::Command;

pub fn command(program: &str) -> Command {
    let mut c = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    c
}

/// Limit a file to the given principals (icacls on Windows, chmod 600 elsewhere). Best effort.
pub fn restrict_file(path: &std::path::Path, principals: &[&str]) {
    #[cfg(windows)]
    {
        let mut c = command("icacls");
        c.arg(path).args(["/inheritance:r", "/grant:r"]);
        for p in principals.iter().filter(|p| !p.is_empty()) {
            c.arg(format!("{p}:F"));
        }
        let _ = c.output();
    }
    #[cfg(not(windows))]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = principals;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
}

pub fn current_user() -> String {
    let user = std::env::var("USERNAME").unwrap_or_default();
    match std::env::var("USERDOMAIN") {
        Ok(d) if !d.is_empty() && !user.is_empty() => format!("{d}\\{user}"),
        _ => user,
    }
}

/// Run a program to completion on a blocking thread. `strict`: Err on spawn failure, timeout or
/// non-zero exit; otherwise stdout is returned whatever the exit code.
pub async fn run(program: &str, args: &[&str], stdin: Option<String>, strict: bool) -> Result<String, String> {
    let program = program.to_string();
    let name = program.clone();
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    let task = tauri::async_runtime::spawn_blocking(move || -> Result<String, String> {
        use std::io::Write;
        use std::process::Stdio;
        let mut c = command(&program);
        c.args(&args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = c.spawn().map_err(|e| format!("{program}: {e}"))?;
        if let Some(mut si) = child.stdin.take() {
            if let Some(text) = stdin {
                let _ = si.write_all(text.as_bytes());
            }
        }
        let out = child.wait_with_output().map_err(|e| format!("{program}: {e}"))?;
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        if strict && !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            let msg = if err.trim().is_empty() { stdout.trim().to_string() } else { err.trim().to_string() };
            return Err(format!("{program} exited {}: {}", out.status.code().unwrap_or(-1), msg.chars().take(500).collect::<String>()));
        }
        Ok(stdout)
    });
    match tokio::time::timeout(std::time::Duration::from_secs(30), task).await {
        Ok(joined) => joined.map_err(|e| e.to_string())?,
        Err(_) => Err(format!("{name} timed out")),
    }
}
