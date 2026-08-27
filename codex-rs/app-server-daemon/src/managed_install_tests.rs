use std::path::PathBuf;

use pretty_assertions::assert_eq;

use super::app_server_version_matches_cli;
use super::daemon_codex_bin;
use super::executable_identity_from_bytes;
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

#[test]
fn executable_identity_uses_binary_contents() {
    let old = executable_identity_from_bytes(b"old");
    let same = executable_identity_from_bytes(b"old");
    let new = executable_identity_from_bytes(b"new");

    assert_eq!(old, same);
    assert_ne!(old, new);
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
