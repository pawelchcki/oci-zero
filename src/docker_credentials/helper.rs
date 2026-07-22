use serde::Deserialize;
use std::{
    ffi::OsString,
    io::Write,
    process::{Command, Stdio},
    string::String,
};

use super::{DockerCredentialError, OwnedCredentials};

const CREDENTIALS_NOT_FOUND: &str = "credentials not found in native keychain";

pub(super) fn validate_name(helper: &str) -> Result<(), DockerCredentialError> {
    if helper.is_empty()
        || helper
            .bytes()
            .any(|byte| !(byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')))
    {
        return Err(DockerCredentialError::InvalidHelperName {
            helper: helper.into(),
        });
    }
    Ok(())
}

pub(super) fn run(
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
    parse_response(program, &output.stdout).map(Some)
}

pub(super) fn parse_response(
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
