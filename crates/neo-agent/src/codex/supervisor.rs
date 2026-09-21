use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use tokio::process::{Child, Command};
use tokio::time::Duration;

use super::{CodexClient, CodexError};

#[derive(Clone, Debug)]
pub struct CodexSupervisorConfig {
    pub executable: PathBuf,
    pub home: PathBuf,
    pub request_timeout: Duration,
}

impl CodexSupervisorConfig {
    pub fn new(executable: impl Into<PathBuf>, home: impl Into<PathBuf>) -> Self {
        Self {
            executable: executable.into(),
            home: home.into(),
            request_timeout: Duration::from_secs(20),
        }
    }
}

pub struct CodexSupervisor {
    client: CodexClient,
    child: Child,
}

impl CodexSupervisor {
    pub async fn launch(config: CodexSupervisorConfig) -> Result<Self, CodexError> {
        tokio::fs::create_dir_all(&config.home).await?;
        let working_directory = config.home.join("runtime");
        tokio::fs::create_dir_all(&working_directory).await?;

        let mut command = Command::new(&config.executable);
        command
            .arg("-c")
            .arg("cli_auth_credentials_store=\"keyring\"")
            .arg("app-server")
            .arg("--listen")
            .arg("stdio://")
            .current_dir(&working_directory)
            .env_clear()
            .env("CODEX_HOME", &config.home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        copy_safe_environment(&mut command);

        let mut child = command.spawn()?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| CodexError::Protocol("app-server stdout was not piped".into()))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| CodexError::Protocol("app-server stdin was not piped".into()))?;
        let client = match CodexClient::connect(stdout, stdin, config.request_timeout).await {
            Ok(client) => client,
            Err(error) => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                return Err(error);
            }
        };
        Ok(Self { client, child })
    }

    pub fn client(&self) -> &CodexClient {
        &self.client
    }

    pub fn process_id(&self) -> Option<u32> {
        self.child.id()
    }

    pub fn has_exited(&mut self) -> Result<bool, CodexError> {
        Ok(self.child.try_wait()?.is_some())
    }

    pub async fn shutdown(mut self) -> Result<(), CodexError> {
        if self.child.try_wait()?.is_none() {
            self.child.kill().await?;
        }
        let _status = self.child.wait().await?;
        Ok(())
    }
}

fn copy_safe_environment(command: &mut Command) {
    for name in ["HOME", "TMPDIR", "LANG", "LC_ALL", "USER", "LOGNAME"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
}

pub fn default_codex_home(application_support: &Path) -> PathBuf {
    application_support.join("codex")
}

pub fn configured_executable() -> Option<OsString> {
    std::env::var_os("NEO_CODEX_BIN")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_home_is_isolated_from_normal_codex() {
        let application_support = Path::new("/tmp/starkbot-neo");
        assert_eq!(
            default_codex_home(application_support),
            Path::new("/tmp/starkbot-neo/codex")
        );
    }
}
