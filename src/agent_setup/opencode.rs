//! OpenCode status tracking setup.
//!
//! Resolves the plugin directory from `OPENCODE_CONFIG_DIR`,
//! `XDG_CONFIG_HOME/opencode`, or `~/.config/opencode`. `OPENCODE_CONFIG`
//! identifies a config file and does not change plugin discovery directories.
//! Installs the status plugin while preserving other OpenCode configuration.

use anyhow::{Context, Result};
use serde_json::Value;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::{StatusCheck, UpdatePreview};

/// OpenCode distribution files, embedded at compile time.
const PLUGIN_SOURCE: &str = include_str!("../../resources/opencode/plugins/workmux-status.ts");
const PACKAGE_JSON: &str = include_str!("../../resources/opencode/package.json");
const V2_FILES: &[(&str, &str)] = &[
    (
        "index.ts",
        include_str!("../../resources/opencode/v2/workmux-status/index.ts"),
    ),
    (
        "tui.ts",
        include_str!("../../resources/opencode/v2/workmux-status/tui.ts"),
    ),
    (
        "status.ts",
        include_str!("../../resources/opencode/v2/workmux-status/status.ts"),
    ),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpenCodeVersion {
    V1,
    V2,
}

fn parse_version(output: &str) -> Option<OpenCodeVersion> {
    let version = output.split_whitespace().last()?.trim_start_matches('v');
    let (major, remainder) = version.split_once('.')?;
    if !remainder.chars().next()?.is_ascii_digit() {
        return None;
    }
    match major.parse::<u32>().ok()? {
        1 => Some(OpenCodeVersion::V1),
        2 => Some(OpenCodeVersion::V2),
        _ => None,
    }
}

fn installed_version() -> Result<OpenCodeVersion> {
    version_from_command(Path::new("opencode"))
}

fn version_from_command(command: &Path) -> Result<OpenCodeVersion> {
    let output = Command::new(command)
        .arg("--version")
        .output()
        .context("Could not run opencode --version; status tracking was not changed")?;
    let version = String::from_utf8_lossy(&output.stdout);
    if !output.status.success() {
        anyhow::bail!("opencode --version failed; status tracking was not changed");
    }
    parse_version(&version).ok_or_else(|| {
        anyhow::anyhow!(
            "Unsupported OpenCode version {:?}; status tracking was not changed",
            version.trim()
        )
    })
}

fn v2_dir(config_dir: &Path) -> PathBuf {
    config_dir.join("plugins/workmux-status")
}

fn v2_files_present(config_dir: &Path) -> bool {
    V2_FILES
        .iter()
        .any(|(name, _)| v2_dir(config_dir).join(name).exists())
}

fn read_optional(path: &Path) -> Result<String> {
    match fs::read_to_string(path) {
        Ok(content) => Ok(content),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(error).with_context(|| format!("Failed to read {}", path.display())),
    }
}

fn non_empty(value: Option<OsString>) -> Option<OsString> {
    value.filter(|value| !value.is_empty())
}

fn resolve_config_dir(
    config_dir: Option<OsString>,
    xdg_config_home: Option<OsString>,
    home: Option<PathBuf>,
) -> Option<PathBuf> {
    if let Some(dir) = non_empty(config_dir) {
        return Some(PathBuf::from(dir));
    }
    if let Some(xdg) = non_empty(xdg_config_home) {
        return Some(PathBuf::from(xdg).join("opencode"));
    }
    home.map(|home| home.join(".config/opencode"))
}

pub fn opencode_config_dir() -> Option<PathBuf> {
    resolve_config_dir(
        std::env::var_os("OPENCODE_CONFIG_DIR"),
        std::env::var_os("XDG_CONFIG_HOME"),
        home::home_dir(),
    )
}

/// Detect if OpenCode is present via filesystem.
/// Returns the reason string if detected, None otherwise.
pub fn detect() -> Option<&'static str> {
    if non_empty(std::env::var_os("OPENCODE_CONFIG_DIR"))
        .is_some_and(|dir| PathBuf::from(dir).is_dir())
    {
        return Some("found $OPENCODE_CONFIG_DIR");
    }
    if non_empty(std::env::var_os("OPENCODE_CONFIG"))
        .is_some_and(|file| PathBuf::from(file).is_file())
    {
        return Some("found $OPENCODE_CONFIG");
    }
    if opencode_config_dir().is_some_and(|dir| dir.is_dir()) {
        return Some("found OpenCode config directory");
    }

    None
}

fn check_at(config_dir: &Path, version: OpenCodeVersion) -> Result<StatusCheck> {
    let plugin = config_dir.join("plugins/workmux-status.ts");
    let legacy = config_dir.join("plugin/workmux-status.ts");
    match version {
        OpenCodeVersion::V1 => {
            if plugin.exists() {
                let installed = fs::read_to_string(&plugin).with_context(|| {
                    format!("Failed to read OpenCode plugin {}", plugin.display())
                })?;
                return if installed == PLUGIN_SOURCE
                    && !legacy.exists()
                    && !v2_files_present(config_dir)
                {
                    Ok(StatusCheck::Installed)
                } else {
                    Ok(StatusCheck::UpdateAvailable)
                };
            }
        }
        OpenCodeVersion::V2 => {
            let mut all_installed = true;
            for (name, source) in V2_FILES {
                let path = v2_dir(config_dir).join(name);
                match fs::read_to_string(&path) {
                    Ok(installed) => all_installed &= installed == *source,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        all_installed = false;
                    }
                    Err(error) => {
                        return Err(error).context(format!("Failed to read {}", path.display()));
                    }
                }
            }
            if all_installed && !plugin.exists() && !legacy.exists() {
                return Ok(StatusCheck::Installed);
            }
            if v2_files_present(config_dir) || plugin.exists() || legacy.exists() {
                return Ok(StatusCheck::UpdateAvailable);
            }
        }
    }
    if legacy.exists() || v2_files_present(config_dir) {
        Ok(StatusCheck::UpdateAvailable)
    } else {
        Ok(StatusCheck::NotInstalled)
    }
}

/// Check if workmux plugin is installed for OpenCode.
pub fn check() -> Result<StatusCheck> {
    let Some(config_dir) = opencode_config_dir() else {
        return Ok(StatusCheck::NotInstalled);
    };
    check_at(&config_dir, installed_version()?)
}

pub(crate) fn update_preview() -> Result<Option<UpdatePreview>> {
    let Some(config_dir) = opencode_config_dir() else {
        return Ok(None);
    };
    let version = installed_version()?;
    let plugin = config_dir.join("plugins/workmux-status.ts");
    let legacy = config_dir.join("plugin/workmux-status.ts");
    if version == OpenCodeVersion::V1 {
        let installed_path = if plugin.exists() { &plugin } else { &legacy };
        let mut installed = read_optional(installed_path)?;
        for (name, _) in V2_FILES {
            let path = v2_dir(&config_dir).join(name);
            if path.exists() {
                installed.push_str(&format!("\n--- V2 {name} (to remove) ---\n"));
                installed.push_str(&read_optional(&path)?);
            }
        }
        return Ok(Some(UpdatePreview {
            label: installed_path.display().to_string(),
            installed,
            bundled: PLUGIN_SOURCE.to_string(),
        }));
    }

    let mut installed = String::new();
    let mut bundled = String::new();
    for (name, source) in V2_FILES {
        let path = v2_dir(&config_dir).join(name);
        installed.push_str(&format!("--- {name} ---\n"));
        bundled.push_str(&format!("--- {name} ---\n"));
        installed.push_str(&read_optional(&path)?);
        bundled.push_str(source);
        installed.push('\n');
        bundled.push('\n');
    }
    if plugin.exists() || legacy.exists() {
        installed.push_str("--- V1 plugin (to remove) ---\n");
        installed.push_str(&fs::read_to_string(if plugin.exists() {
            plugin
        } else {
            legacy
        })?);
    }
    Ok(Some(UpdatePreview {
        label: v2_dir(&config_dir).display().to_string(),
        installed,
        bundled,
    }))
}

fn remove_v2_files(config_dir: &Path) -> Result<()> {
    let dir = v2_dir(config_dir);
    for (name, source) in V2_FILES {
        let path = dir.join(name);
        if path.exists() && fs::read_to_string(&path)? != *source {
            anyhow::bail!(
                "Refusing to replace a modified OpenCode V2 plugin file: {}",
                path.display()
            );
        }
    }
    for (name, _) in V2_FILES {
        let path = dir.join(name);
        if path.exists() {
            fs::remove_file(path)?;
        }
    }
    if dir.is_dir() && dir.read_dir()?.next().is_none() {
        fs::remove_dir(dir)?;
    }
    Ok(())
}

fn install_at(config_dir: &Path, version: OpenCodeVersion) -> Result<()> {
    let plugin = config_dir.join("plugins/workmux-status.ts");
    let legacy_plugin = config_dir.join("plugin/workmux-status.ts");
    match version {
        OpenCodeVersion::V1 => {
            // Preflight modified files before writing the V1 plugin.
            remove_v2_files(config_dir)?;
            fs::create_dir_all(plugin.parent().expect("plugin path has a parent"))
                .context("Failed to create OpenCode plugin directory")?;
            fs::write(&plugin, PLUGIN_SOURCE).context("Failed to write OpenCode plugin")?;
            if legacy_plugin.exists() {
                fs::remove_file(&legacy_plugin)
                    .context("Failed to remove legacy OpenCode plugin")?;
            }
        }
        OpenCodeVersion::V2 => {
            // A V1 plugin in the same config would fail to load in V2.
            for path in [&plugin, &legacy_plugin] {
                if path.exists() && fs::read_to_string(path)? != PLUGIN_SOURCE {
                    anyhow::bail!(
                        "Refusing to remove a modified OpenCode V1 plugin: {}",
                        path.display()
                    );
                }
            }
            let dir = v2_dir(config_dir);
            fs::create_dir_all(&dir).context("Failed to create OpenCode V2 plugin directory")?;
            for (name, source) in V2_FILES {
                fs::write(dir.join(name), source).context("Failed to write OpenCode V2 plugin")?;
            }
            for path in [&plugin, &legacy_plugin] {
                if path.exists() {
                    fs::remove_file(path)?;
                }
            }
        }
    }
    Ok(())
}

/// Install workmux plugin for OpenCode.
/// Returns a description of what was done.
pub fn install() -> Result<String> {
    let config_dir = opencode_config_dir()
        .ok_or_else(|| anyhow::anyhow!("Could not determine OpenCode config directory"))?;
    let version = installed_version()?;
    install_at(&config_dir, version)?;

    let path = match version {
        OpenCodeVersion::V1 => config_dir.join("plugins/workmux-status.ts"),
        OpenCodeVersion::V2 => v2_dir(&config_dir),
    };
    Ok(format!(
        "Installed OpenCode plugin to {}. Restart OpenCode for it to take effect.",
        path.display()
    ))
}

/// Remove workmux plugin files from OpenCode config directory.
///
/// Removes plugin files from both supported locations. It removes package.json
/// only when the file consists entirely of the bundled package configuration.
pub fn uninstall() -> Result<String> {
    let Some(config_dir) = opencode_config_dir() else {
        return Ok("No OpenCode config directory found".to_string());
    };
    uninstall_at(config_dir)
}

fn uninstall_at(config_dir: PathBuf) -> Result<String> {
    let mut removed = Vec::new();

    let dir = v2_dir(&config_dir);
    for (name, _) in V2_FILES {
        let path = dir.join(name);
        if path.exists() {
            fs::remove_file(&path)?;
            removed.push(path.display().to_string());
        }
    }
    if dir.is_dir() && dir.read_dir()?.next().is_none() {
        fs::remove_dir(dir)?;
    }

    let plugin_path = config_dir.join("plugins/workmux-status.ts");
    if plugin_path.exists() {
        fs::remove_file(&plugin_path)?;
        removed.push(plugin_path.display().to_string());
        if let Some(parent) = plugin_path.parent()
            && parent
                .read_dir()
                .is_ok_and(|mut entries| entries.next().is_none())
        {
            let _ = fs::remove_dir(parent);
        }
    }

    let legacy_path = config_dir.join("plugin/workmux-status.ts");
    if legacy_path.exists() {
        fs::remove_file(&legacy_path)?;
        removed.push(legacy_path.display().to_string());
    }

    let package_path = config_dir.join("package.json");
    if package_path.exists() {
        let content = fs::read_to_string(&package_path)?;
        if let (Ok(installed), Ok(existing)) = (
            serde_json::from_str::<Value>(PACKAGE_JSON),
            serde_json::from_str::<Value>(&content),
        ) && installed == existing
        {
            fs::remove_file(&package_path)?;
            removed.push(package_path.display().to_string());
        }
    }

    if removed.is_empty() {
        Ok("No OpenCode plugin files found".to_string())
    } else {
        Ok(format!(
            "Removed OpenCode plugin files: {}",
            removed.join(", ")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn config_dir_prefers_non_empty_directory_override() {
        assert_eq!(
            resolve_config_dir(
                Some("/config-dir".into()),
                Some("/xdg".into()),
                Some("/home/user".into()),
            ),
            Some(PathBuf::from("/config-dir"))
        );
    }

    #[test]
    fn config_dir_uses_xdg_then_home_defaults() {
        assert_eq!(
            resolve_config_dir(None, Some("/xdg".into()), Some("/home/user".into())),
            Some(PathBuf::from("/xdg/opencode"))
        );
        assert_eq!(
            resolve_config_dir(None, Some("".into()), Some("/home/user".into())),
            Some(PathBuf::from("/home/user/.config/opencode"))
        );
    }

    #[test]
    fn recognizes_only_supported_opencode_versions() {
        assert_eq!(
            parse_version("opencode 1.18.29\n"),
            Some(OpenCodeVersion::V1)
        );
        assert_eq!(
            parse_version("opencode v2.0.18\n"),
            Some(OpenCodeVersion::V2)
        );
        assert_eq!(parse_version("2.0.18\n"), Some(OpenCodeVersion::V2));
        assert_eq!(parse_version("opencode v3.0.0"), None);
        assert_eq!(parse_version("opencode development"), None);
        assert_eq!(parse_version(""), None);
    }

    #[cfg(unix)]
    #[test]
    fn invokes_opencode_version_and_fails_closed_on_unknown_or_failed_commands() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let binary = tmp.path().join("opencode");
        fs::write(&binary, "#!/bin/sh\nprintf 'opencode v2.0.18\\n'\n").unwrap();
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(version_from_command(&binary).unwrap(), OpenCodeVersion::V2);

        fs::write(&binary, "#!/bin/sh\nprintf 'opencode v1.18.29\\n'\n").unwrap();
        assert_eq!(version_from_command(&binary).unwrap(), OpenCodeVersion::V1);

        fs::write(&binary, "#!/bin/sh\nprintf 'opencode v3.0.0\\n'\n").unwrap();
        assert!(version_from_command(&binary).is_err());
        fs::write(&binary, "#!/bin/sh\nexit 1\n").unwrap();
        assert!(version_from_command(&binary).is_err());
        assert!(version_from_command(&tmp.path().join("missing")).is_err());
    }

    #[test]
    fn v2_install_migrates_only_the_workmux_plugin_and_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let plugins = tmp.path().join("plugins");
        fs::create_dir_all(&plugins).unwrap();
        fs::write(plugins.join("workmux-status.ts"), PLUGIN_SOURCE).unwrap();
        fs::write(plugins.join("custom.ts"), "// keep me").unwrap();
        fs::write(tmp.path().join("package.json"), r#"{"custom": true}"#).unwrap();

        assert!(matches!(
            check_at(tmp.path(), OpenCodeVersion::V2).unwrap(),
            StatusCheck::UpdateAvailable
        ));
        install_at(tmp.path(), OpenCodeVersion::V2).unwrap();
        install_at(tmp.path(), OpenCodeVersion::V2).unwrap();

        for (name, source) in V2_FILES {
            assert_eq!(
                fs::read_to_string(v2_dir(tmp.path()).join(name)).unwrap(),
                *source
            );
        }
        assert!(!plugins.join("workmux-status.ts").exists());
        assert_eq!(
            fs::read_to_string(plugins.join("custom.ts")).unwrap(),
            "// keep me"
        );
        assert_eq!(
            fs::read_to_string(tmp.path().join("package.json")).unwrap(),
            r#"{"custom": true}"#
        );
        assert!(matches!(
            check_at(tmp.path(), OpenCodeVersion::V2).unwrap(),
            StatusCheck::Installed
        ));
        assert!(matches!(
            check_at(tmp.path(), OpenCodeVersion::V1).unwrap(),
            StatusCheck::UpdateAvailable
        ));
    }

    #[test]
    fn v1_install_removes_v2_entrypoints_before_installing_the_original() {
        let tmp = tempfile::tempdir().unwrap();
        install_at(tmp.path(), OpenCodeVersion::V2).unwrap();
        assert!(matches!(
            check_at(tmp.path(), OpenCodeVersion::V1).unwrap(),
            StatusCheck::UpdateAvailable
        ));

        install_at(tmp.path(), OpenCodeVersion::V1).unwrap();
        assert_eq!(
            fs::read_to_string(tmp.path().join("plugins/workmux-status.ts")).unwrap(),
            PLUGIN_SOURCE
        );
        assert!(!v2_dir(tmp.path()).exists());
        assert!(matches!(
            check_at(tmp.path(), OpenCodeVersion::V1).unwrap(),
            StatusCheck::Installed
        ));
    }

    #[test]
    fn v2_install_refuses_to_remove_modified_v1_plugin() {
        let tmp = tempfile::tempdir().unwrap();
        let plugin = tmp.path().join("plugins/workmux-status.ts");
        fs::create_dir_all(plugin.parent().unwrap()).unwrap();
        fs::write(&plugin, "// custom user code").unwrap();

        assert!(install_at(tmp.path(), OpenCodeVersion::V2).is_err());
        assert_eq!(fs::read_to_string(plugin).unwrap(), "// custom user code");
        assert!(!v2_dir(tmp.path()).exists());
    }

    #[test]
    fn v1_install_refuses_to_remove_modified_v2_plugin() {
        let tmp = tempfile::tempdir().unwrap();
        install_at(tmp.path(), OpenCodeVersion::V2).unwrap();
        let custom = v2_dir(tmp.path()).join("tui.ts");
        fs::write(&custom, "// custom user code").unwrap();

        assert!(install_at(tmp.path(), OpenCodeVersion::V1).is_err());
        assert_eq!(fs::read_to_string(custom).unwrap(), "// custom user code");
        assert!(!tmp.path().join("plugins/workmux-status.ts").exists());
    }

    #[test]
    fn v2_check_detects_partial_installs_and_legacy_v1_files() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(matches!(
            check_at(tmp.path(), OpenCodeVersion::V2).unwrap(),
            StatusCheck::NotInstalled
        ));
        fs::create_dir_all(v2_dir(tmp.path())).unwrap();
        fs::write(v2_dir(tmp.path()).join("index.ts"), V2_FILES[0].1).unwrap();
        assert!(matches!(
            check_at(tmp.path(), OpenCodeVersion::V2).unwrap(),
            StatusCheck::UpdateAvailable
        ));
        install_at(tmp.path(), OpenCodeVersion::V2).unwrap();
        fs::create_dir_all(tmp.path().join("plugin")).unwrap();
        fs::write(tmp.path().join("plugin/workmux-status.ts"), PLUGIN_SOURCE).unwrap();
        assert!(matches!(
            check_at(tmp.path(), OpenCodeVersion::V2).unwrap(),
            StatusCheck::UpdateAvailable
        ));
        install_at(tmp.path(), OpenCodeVersion::V2).unwrap();
        assert!(!tmp.path().join("plugin/workmux-status.ts").exists());
    }

    #[test]
    fn uninstall_removes_both_workmux_versions_but_keeps_other_plugins() {
        let tmp = tempfile::tempdir().unwrap();
        install_at(tmp.path(), OpenCodeVersion::V2).unwrap();
        let custom = v2_dir(tmp.path()).join("custom.ts");
        fs::write(&custom, "// keep me").unwrap();
        fs::write(tmp.path().join("plugins/workmux-status.ts"), PLUGIN_SOURCE).unwrap();

        uninstall_at(tmp.path().to_path_buf()).unwrap();
        assert_eq!(fs::read_to_string(custom).unwrap(), "// keep me");
        assert!(!tmp.path().join("plugins/workmux-status.ts").exists());
        for (name, _) in V2_FILES {
            assert!(!v2_dir(tmp.path()).join(name).exists());
        }
    }

    #[test]
    fn check_compares_installed_source_and_flags_legacy_files() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(matches!(
            check_at(tmp.path(), OpenCodeVersion::V1).unwrap(),
            StatusCheck::NotInstalled
        ));

        let legacy = tmp.path().join("plugin/workmux-status.ts");
        fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        fs::write(&legacy, PLUGIN_SOURCE).unwrap();
        assert!(matches!(
            check_at(tmp.path(), OpenCodeVersion::V1).unwrap(),
            StatusCheck::UpdateAvailable
        ));

        let plugin = tmp.path().join("plugins/workmux-status.ts");
        fs::create_dir_all(plugin.parent().unwrap()).unwrap();
        fs::write(&plugin, "// old plugin").unwrap();
        assert!(matches!(
            check_at(tmp.path(), OpenCodeVersion::V1).unwrap(),
            StatusCheck::UpdateAvailable
        ));
        fs::write(&plugin, PLUGIN_SOURCE).unwrap();
        assert!(matches!(
            check_at(tmp.path(), OpenCodeVersion::V1).unwrap(),
            StatusCheck::UpdateAvailable
        ));
        fs::remove_file(legacy).unwrap();
        assert!(matches!(
            check_at(tmp.path(), OpenCodeVersion::V1).unwrap(),
            StatusCheck::Installed
        ));
    }

    #[test]
    fn install_preserves_package_json_and_other_plugins() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("plugins")).unwrap();
        fs::write(tmp.path().join("plugins/custom.ts"), "// custom").unwrap();
        fs::create_dir_all(tmp.path().join("plugin")).unwrap();
        fs::write(
            tmp.path().join("plugin/workmux-status.ts"),
            "// legacy workmux plugin",
        )
        .unwrap();
        fs::write(tmp.path().join("plugin/custom.ts"), "// legacy custom").unwrap();
        let package = serde_json::to_string_pretty(&json!({
            "name": "custom-config",
            "scripts": { "check": "echo ok" },
            "dependencies": {
                "@opencode-ai/plugin": "9.0.0",
                "other-package": "2.0.0"
            }
        }))
        .unwrap();
        fs::write(tmp.path().join("package.json"), &package).unwrap();

        install_at(tmp.path(), OpenCodeVersion::V1).unwrap();
        assert_eq!(
            fs::read_to_string(tmp.path().join("package.json")).unwrap(),
            package
        );
        assert_eq!(
            fs::read_to_string(tmp.path().join("plugins/custom.ts")).unwrap(),
            "// custom"
        );
        assert_eq!(
            fs::read_to_string(tmp.path().join("plugins/workmux-status.ts")).unwrap(),
            PLUGIN_SOURCE
        );
        assert!(!tmp.path().join("plugin/workmux-status.ts").exists());
        assert_eq!(
            fs::read_to_string(tmp.path().join("plugin/custom.ts")).unwrap(),
            "// legacy custom"
        );
    }

    #[test]
    fn install_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        install_at(tmp.path(), OpenCodeVersion::V1).unwrap();
        let first_plugin = fs::read(tmp.path().join("plugins/workmux-status.ts")).unwrap();
        install_at(tmp.path(), OpenCodeVersion::V1).unwrap();
        assert_eq!(
            fs::read(tmp.path().join("plugins/workmux-status.ts")).unwrap(),
            first_plugin
        );
        assert!(!tmp.path().join("package.json").exists());
    }

    #[test]
    fn uninstall_no_files() {
        let tmp = tempfile::tempdir().unwrap();
        let result = uninstall_at(tmp.path().to_path_buf()).unwrap();
        assert!(result.contains("No OpenCode plugin files found"));
    }

    #[test]
    fn uninstall_removes_plugin_and_exact_bundled_package() {
        let tmp = tempfile::tempdir().unwrap();
        install_at(tmp.path(), OpenCodeVersion::V1).unwrap();
        fs::write(tmp.path().join("package.json"), PACKAGE_JSON).unwrap();

        let result = uninstall_at(tmp.path().to_path_buf()).unwrap();
        assert!(result.contains("Removed OpenCode plugin files"));
        assert!(!tmp.path().join("plugins/workmux-status.ts").exists());
        assert!(!tmp.path().join("package.json").exists());
    }

    #[test]
    fn uninstall_keeps_modified_package_json() {
        let tmp = tempfile::tempdir().unwrap();
        let plugin_dir = tmp.path().join("plugins");
        fs::create_dir_all(&plugin_dir).unwrap();
        fs::write(plugin_dir.join("workmux-status.ts"), "// plugin code").unwrap();
        fs::write(tmp.path().join("package.json"), r#"{"name": "custom"}"#).unwrap();

        uninstall_at(tmp.path().to_path_buf()).unwrap();
        assert!(tmp.path().join("package.json").exists());
    }

    #[test]
    fn uninstall_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(
            uninstall_at(tmp.path().to_path_buf())
                .unwrap()
                .contains("No OpenCode plugin files found")
        );
        assert!(
            uninstall_at(tmp.path().to_path_buf())
                .unwrap()
                .contains("No OpenCode plugin files found")
        );
    }
}
