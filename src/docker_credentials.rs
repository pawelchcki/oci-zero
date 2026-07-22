//! Host-side Docker CLI credential discovery.
//!
//! This module is opt-in because it reads the local filesystem, allocates, and
//! starts `docker-credential-*` processes. The default crate remains `no_std`
//! and allocation-free.

mod config;
mod error;
mod helper;

use std::{
    env, fs, io,
    path::{Path, PathBuf},
    string::String,
};

use crate::registry::{CredentialProvider, Credentials};
use config::{canonical_server, DockerConfig, EnvironmentAuths};
pub use error::DockerCredentialError;
use helper::{run as run_helper, validate_name as validate_helper_name};

/// Reads credentials using the same local sources and precedence as Docker CLI.
///
/// Credential values are never included in this type's errors. A configured
/// helper is executed directly, without a shell, as
/// `docker-credential-<name> get`.
pub struct DockerCredentialProvider {
    config: DockerConfig,
    environment_auths: EnvironmentAuths,
    resolved: Option<OwnedCredentials>,
}

impl DockerCredentialProvider {
    /// Creates an empty provider which always returns no credentials.
    pub fn empty() -> Self {
        Self {
            config: DockerConfig::default(),
            environment_auths: EnvironmentAuths::default(),
            resolved: None,
        }
    }

    /// Loads `$DOCKER_CONFIG/config.json`, or `~/.docker/config.json` when the
    /// environment variable is absent. A missing file is treated as an empty
    /// configuration. `DOCKER_AUTH_CONFIG`, when set, takes precedence for
    /// registries present in that JSON object.
    pub fn from_env() -> Result<Self, DockerCredentialError> {
        let mut provider = match docker_config_path() {
            Some(path) => Self::from_path_if_present(&path)?,
            None => Self::empty(),
        };
        if let Some(value) = env::var_os("DOCKER_AUTH_CONFIG") {
            let value = value
                .into_string()
                .map_err(|_| DockerCredentialError::EnvironmentNotUnicode)?;
            provider.environment_auths = EnvironmentAuths::parse(value.as_bytes())?;
        }
        Ok(provider)
    }

    /// Loads a specific Docker `config.json` without consulting environment
    /// variables. A missing path is returned as an I/O error.
    pub fn from_path(path: impl AsRef<Path>) -> Result<Self, DockerCredentialError> {
        let path = path.as_ref();
        let bytes = fs::read(path).map_err(|source| DockerCredentialError::ReadConfig {
            path: path.to_path_buf(),
            source,
        })?;
        Self::from_config_bytes(&bytes)
    }

    /// Parses Docker `config.json` bytes without consulting the filesystem or
    /// environment. This is useful for applications which already own their
    /// configuration loading policy.
    pub fn from_config_bytes(bytes: &[u8]) -> Result<Self, DockerCredentialError> {
        Ok(Self {
            config: DockerConfig::parse(bytes)?,
            environment_auths: EnvironmentAuths::default(),
            resolved: None,
        })
    }

    /// Resolves credentials for a registry authority such as `ghcr.io` or
    /// `registry.example:5000`.
    pub fn credentials_for(
        &mut self,
        authority: &str,
    ) -> Result<Option<Credentials<'_>>, DockerCredentialError> {
        self.resolved = self.resolve_with(authority, run_helper)?;
        Ok(self.resolved.as_ref().map(OwnedCredentials::borrow))
    }

    fn from_path_if_present(path: &Path) -> Result<Self, DockerCredentialError> {
        match fs::read(path) {
            Ok(bytes) => Self::from_config_bytes(&bytes),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Self::empty()),
            Err(source) => Err(DockerCredentialError::ReadConfig {
                path: path.to_path_buf(),
                source,
            }),
        }
    }

    fn resolve_with<F>(
        &self,
        authority: &str,
        mut helper_runner: F,
    ) -> Result<Option<OwnedCredentials>, DockerCredentialError>
    where
        F: FnMut(&str, &str) -> Result<Option<OwnedCredentials>, DockerCredentialError>,
    {
        let server = canonical_server(authority);
        if let Some(credentials) = self.environment_auths.credentials_for(&server)? {
            return Ok(Some(credentials));
        }

        if let Some(helper) = self.config.helper_for(&server) {
            validate_helper_name(helper)?;
            return helper_runner(helper, &server);
        }

        self.config.credentials_for(&server)
    }
}

impl Default for DockerCredentialProvider {
    fn default() -> Self {
        Self::empty()
    }
}

impl CredentialProvider for DockerCredentialProvider {
    type Error = DockerCredentialError;

    fn credentials<'a>(
        &'a mut self,
        authority: &str,
        _scope: Option<&str>,
    ) -> Result<Option<Credentials<'a>>, Self::Error> {
        self.credentials_for(authority)
    }
}

struct OwnedCredentials {
    username: String,
    secret: String,
}

impl OwnedCredentials {
    fn borrow(&self) -> Credentials<'_> {
        Credentials {
            username: &self.username,
            password: &self.secret,
        }
    }
}

fn docker_config_path() -> Option<PathBuf> {
    if let Some(directory) = env::var_os("DOCKER_CONFIG").filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(directory).join("config.json"));
    }
    home_directory().map(|home| home.join(".docker").join("config.json"))
}

fn home_directory() -> Option<PathBuf> {
    env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests;
