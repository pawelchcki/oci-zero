//! Host-side Docker CLI credential discovery.
//!
//! This module is opt-in because it reads the local filesystem, allocates, and
//! starts `docker-credential-*` processes. The default crate remains `no_std`
//! and allocation-free.

use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::Deserialize;
use std::{
    collections::HashMap,
    env,
    error::Error,
    ffi::OsString,
    fmt, fs,
    io::{self, Write},
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
    string::{String, ToString},
};

use crate::registry::{CredentialProvider, Credentials};

const DOCKER_HUB_AUTH_KEY: &str = "https://index.docker.io/v1/";
const CREDENTIALS_NOT_FOUND: &str = "credentials not found in native keychain";

/// Reads credentials using the same local sources and precedence as Docker CLI.
///
/// Credential values are never included in this type's errors. A configured
/// helper is executed directly, without a shell, as
/// `docker-credential-<name> get`.
pub struct DockerCredentialProvider {
    config: ConfigFile,
    environment_auths: HashMap<String, AuthEntry>,
    resolved: Option<OwnedCredentials>,
}

impl DockerCredentialProvider {
    /// Creates an empty provider which always returns no credentials.
    pub fn empty() -> Self {
        Self {
            config: ConfigFile::default(),
            environment_auths: HashMap::new(),
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
            provider.environment_auths = parse_environment_auths(value.as_bytes())?;
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
        let config = parse_config(bytes)?;
        Ok(Self {
            config,
            environment_auths: HashMap::new(),
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
        Ok(self.resolved.as_ref().map(|credentials| Credentials {
            username: &credentials.username,
            password: &credentials.secret,
        }))
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
        let server = auth_config_key(authority);
        if let Some(auth) = lookup(&self.environment_auths, &server) {
            return decode_auth(&server, auth);
        }

        let helper = match lookup(&self.config.credential_helpers, &server) {
            Some(helper) => (!helper.is_empty()).then_some(helper),
            None => (!self.config.credentials_store.is_empty())
                .then_some(&self.config.credentials_store),
        };
        if let Some(helper) = helper {
            validate_helper_name(helper)?;
            return helper_runner(helper, &server);
        }

        lookup(&self.config.auths, &server)
            .map(|auth| decode_auth(&server, auth))
            .unwrap_or(Ok(None))
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

#[derive(Default, Deserialize)]
#[serde(default)]
struct ConfigFile {
    auths: HashMap<String, AuthEntry>,
    #[serde(rename = "credsStore")]
    credentials_store: String,
    #[serde(rename = "credHelpers")]
    credential_helpers: HashMap<String, String>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct AuthEntry {
    auth: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EnvironmentConfig {
    #[serde(default)]
    auths: HashMap<String, EnvironmentAuth>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EnvironmentAuth {
    auth: String,
}

struct OwnedCredentials {
    username: String,
    secret: String,
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

fn parse_config(bytes: &[u8]) -> Result<ConfigFile, DockerCredentialError> {
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(ConfigFile::default());
    }
    serde_json::from_slice(bytes).map_err(DockerCredentialError::ParseConfig)
}

fn parse_environment_auths(
    bytes: &[u8],
) -> Result<HashMap<String, AuthEntry>, DockerCredentialError> {
    let config: EnvironmentConfig =
        serde_json::from_slice(bytes).map_err(DockerCredentialError::ParseEnvironmentConfig)?;
    let mut auths = HashMap::with_capacity(config.auths.len());
    for (server, auth) in config.auths {
        if auth.auth.is_empty() {
            return Err(DockerCredentialError::InvalidAuth { registry: server });
        }
        auths.insert(server, AuthEntry { auth: auth.auth });
    }
    Ok(auths)
}

fn auth_config_key(authority: &str) -> String {
    let authority = authority.to_ascii_lowercase();
    match authority.as_str() {
        "docker.io" | "index.docker.io" | "registry-1.docker.io" => DOCKER_HUB_AUTH_KEY.to_string(),
        _ => authority,
    }
}

fn lookup<'a, T>(values: &'a HashMap<String, T>, server: &str) -> Option<&'a T> {
    values.get(server)
}

fn decode_auth(
    server: &str,
    auth: &AuthEntry,
) -> Result<Option<OwnedCredentials>, DockerCredentialError> {
    if auth.auth.is_empty() {
        return Ok(None);
    }
    let decoded =
        STANDARD
            .decode(auth.auth.as_bytes())
            .map_err(|_| DockerCredentialError::InvalidAuth {
                registry: server.to_string(),
            })?;
    let decoded = String::from_utf8(decoded).map_err(|_| DockerCredentialError::InvalidAuth {
        registry: server.to_string(),
    })?;
    let (username, secret) = decoded
        .split_once(':')
        .filter(|(username, _)| !username.is_empty())
        .ok_or_else(|| DockerCredentialError::InvalidAuth {
            registry: server.to_string(),
        })?;
    Ok(Some(OwnedCredentials {
        username: username.to_string(),
        secret: secret.trim_matches('\0').to_string(),
    }))
}

fn validate_helper_name(helper: &str) -> Result<(), DockerCredentialError> {
    if helper.is_empty()
        || helper
            .bytes()
            .any(|byte| !(byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')))
    {
        return Err(DockerCredentialError::InvalidHelperName {
            helper: helper.to_string(),
        });
    }
    Ok(())
}

fn run_helper(
    helper: &str,
    server: &str,
) -> Result<Option<OwnedCredentials>, DockerCredentialError> {
    let program = OsString::from(std::format!("docker-credential-{helper}"));
    let mut child = Command::new(&program)
        .arg("get")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|source| DockerCredentialError::RunHelper {
            program: program.clone(),
            source,
        })?;

    let write_result = child
        .stdin
        .take()
        .ok_or_else(|| DockerCredentialError::MissingHelperInput {
            program: program.clone(),
        })?
        .write_all(server.as_bytes());
    if let Err(source) = write_result {
        let _ = child.kill();
        let _ = child.wait();
        return Err(DockerCredentialError::RunHelper { program, source });
    }

    let output = child
        .wait_with_output()
        .map_err(|source| DockerCredentialError::RunHelper {
            program: program.clone(),
            source,
        })?;
    if !output.status.success() {
        if String::from_utf8_lossy(&output.stdout).trim() == CREDENTIALS_NOT_FOUND {
            return Ok(None);
        }
        return Err(DockerCredentialError::HelperFailed {
            program,
            status: output.status,
        });
    }
    parse_helper_response(program, &output.stdout).map(Some)
}

fn parse_helper_response(
    program: OsString,
    bytes: &[u8],
) -> Result<OwnedCredentials, DockerCredentialError> {
    #[derive(Deserialize)]
    #[serde(rename_all = "PascalCase")]
    struct Response {
        username: String,
        secret: String,
    }

    let response: Response = serde_json::from_slice(bytes).map_err(|source| {
        DockerCredentialError::InvalidHelperResponse {
            program: program.clone(),
            source,
        }
    })?;
    if response.username.is_empty() {
        return Err(DockerCredentialError::MissingHelperUsername { program });
    }
    Ok(OwnedCredentials {
        username: response.username,
        secret: response.secret,
    })
}

/// Failure to read Docker configuration or obtain a configured credential.
#[derive(Debug)]
pub enum DockerCredentialError {
    ReadConfig {
        path: PathBuf,
        source: io::Error,
    },
    ParseConfig(serde_json::Error),
    ParseEnvironmentConfig(serde_json::Error),
    EnvironmentNotUnicode,
    InvalidAuth {
        registry: String,
    },
    InvalidHelperName {
        helper: String,
    },
    RunHelper {
        program: OsString,
        source: io::Error,
    },
    MissingHelperInput {
        program: OsString,
    },
    HelperFailed {
        program: OsString,
        status: ExitStatus,
    },
    InvalidHelperResponse {
        program: OsString,
        source: serde_json::Error,
    },
    MissingHelperUsername {
        program: OsString,
    },
}

impl fmt::Display for DockerCredentialError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ReadConfig { path, .. } => {
                write!(
                    formatter,
                    "could not read Docker config at {}",
                    path.display()
                )
            }
            Self::ParseConfig(_) => formatter.write_str("Docker config is not valid JSON"),
            Self::ParseEnvironmentConfig(_) => {
                formatter.write_str("DOCKER_AUTH_CONFIG is not valid Docker auth JSON")
            }
            Self::EnvironmentNotUnicode => {
                formatter.write_str("DOCKER_AUTH_CONFIG is not valid Unicode")
            }
            Self::InvalidAuth { registry } => {
                write!(formatter, "Docker auth entry for {registry} is invalid")
            }
            Self::InvalidHelperName { helper } => {
                write!(
                    formatter,
                    "Docker credential helper name {helper:?} is invalid"
                )
            }
            Self::RunHelper { program, .. } => {
                write!(
                    formatter,
                    "could not run Docker credential helper {program:?}"
                )
            }
            Self::MissingHelperInput { program } => {
                write!(
                    formatter,
                    "Docker credential helper {program:?} has no input pipe"
                )
            }
            Self::HelperFailed { program, status } => write!(
                formatter,
                "Docker credential helper {program:?} failed with {status}"
            ),
            Self::InvalidHelperResponse { program, .. } => write!(
                formatter,
                "Docker credential helper {program:?} returned invalid JSON"
            ),
            Self::MissingHelperUsername { program } => write!(
                formatter,
                "Docker credential helper {program:?} returned an empty username"
            ),
        }
    }
}

impl Error for DockerCredentialError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::ReadConfig { source, .. } | Self::RunHelper { source, .. } => Some(source),
            Self::ParseConfig(source)
            | Self::ParseEnvironmentConfig(source)
            | Self::InvalidHelperResponse { source, .. } => Some(source),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{ffi::OsString, string::ToString};

    use super::{
        parse_environment_auths, parse_helper_response, DockerCredentialError,
        DockerCredentialProvider, OwnedCredentials, DOCKER_HUB_AUTH_KEY,
    };

    fn config(json: &str) -> DockerCredentialProvider {
        DockerCredentialProvider::from_config_bytes(json.as_bytes()).unwrap()
    }

    #[test]
    fn registry_helper_precedes_global_store_and_inline_auth() {
        let provider = config(
            r#"{
                "auths": {"ghcr.io": {"auth": "d3Jvbmc6d3Jvbmc="}},
                "credsStore": "osxkeychain",
                "credHelpers": {"ghcr.io": "ghcr"}
            }"#,
        );
        let credentials = provider
            .resolve_with("GHCR.IO", |helper, server| {
                assert_eq!(helper, "ghcr");
                assert_eq!(server, "ghcr.io");
                Ok(Some(OwnedCredentials {
                    username: "helper-user".to_string(),
                    secret: "helper-secret".to_string(),
                }))
            })
            .unwrap()
            .unwrap();
        assert_eq!(credentials.username, "helper-user");
        assert_eq!(credentials.secret, "helper-secret");
    }

    #[test]
    fn global_store_precedes_inline_auth() {
        let provider = config(
            r#"{
                "auths": {"ghcr.io": {"auth": "d3Jvbmc6d3Jvbmc="}},
                "credsStore": "ddtool"
            }"#,
        );
        let credentials = provider
            .resolve_with("ghcr.io", |helper, server| {
                assert_eq!((helper, server), ("ddtool", "ghcr.io"));
                Ok(Some(OwnedCredentials {
                    username: "store-user".to_string(),
                    secret: "store-secret".to_string(),
                }))
            })
            .unwrap()
            .unwrap();
        assert_eq!(credentials.username, "store-user");
    }

    #[test]
    fn environment_auth_precedes_helpers() {
        let mut provider = config(r#"{"credsStore":"desktop"}"#);
        provider.environment_auths = parse_environment_auths(
            br#"{"auths":{"ghcr.io":{"auth":"ZW52LXVzZXI6ZW52LXNlY3JldA=="}}}"#,
        )
        .unwrap();
        let credentials = provider
            .resolve_with("ghcr.io", |_, _| panic!("helper must not run"))
            .unwrap()
            .unwrap();
        assert_eq!(credentials.username, "env-user");
        assert_eq!(credentials.secret, "env-secret");
    }

    #[test]
    fn decodes_inline_auth_and_preserves_colons_in_secret() {
        let provider = config(r#"{"auths":{"registry.example":{"auth":"dXNlcjpwYXNzOndvcmQ="}}}"#);
        let credentials = provider
            .resolve_with("registry.example", |_, _| panic!("helper must not run"))
            .unwrap()
            .unwrap();
        assert_eq!(credentials.username, "user");
        assert_eq!(credentials.secret, "pass:word");
    }

    #[test]
    fn canonicalizes_docker_hub_for_helper_lookup() {
        let provider = config(r#"{"credHelpers":{"https://index.docker.io/v1/":"osxkeychain"}}"#);
        provider
            .resolve_with("registry-1.docker.io", |helper, server| {
                assert_eq!(helper, "osxkeychain");
                assert_eq!(server, DOCKER_HUB_AUTH_KEY);
                Ok(None)
            })
            .unwrap();
    }

    #[test]
    fn helper_not_found_does_not_fall_back_to_inline_auth() {
        let provider = config(
            r#"{
                "auths": {"ghcr.io": {"auth": "dXNlcjpwYXNz"}},
                "credHelpers": {"ghcr.io": "ghcr"}
            }"#,
        );
        assert!(provider
            .resolve_with("ghcr.io", |_, _| Ok(None))
            .unwrap()
            .is_none());
    }

    #[test]
    fn empty_registry_helper_disables_the_global_store() {
        let provider = config(
            r#"{
                "auths": {"ghcr.io": {"auth": "dXNlcjpwYXNz"}},
                "credsStore": "desktop",
                "credHelpers": {"ghcr.io": ""}
            }"#,
        );
        let credentials = provider
            .resolve_with("ghcr.io", |_, _| panic!("helper must not run"))
            .unwrap()
            .unwrap();
        assert_eq!(credentials.username, "user");
        assert_eq!(credentials.secret, "pass");
    }

    #[test]
    fn rejects_helper_paths() {
        let provider = config(r#"{"credsStore":"../../helper"}"#);
        assert!(matches!(
            provider.resolve_with("ghcr.io", |_, _| Ok(None)),
            Err(DockerCredentialError::InvalidHelperName { .. })
        ));
    }

    #[test]
    fn parses_standard_helper_response_without_exposing_secret() {
        let credentials = parse_helper_response(
            OsString::from("docker-credential-test"),
            br#"{"Username":"alice","Secret":"top-secret"}"#,
        )
        .unwrap();
        assert_eq!(credentials.username, "alice");
        assert_eq!(credentials.secret, "top-secret");
    }

    #[test]
    fn empty_config_is_valid() {
        let provider = DockerCredentialProvider::from_config_bytes(b" \n\t").unwrap();
        assert!(provider
            .resolve_with("ghcr.io", |_, _| Ok(None))
            .unwrap()
            .is_none());
    }

    #[test]
    fn environment_config_rejects_unknown_fields() {
        assert!(parse_environment_auths(
            br#"{"auths":{"ghcr.io":{"auth":"dXNlcjpwYXNz","email":"x"}}}"#
        )
        .is_err());
    }
}
