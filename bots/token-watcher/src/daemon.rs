use anyhow::{Result, bail};
use std::path::{Path, PathBuf};
use std::process::Command;

pub const LABEL: &str = "com.aperture.switchboard-token-watcher";

pub fn plist_path() -> Result<PathBuf> {
    let home =
        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("cannot determine home directory"))?;
    Ok(home
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{LABEL}.plist")))
}

pub fn generate_plist(binary: &Path, run_args: &[String], env_vars: &[(String, String)]) -> String {
    let mut xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{}</string>
        <string>run</string>
"#,
        xml_escape(binary.to_str().unwrap_or(""))
    );

    for arg in run_args {
        xml.push_str(&format!("        <string>{}</string>\n", xml_escape(arg)));
    }

    xml.push_str(
        "    </array>\n    <key>RunAtLoad</key>\n    <true/>\n    <key>KeepAlive</key>\n    <true/>\n",
    );

    if !env_vars.is_empty() {
        xml.push_str("    <key>EnvironmentVariables</key>\n    <dict>\n");
        for (k, v) in env_vars {
            xml.push_str(&format!(
                "        <key>{}</key>\n        <string>{}</string>\n",
                xml_escape(k),
                xml_escape(v)
            ));
        }
        xml.push_str("    </dict>\n");
    }

    let log_path = dirs::home_dir()
        .map(|h| {
            h.join("Library")
                .join("Logs")
                .join("switchboard-token-watcher.log")
        })
        .unwrap_or_else(|| PathBuf::from("/tmp/switchboard-token-watcher.log"));

    xml.push_str(&format!(
        "    <key>StandardErrorPath</key>\n    <string>{}</string>\n</dict>\n</plist>\n",
        xml_escape(log_path.to_str().unwrap_or("/tmp/switchboard-token-watcher.log"))
    ));

    xml
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

pub fn is_loaded() -> bool {
    Command::new("launchctl")
        .args(["list", LABEL])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

pub fn load(plist: &Path) -> Result<()> {
    let out = Command::new("launchctl").arg("load").arg("-w").arg(plist).output()?;
    if !out.status.success() {
        bail!(
            "launchctl load failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(())
}

pub fn unload(plist: &Path) -> Result<()> {
    let out = Command::new("launchctl").arg("unload").arg(plist).output()?;
    if !out.status.success() {
        bail!(
            "launchctl unload failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plist_uses_absolute_path_and_run_subcommand() {
        let content = generate_plist(
            Path::new("/usr/local/bin/switchboard-token-watcher"),
            &["--all".into(), "--threshold".into(), "0.25".into()],
            &[],
        );
        assert!(content.contains("<string>/usr/local/bin/switchboard-token-watcher</string>"));
        assert!(content.contains("<string>run</string>"));
        assert!(content.contains("<string>--all</string>"));
        assert!(content.contains("<string>--threshold</string>"));
        assert!(content.contains("<string>0.25</string>"));
        assert!(content.contains(LABEL));
        assert!(content.contains("<key>KeepAlive</key>"));
        assert!(content.contains("<key>RunAtLoad</key>"));
    }

    #[test]
    fn plist_includes_env_vars() {
        let content = generate_plist(
            Path::new("/bin/bot"),
            &[],
            &[("SWITCHBOARD_DIR".into(), "/tmp/sw".into())],
        );
        assert!(content.contains("<key>SWITCHBOARD_DIR</key>"));
        assert!(content.contains("<string>/tmp/sw</string>"));
    }

    #[test]
    fn plist_escapes_xml_special_chars() {
        let content = generate_plist(
            Path::new("/bin/bot"),
            &["--handle".into(), "a<b&c".into()],
            &[],
        );
        assert!(content.contains("a&lt;b&amp;c"));
        assert!(!content.contains("a<b&c"));
    }
}
