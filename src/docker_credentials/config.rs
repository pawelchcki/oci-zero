use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::Deserialize;
use std::{collections::HashMap, string::String};

use super::{DockerCredentialError, OwnedCredentials};

const DOCKER_HUB_AUTH_KEY: &str = "https://index.docker.io/v1/";

#[derive(Default, Deserialize)]
#[serde(default)]
pub(super) struct DockerConfig {
    auths: HashMap<String, AuthEntry>,
    #[serde(rename = "credsStore")]
    credentials_store: String,
    #[serde(rename = "credHelpers")]
    credential_helpers: HashMap<String, String>,
}

impl DockerConfig {
    pub(super) fn parse(bytes: &[u8]) -> Result<Self, DockerCredentialError> {
        if bytes.iter().all(u8::is_ascii_whitespace) {
            return Ok(Self::default());
        }
        serde_json::from_slice(bytes).map_err(DockerCredentialError::ParseConfig)
    }

    pub(super) fn helper_for(&self, server: &str) -> Option<&str> {
        match self.credential_helpers.get(server) {
            Some(helper) => (!helper.is_empty()).then_some(helper.as_str()),
            None => (!self.credentials_store.is_empty()).then_some(self.credentials_store.as_str()),
        }
    }

    pub(super) fn credentials_for(
        &self,
        server: &str,
    ) -> Result<Option<OwnedCredentials>, DockerCredentialError> {
        self.auths
            .get(server)
            .map(|auth| decode_auth(server, auth))
            .unwrap_or(Ok(None))
    }
}

#[derive(Default)]
pub(super) struct EnvironmentAuths(HashMap<String, AuthEntry>);

impl EnvironmentAuths {
    pub(super) fn parse(bytes: &[u8]) -> Result<Self, DockerCredentialError> {
        let config: EnvironmentConfig =
            serde_json::from_slice(bytes).map_err(DockerCredentialError::ParseEnvironmentConfig)?;
        let mut auths = HashMap::with_capacity(config.auths.len());
        for (server, auth) in config.auths {
            if auth.auth.is_empty() {
                return Err(DockerCredentialError::InvalidAuth { registry: server });
            }
            auths.insert(server, AuthEntry { auth: auth.auth });
        }
        Ok(Self(auths))
    }

    pub(super) fn credentials_for(
        &self,
        server: &str,
    ) -> Result<Option<OwnedCredentials>, DockerCredentialError> {
        self.0
            .get(server)
            .map(|auth| decode_auth(server, auth))
            .unwrap_or(Ok(None))
    }
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

pub(super) fn canonical_server(authority: &str) -> String {
    let authority = authority.to_ascii_lowercase();
    match authority.as_str() {
        "docker.io" | "index.docker.io" | "registry-1.docker.io" => DOCKER_HUB_AUTH_KEY.into(),
        _ => authority,
    }
}

fn decode_auth(
    server: &str,
    auth: &AuthEntry,
) -> Result<Option<OwnedCredentials>, DockerCredentialError> {
    if auth.auth.is_empty() {
        return Ok(None);
    }
    let decoded = STANDARD
        .decode(auth.auth.as_bytes())
        .map_err(|_| invalid_auth(server))?;
    let decoded = String::from_utf8(decoded).map_err(|_| invalid_auth(server))?;
    let (username, secret) = decoded
        .split_once(':')
        .filter(|(username, _)| !username.is_empty())
        .ok_or_else(|| invalid_auth(server))?;
    Ok(Some(OwnedCredentials {
        username: username.into(),
        secret: secret.trim_matches('\0').into(),
    }))
}

fn invalid_auth(registry: &str) -> DockerCredentialError {
    DockerCredentialError::InvalidAuth {
        registry: registry.into(),
    }
}

#[cfg(test)]
pub(super) const TEST_DOCKER_HUB_AUTH_KEY: &str = DOCKER_HUB_AUTH_KEY;
