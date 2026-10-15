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
    // the path is passed via the environment, never interpolated into the script (no injection)
    let script = r#"$ErrorActionPreference='Stop'
$f = Get-Item -LiteralPath $env:GPN_EXE
$v = $f.VersionInfo
Add-Type -AssemblyName System.Drawing
Add-Type -Namespace Gpn -Name Icons -MemberDefinition '[DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern uint PrivateExtractIcons(string file, int index, int cx, int cy, IntPtr[] icons, uint[] ids, uint count, uint flags); [DllImport("user32.dll")] public static extern bool DestroyIcon(IntPtr h);'
$bmp = $null
# ask the exe's own icon resource for a large image (ExtractAssociatedIcon only gives 32px)
foreach ($size in 256, 128, 64) {
  try {
    $h = New-Object IntPtr[] 1
    $id = New-Object uint32[] 1
    $n = [Gpn.Icons]::PrivateExtractIcons($f.FullName, 0, $size, $size, $h, $id, 1, 0)
    if ($n -gt 0 -and $h[0] -ne [IntPtr]::Zero) {
      $bmp = [System.Drawing.Icon]::FromHandle($h[0]).ToBitmap()
      [Gpn.Icons]::DestroyIcon($h[0]) | Out-Null
      break
    }
  } catch {}
}
if (-not $bmp) { try { $bmp = [System.Drawing.Icon]::ExtractAssociatedIcon($f.FullName).ToBitmap() } catch {} }
$i = $null
if ($bmp) {
  # 96px = crisp at 48 CSS px on a 200% display, ~15 KB as PNG
  $out = New-Object System.Drawing.Bitmap 96, 96
  $g = [System.Drawing.Graphics]::FromImage($out)
  $g.InterpolationMode = 'HighQualityBicubic'; $g.SmoothingMode = 'HighQuality'; $g.PixelOffsetMode = 'HighQuality'
  $g.DrawImage($bmp, 0, 0, 96, 96); $g.Dispose()
  $ms = New-Object System.IO.MemoryStream
  $out.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
  $i = [Convert]::ToBase64String($ms.ToArray())
}
@{ name = @($v.FileDescription, $v.ProductName) | Where-Object { $_ } | Select-Object -First 1; company = $v.CompanyName; icon = $i } | ConvertTo-Json -Compress"#;

    let out = crate::proc::command("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .env("GPN_EXE", exe_path)
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
