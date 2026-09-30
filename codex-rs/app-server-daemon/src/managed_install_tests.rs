use std::path::PathBuf;

use pretty_assertions::assert_eq;

use super::ExecutableIdentity;
use super::app_server_version_matches_cli;
use super::daemon_codex_bin;
use super::executable_identity;
use super::managed_codex_bin;
use super::parse_codex_version;

#[test]
fn parses_codex_cli_version_output() {
    assert_eq!(
        parse_codex_version("codex 1.2.3\n").expect("version"),
        "1.2.3"
    );
}

#[test]
fn rejects_malformed_codex_cli_version_output() {
    assert!(parse_codex_version("codex\n").is_err());
}

#[tokio::test]
async fn executable_identity_uses_binary_contents() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let executable = directory.path().join("codex");
    // Span multiple reads, including a partial final buffer, and preserve the
    // digest stored by older clients that hashed the complete file in memory.
    let mut bytes: Vec<u8> = (0..200_003).map(|index| (index % 251) as u8).collect();
    for contents in [&bytes[..], &[][..]] {
        std::fs::write(&executable, contents).expect("write executable");
        assert_eq!(
            executable_identity(&executable).await.expect("identity"),
            ExecutableIdentity {
                digest: *blake3::hash(contents).as_bytes(),
            }
        );
    }
    std::fs::write(&executable, &bytes).expect("write executable");
    let old = executable_identity(&executable).await.expect("identity");
    bytes[100_000] ^= 1;
    std::fs::write(&executable, bytes).expect("replace executable");
    assert_ne!(
        executable_identity(&executable)
            .await
            .expect("new identity"),
        old
    );
}

#[test]
fn daemon_codex_bin_prefers_current_exe_over_standalone() {
    let codex_home = PathBuf::from("/tmp/codex-home-does-not-matter");
    let resolved = daemon_codex_bin(&codex_home);
    let current_exe = std::env::current_exe().expect("current test binary");
    assert_eq!(resolved, current_exe);
    assert_ne!(resolved, managed_codex_bin(&codex_home));
}

#[test]
fn app_server_version_matches_this_cli_package_version() {
    assert!(app_server_version_matches_cli(env!("CARGO_PKG_VERSION")));
    assert!(!app_server_version_matches_cli("0.0.0-not-this-build"));
}
