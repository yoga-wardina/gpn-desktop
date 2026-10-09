//! Windows-only: read exe version info + extract icon, via PowerShell.
//! Returns (name, company, icon_png_base64).

use base64::Engine;

pub struct ExeMeta {
    pub name: Option<String>,
    pub company: Option<String>,
    pub icon_b64: Option<String>,
}

pub fn resolve(exe_path: &str) -> Option<ExeMeta> {
    if !cfg!(windows) || !exe_path.contains('\\') && !exe_path.contains('/') {
        return None;
    }
    let script = format!(
        r#"$ErrorActionPreference='Stop'
$f = Get-Item -LiteralPath '{path}'
$v = $f.VersionInfo
$i = $null
try {{
  Add-Type -AssemblyName System.Drawing
  $ico = [System.Drawing.Icon]::ExtractAssociatedIcon($f.FullName)
  if ($ico) {{
    $ms = New-Object System.IO.MemoryStream
    $ico.ToBitmap().Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
    $i = [Convert]::ToBase64String($ms.ToArray())
  }}
}} catch {{}}
@{{ name = @($v.FileDescription, $v.ProductName) | Where-Object {{ $_ }} | Select-Object -First 1; company = $v.CompanyName; icon = $i }} | ConvertTo-Json -Compress"#,
        path = exe_path.replace('\'', "''")
    );

    let out = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).ok()?;
    let name = v
        .get("name")
        .and_then(|x| x.as_str())
        .map(|s| s.to_string());
    let company = v
        .get("company")
        .and_then(|x| x.as_str())
        .map(|s| s.to_string());
    let icon_b64 = v
        .get("icon")
        .and_then(|x| x.as_str())
        .map(|s| s.to_string());
    Some(ExeMeta {
        name,
        company,
        icon_b64,
    })
}

/// Validate a base64 icon string decodes (cheap sanity check).
pub fn valid_png_b64(b64: &str) -> bool {
    base64::engine::general_purpose::STANDARD
        .decode(b64)
        .map(|b| b.starts_with(&[0x89, b'P', b'N', b'G']))
        .unwrap_or(false)
}
