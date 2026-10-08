use std::{error::Error, ffi::OsString, io, path::PathBuf, process::ExitStatus, string::String};

/// Failure to read Docker configuration or obtain a configured credential.
#[derive(Debug, derive_more::Display)]
pub enum DockerCredentialError {
    #[display("could not read Docker config at {}", path.display())]
    ReadConfig { path: PathBuf, source: io::Error },
    #[display("Docker config is not valid JSON")]
    ParseConfig(serde_json::Error),
    #[display("DOCKER_AUTH_CONFIG is not valid Docker auth JSON")]
    ParseEnvironmentConfig(serde_json::Error),
    #[display("DOCKER_AUTH_CONFIG is not valid Unicode")]
    EnvironmentNotUnicode,
    #[display("Docker auth entry for {registry} is invalid")]
    InvalidAuth { registry: String },
    #[display("Docker credential helper name {helper:?} is invalid")]
    InvalidHelperName { helper: String },
    #[display("could not run Docker credential helper {program:?}")]
    RunHelper {
        program: OsString,
        source: io::Error,
    },
    #[display("Docker credential helper {program:?} has no input pipe")]
    MissingHelperInput { program: OsString },
    #[display("Docker credential helper {program:?} failed with {status}")]
    HelperFailed {
        program: OsString,
        status: ExitStatus,
    },
    #[display("Docker credential helper {program:?} returned invalid JSON")]
    InvalidHelperResponse {
        program: OsString,
        source: serde_json::Error,
    },
    #[display("Docker credential helper {program:?} returned an empty username")]
    MissingHelperUsername { program: OsString },
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
