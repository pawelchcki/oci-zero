use std::{ffi::OsString, string::ToString};

use super::{
    config::{EnvironmentAuths, TEST_DOCKER_HUB_AUTH_KEY},
    helper::parse_response,
    DockerCredentialError, DockerCredentialProvider, OwnedCredentials,
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
    provider.environment_auths = EnvironmentAuths::parse(
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
fn environment_auth_falls_back_for_unmatched_registry() {
    let mut provider = config(r#"{"credsStore":"desktop"}"#);
    provider.environment_auths = EnvironmentAuths::parse(
        br#"{"auths":{"registry.example":{"auth":"ZW52LXVzZXI6ZW52LXNlY3JldA=="}}}"#,
    )
    .unwrap();
    provider
        .resolve_with("ghcr.io", |helper, server| {
            assert_eq!((helper, server), ("desktop", "ghcr.io"));
            Ok(None)
        })
        .unwrap();
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
    for authority in ["docker.io", "index.docker.io", "registry-1.docker.io"] {
        provider
            .resolve_with(authority, |helper, server| {
                assert_eq!(helper, "osxkeychain");
                assert_eq!(server, TEST_DOCKER_HUB_AUTH_KEY);
                Ok(None)
            })
            .unwrap();
    }
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
    let credentials = parse_response(
        OsString::from("docker-credential-test"),
        br#"{"Username":"alice","Secret":"top-secret"}"#,
    )
    .unwrap();
    assert_eq!(credentials.username, "alice");
    assert_eq!(credentials.secret, "top-secret");
}

#[test]
fn rejects_helper_response_without_username() {
    assert!(matches!(
        parse_response(
            OsString::from("docker-credential-test"),
            br#"{"Username":"","Secret":"top-secret"}"#,
        ),
        Err(DockerCredentialError::MissingHelperUsername { .. })
    ));
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
    assert!(EnvironmentAuths::parse(
        br#"{"auths":{"ghcr.io":{"auth":"dXNlcjpwYXNz","email":"x"}}}"#
    )
    .is_err());
}
