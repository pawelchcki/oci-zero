use std::{
    error::Error, ffi::OsString, fmt, io, path::PathBuf, process::ExitStatus, string::String,
};

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
