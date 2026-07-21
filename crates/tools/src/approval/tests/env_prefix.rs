use super::*;

// Layer 1: check_dangerous regex hits

#[test]
fn test_dangerous_ld_preload() {
    assert_eq!(
        check_dangerous("LD_PRELOAD=/evil.so cat /etc/passwd"),
        Some("dangerous dynamic linker env var"),
    );
}

#[test]
fn test_dangerous_ld_library_path() {
    assert_eq!(
        check_dangerous("LD_LIBRARY_PATH=/tmp cat /file"),
        Some("dangerous dynamic linker env var"),
    );
}

#[test]
fn test_dangerous_ld_audit() {
    assert_eq!(
        check_dangerous("LD_AUDIT=/evil.so ls"),
        Some("dangerous dynamic linker env var"),
    );
}

#[test]
fn test_dangerous_dyld_insert_libraries() {
    assert_eq!(
        check_dangerous("DYLD_INSERT_LIBRARIES=/evil.dylib cat /etc/passwd"),
        Some("dangerous macOS dynamic linker env var"),
    );
}

#[test]
fn test_dangerous_dyld_library_path() {
    assert_eq!(
        check_dangerous("DYLD_LIBRARY_PATH=/tmp ls"),
        Some("dangerous macOS dynamic linker env var"),
    );
}

#[test]
fn test_dangerous_path_override() {
    assert_eq!(
        check_dangerous("PATH=/tmp:$PATH cat /etc/passwd"),
        Some("PATH override"),
    );
}

#[test]
fn test_dangerous_pythonpath() {
    assert_eq!(
        check_dangerous("PYTHONPATH=/evil python3 -c 'import os'"),
        Some("dangerous language runtime env var"),
    );
}

#[test]
fn test_dangerous_node_options() {
    assert_eq!(
        check_dangerous("NODE_OPTIONS='--require=/evil.js' node app.js"),
        Some("dangerous language runtime env var"),
    );
}

#[test]
fn test_dangerous_java_tool_options() {
    assert_eq!(
        check_dangerous("JAVA_TOOL_OPTIONS=-javaagent:/evil.jar java Main"),
        Some("dangerous language runtime env var"),
    );
}

#[test]
fn test_dangerous_java_options_variants() {
    assert_eq!(
        check_dangerous("_JAVA_OPTIONS=-javaagent:/evil.jar java Main"),
        Some("dangerous language runtime env var"),
    );
    assert_eq!(
        check_dangerous("JDK_JAVA_OPTIONS=-javaagent:/evil.jar java Main"),
        Some("dangerous language runtime env var"),
    );
}

#[test]
fn test_dangerous_perl5opt() {
    assert_eq!(
        check_dangerous("PERL5OPT=-M/evil perl -e1"),
        Some("dangerous language runtime env var"),
    );
}

#[test]
fn test_dangerous_rubyopt() {
    assert_eq!(
        check_dangerous("RUBYOPT=-r/evil ruby -e1"),
        Some("dangerous language runtime env var"),
    );
}

#[test]
fn test_dangerous_bash_env() {
    assert_eq!(
        check_dangerous("BASH_ENV=/evil.sh bash -c 'echo hi'"),
        Some("dangerous shell startup env var"),
    );
}

#[test]
fn test_dangerous_env_var_in_subshell_not_caught_by_regex() {
    // Anchored regex patterns intentionally do NOT match env vars inside
    // quoted subshell arguments. This is safe because sh/bash are not safe
    // bins and require approval via the mode/allowlist path.
    assert!(check_dangerous(r#"sh -c "LD_PRELOAD=/evil.so cat /etc/passwd""#).is_none());
}

#[test]
fn test_dangerous_env_var_after_separator() {
    // Patterns still fire after command separators.
    assert_eq!(
        check_dangerous("echo hi; LD_PRELOAD=/evil.so cat /file"),
        Some("dangerous dynamic linker env var"),
    );
    assert_eq!(
        check_dangerous("true && PATH=/evil:$PATH cmd"),
        Some("PATH override"),
    );
}

#[test]
fn test_dangerous_env_var_case_insensitive() {
    assert_eq!(
        check_dangerous("ld_preload=/evil.so cat /file"),
        Some("dangerous dynamic linker env var"),
    );
}

#[test]
fn test_benign_env_var_not_flagged() {
    // Variables whose names are not in the dangerous list.
    assert!(check_dangerous("FOO=bar echo hi").is_none());
    assert!(check_dangerous("RUST_LOG=debug cargo test").is_none());
    assert!(check_dangerous("MY_LD_PRELOAD_FLAG=1 echo hi").is_none());
    // LD_DEBUG is diagnostic only (no code injection) — not flagged.
    assert!(check_dangerous("LD_DEBUG=bindings ./myprogram").is_none());
    // Bare ENV is too noisy (ENV=test, ENV=production) — not flagged.
    assert!(check_dangerous("ENV=test rake test").is_none());
    assert!(check_dangerous("ENV=production ./server").is_none());
}

#[test]
fn test_no_false_positive_on_grep_sed_arguments() {
    // Regression test: env var names inside grep/sed/awk arguments must
    // NOT trigger the regex. The (?:^|[;&|]\s*) anchor prevents this.
    assert!(check_dangerous("grep 'PATH=' ~/.bashrc").is_none());
    assert!(check_dangerous(r#"grep "PATH=" .env"#).is_none());
    assert!(check_dangerous("sed -n '/PATH=/p' .env").is_none());
    assert!(check_dangerous("awk -F'PATH=' '{print $2}' file").is_none());
    assert!(check_dangerous("grep 'LD_PRELOAD=' config.txt").is_none());
    assert!(check_dangerous("grep 'NODE_OPTIONS=' .env").is_none());
    // Unquoted grep with empty value — also benign.
    assert!(check_dangerous("grep PATH= file").is_none());
    // Unquoted grep with value — also benign (the `PATH=` is an argument,
    // not a shell assignment).
    assert!(check_dangerous("grep PATH=/usr/bin .env").is_none());
    assert!(check_dangerous("grep LD_PRELOAD=/path config.txt").is_none());
}

// Layer 2: extract_first_bin semantic check

#[test]
fn test_extract_first_bin_dangerous_prefix_returns_none() {
    assert_eq!(extract_first_bin("LD_PRELOAD=/evil.so cat /file"), None);
    assert_eq!(extract_first_bin("DYLD_INSERT_LIBRARIES=/e.dylib ls"), None);
    assert_eq!(extract_first_bin("PATH=/tmp:$PATH cat /etc/passwd"), None);
    assert_eq!(extract_first_bin("NODE_OPTIONS=--evil node app.js"), None);
    assert_eq!(extract_first_bin("BASH_ENV=/evil.sh bash -c hi"), None);
}

#[test]
fn test_extract_first_bin_dangerous_case_insensitive() {
    assert_eq!(extract_first_bin("ld_preload=/evil.so cat /file"), None);
    assert_eq!(extract_first_bin("Ld_Preload=/evil.so cat /file"), None);
}

#[test]
fn test_extract_first_bin_benign_prefix_still_works() {
    assert_eq!(extract_first_bin("FOO=bar echo hi"), Some("echo"));
    assert_eq!(
        extract_first_bin("RUST_LOG=debug cargo test"),
        Some("cargo"),
    );
    assert_eq!(extract_first_bin("CC=gcc CXX=g++ cmake .."), Some("cmake"),);
}

#[test]
fn test_extract_first_bin_non_dangerous_ld_prefix() {
    // MY_LD_PRELOAD_FLAG contains "LD_PRELOAD" as substring but the key
    // itself is not a match.
    assert_eq!(
        extract_first_bin("MY_LD_PRELOAD_FLAG=1 echo hi"),
        Some("echo"),
    );
}

#[test]
fn test_extract_first_bin_quoted_token_limitation() {
    // Quoted env-var assignments bypass Layer 2 because split_once('=')
    // sees key `"LD_PRELOAD` (with leading `"`). The anchored regex
    // (Layer 1) also does not match here because `"` is not whitespace.
    // In practice, shells interpret `"LD_PRELOAD=/evil.so"` as a command
    // name, not an env-var assignment, so this is not an exploitable
    // vector. And sh/bash subshell wrappers require approval separately.
    assert_eq!(
        extract_first_bin(r#""LD_PRELOAD=/evil.so" cat /file"#),
        Some("cat"),
    );
    assert!(check_dangerous(r#""LD_PRELOAD=/evil.so" cat /file"#).is_none());
}

#[test]
fn test_extract_first_bin_ld_debug_and_env_are_benign() {
    // LD_DEBUG and ENV were intentionally excluded from DANGEROUS_ENV_VARS.
    assert_eq!(
        extract_first_bin("LD_DEBUG=bindings ./myprogram"),
        Some("myprogram"),
    );
    assert_eq!(extract_first_bin("ENV=test rake test"), Some("rake"));
}

// is_safe_command rejects dangerous-prefixed safe commands

#[test]
fn test_is_safe_command_rejects_dangerous_prefix() {
    assert!(!is_safe_command("LD_PRELOAD=/evil.so cat /etc/passwd"));
    assert!(!is_safe_command("PATH=/tmp echo hi"));
    assert!(!is_safe_command("DYLD_INSERT_LIBRARIES=/e.dylib ls"));
}

#[test]
fn test_is_safe_command_allows_benign_prefix() {
    assert!(is_safe_command("FOO=bar echo hi"));
    assert!(is_safe_command("RUST_LOG=debug cat file.txt"));
}

// matches_allowlist rejects dangerous-prefixed allowlisted commands

#[test]
fn test_allowlist_rejects_dangerous_prefix() {
    let list = vec!["cat".into(), "ls".into()];
    assert!(!matches_allowlist("LD_PRELOAD=/evil.so cat /file", &list));
    assert!(!matches_allowlist("PATH=/tmp ls", &list));
}

#[test]
fn test_allowlist_wildcard_still_matches_dangerous_prefix() {
    // Wildcard `*` overrides everything — existing documented behavior.
    let list = vec!["*".into()];
    assert!(matches_allowlist("LD_PRELOAD=/evil.so cat /file", &list));
}

#[test]
fn test_allowlist_prefix_no_bypass_via_chained_assignment() {
    // Regression: `command.starts_with(prefix)` must not match when
    // extract_first_bin returned None due to a dangerous env var.
    let list = vec!["MY_APP*".into()];
    assert!(!matches_allowlist(
        "MY_APP=1 LD_PRELOAD=/evil.so cat /file",
        &list,
    ));
    // But benign chained assignments still match.
    assert!(matches_allowlist("MY_APP run --flag", &list));
}

// check_command integration tests

#[tokio::test]
async fn test_env_injection_needs_approval_on_miss() {
    let mgr = ApprovalManager::new(ApprovalMode::OnMiss);
    let action = mgr
        .check_command("LD_PRELOAD=/evil.so cat /etc/passwd")
        .await
        .unwrap();
    assert_eq!(action, ApprovalAction::NeedsApproval);
}

#[tokio::test]
async fn test_env_injection_denied_in_never_mode() {
    let mgr = ApprovalManager::new(ApprovalMode::Never);
    let err = mgr
        .check_command("LD_PRELOAD=/evil.so cat /etc/passwd")
        .await
        .expect_err("expected denial for env injection in never mode");
    assert!(
        err.to_string().contains("dangerous"),
        "unexpected error message: {err}"
    );
}

#[tokio::test]
async fn test_chained_env_injection_denied_in_never_empty_allowlist() {
    // Regression: chained assignments must be caught by Layer 2 safety
    // floor even when Never+empty-allowlist short-circuits before mode check.
    let mgr = ApprovalManager::new(ApprovalMode::Never);
    let err = mgr
        .check_command("FOO=bar LD_PRELOAD=/evil.so cat /etc/passwd")
        .await
        .expect_err("expected denial for chained env injection in never mode");
    assert!(
        err.to_string().contains("dangerous env-var prefix"),
        "unexpected error message: {err}"
    );
}

#[tokio::test]
async fn test_chained_env_injection_needs_approval_on_miss() {
    let mgr = ApprovalManager::new(ApprovalMode::OnMiss);
    let action = mgr
        .check_command("FOO=bar LD_PRELOAD=/evil.so cat /etc/passwd")
        .await
        .unwrap();
    assert_eq!(action, ApprovalAction::NeedsApproval);
}

#[tokio::test]
async fn test_env_injection_needs_approval_always_mode() {
    let mgr = ApprovalManager::new(ApprovalMode::Always);
    let action = mgr
        .check_command("DYLD_INSERT_LIBRARIES=/evil.dylib ls")
        .await
        .unwrap();
    assert_eq!(action, ApprovalAction::NeedsApproval);
}

#[tokio::test]
async fn test_benign_prefix_proceeds_on_miss() {
    let mgr = ApprovalManager::new(ApprovalMode::OnMiss);
    let action = mgr.check_command("RUST_LOG=debug echo hi").await.unwrap();
    assert_eq!(action, ApprovalAction::Proceed);
}

#[tokio::test]
async fn test_env_injection_in_subshell_needs_approval() {
    // Regex doesn't match inside quotes, but sh is not a safe bin so
    // the mode check (OnMiss) still requires approval.
    let mgr = ApprovalManager::new(ApprovalMode::OnMiss);
    let action = mgr
        .check_command(r#"sh -c "LD_PRELOAD=/evil.so cat /etc/passwd""#)
        .await
        .unwrap();
    assert_eq!(action, ApprovalAction::NeedsApproval);
}

#[tokio::test]
async fn test_env_injection_with_explicit_allowlist_wildcard() {
    // Wildcard allowlist still overrides dangerous env var patterns.
    let mut mgr = ApprovalManager::new(ApprovalMode::OnMiss);
    mgr.allowlist = vec!["*".into()];
    let action = mgr
        .check_command("LD_PRELOAD=/evil.so cat /etc/passwd")
        .await
        .unwrap();
    // Dangerous pattern matched, but allowlist wildcard overrides -> falls
    // through to the mode check where safe-bin check fails (extract_first_bin
    // returns None) -> NeedsApproval in explicit OnMiss mode.
    // Actually: check_dangerous fires, matches_allowlist("*") returns true,
    // so the dangerous block is skipped. Then security_level=Allowlist,
    // mode=OnMiss. is_safe_command returns false (extract_first_bin -> None).
    // matches_allowlist("*") returns true -> Proceed.
    assert_eq!(action, ApprovalAction::Proceed);
}
