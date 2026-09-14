use std::path::PathBuf;
use std::process::Command;

use fastmail_cli::config::{Config, ContactsConfig, CoreConfig};
use fastmail_cli::error::Error;

// Environment changes belong to a child process, not the parallel test runner.
fn isolated(test: &str, env: &[(&str, &str)], check: impl FnOnce()) {
    if std::env::var("FASTMAIL_CONFIG_TEST").as_deref() == Ok(test) {
        check();
        return;
    }
    let home = tempfile::tempdir().unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test, "--nocapture"])
        .env("FASTMAIL_CONFIG_TEST", test)
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .env("XDG_CONFIG_HOME", home.path().join("xdg"))
        .env_remove("FASTMAIL_API_TOKEN")
        .env_remove("FASTMAIL_USERNAME")
        .env_remove("FASTMAIL_APP_PASSWORD")
        .envs(env.iter().copied())
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("running 1 test\n"),
        "Child filter did not select exactly one test: {test}"
    );
    assert!(
        output.status.success(),
        "{test}:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn config_path() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap()).join(".config/fastmail-cli/config.toml")
}

fn saved_config() -> Config {
    let config = Config {
        core: CoreConfig {
            api_token: Some("saved-token".into()),
        },
        contacts: ContactsConfig {
            username: Some("saved@example.com".into()),
            app_password: Some("saved-password".into()),
        },
    };
    config.save().unwrap();
    Config::load().unwrap()
}

#[test]
fn missing_config_and_credentials_have_actionable_errors() {
    isolated(
        "missing_config_and_credentials_have_actionable_errors",
        &[],
        || {
            let config = Config::load().unwrap();
            assert!(matches!(config.get_token(), Err(Error::NotAuthenticated)));
            assert_eq!(
                config.get_username().unwrap_err().to_string(),
                "Config error: Username not set in [contacts] config."
            );
            assert_eq!(
                config.get_app_password().unwrap_err().to_string(),
                "Config error: App password not set in [contacts] config."
            );
            assert!(!config_path().exists());
        },
    );
}

#[test]
fn getters_use_saved_credentials_without_environment_overrides() {
    isolated(
        "getters_use_saved_credentials_without_environment_overrides",
        &[],
        || {
            let config = saved_config();
            assert_eq!(config.get_token().unwrap(), "saved-token");
            assert_eq!(config.get_username().unwrap(), "saved@example.com");
            assert_eq!(config.get_app_password().unwrap(), "saved-password");
        },
    );
}

#[test]
fn environment_credentials_override_saved_credentials() {
    isolated(
        "environment_credentials_override_saved_credentials",
        &[
            ("FASTMAIL_API_TOKEN", "env-token"),
            ("FASTMAIL_USERNAME", "env@example.com"),
            ("FASTMAIL_APP_PASSWORD", "env-password"),
        ],
        || {
            let config = saved_config();
            assert_eq!(config.get_token().unwrap(), "env-token");
            assert_eq!(config.get_username().unwrap(), "env@example.com");
            assert_eq!(config.get_app_password().unwrap(), "env-password");
            assert_eq!(config.core.api_token.as_deref(), Some("saved-token"));
            assert_eq!(
                config.contacts.username.as_deref(),
                Some("saved@example.com")
            );
            assert_eq!(
                config.contacts.app_password.as_deref(),
                Some("saved-password")
            );
        },
    );
}

#[test]
fn empty_environment_values_do_not_fall_back_to_saved_credentials() {
    isolated(
        "empty_environment_values_do_not_fall_back_to_saved_credentials",
        &[
            ("FASTMAIL_API_TOKEN", ""),
            ("FASTMAIL_USERNAME", ""),
            ("FASTMAIL_APP_PASSWORD", ""),
        ],
        || {
            let config = saved_config();
            assert_eq!(config.get_token().unwrap(), "");
            assert_eq!(config.get_username().unwrap(), "");
            assert_eq!(config.get_app_password().unwrap(), "");
        },
    );
}

#[test]
fn malformed_config_loads_fail_without_echoing_credentials() {
    isolated(
        "malformed_config_loads_fail_without_echoing_credentials",
        &[],
        || {
            saved_config();
            std::fs::write(config_path(), "[core]\napi_token = 'private-token' invalid").unwrap();
            let error = Config::load().unwrap_err().to_string();
            assert_eq!(
                error,
                "Config error: Invalid config.toml: check TOML syntax and field types."
            );
            assert!(!error.contains("private-token"));
        },
    );
}

#[test]
fn save_replaces_existing_config_and_removes_the_temporary_file() {
    isolated(
        "save_replaces_existing_config_and_removes_the_temporary_file",
        &[],
        || {
            let mut config = saved_config();
            config.set_token("replacement".into());
            config.save().unwrap();
            assert_eq!(Config::load().unwrap().get_token().unwrap(), "replacement");
            assert!(!config_path().with_extension("toml.tmp").exists());
        },
    );
}

#[cfg(unix)]
#[test]
fn save_tightens_existing_permissions_and_refuses_symlink_targets() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    isolated(
        "save_tightens_existing_permissions_and_refuses_symlink_targets",
        &[],
        || {
            let config = saved_config();
            let path = config_path();
            let dir = path.parent().unwrap();
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755)).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
            config.save().unwrap();
            assert_eq!(
                std::fs::metadata(dir).unwrap().permissions().mode() & 0o777,
                0o700
            );
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );

            let target = dir.join("untouched");
            std::fs::write(&target, b"original").unwrap();
            std::fs::remove_file(&path).unwrap();
            symlink(&target, &path).unwrap();
            assert!(
                config
                    .save()
                    .unwrap_err()
                    .to_string()
                    .contains("is a symlink")
            );
            assert_eq!(std::fs::read(&target).unwrap(), b"original");
            assert!(
                std::fs::symlink_metadata(&path)
                    .unwrap()
                    .file_type()
                    .is_symlink()
            );

            std::fs::remove_file(&target).unwrap();
            assert!(
                config
                    .save()
                    .unwrap_err()
                    .to_string()
                    .contains("is a symlink")
            );
            assert!(!target.exists());
        },
    );
}
