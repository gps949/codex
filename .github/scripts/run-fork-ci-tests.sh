#!/usr/bin/env bash
# Both phases use the same package and test selection so feature unification stays identical.
set -euo pipefail
phase="${1:?expected build or run}"
case "$phase" in
  build) phase_args=(--no-run) ;;
  run) phase_args=(--no-fail-fast) ;;
  *) echo "Unknown test phase: $phase" >&2; exit 2 ;;
esac

cargo nextest run "${phase_args[@]}" \
  -p codex-backend-client -p codex-login -p codex-config -p codex-history -p codex-protocol -p codex-app-server-protocol \
  -p codex-model-provider -p codex-models-manager -p codex-core -p codex-tui -p codex-cli -p codex-app-server -p codex-app-server-transport -p codex-app-server-daemon -p codex-app-server-client -p codex-install-context \
  -E '
    (package(=codex-backend-client) and test(credit_expiry)) or
    package(=codex-login) or
    package(=codex-config) or
    package(=codex-history) or
    package(=codex-app-server-daemon) or
    package(=codex-app-server-client) or
    package(=codex-install-context) or
    package(=codex-model-provider) or
    (package(=codex-protocol) and test(usage_limit)) or
    package(=codex-app-server-protocol) or
    (package(=codex-models-manager) and (test(warmup) or test(routing_catalog))) or
    (package(=codex-core) and (kind(lib) or (kind(test) and binary(=all))) and (test(client::tests) or test(compact) or test(suite::model_request::) or test(model_routing_observation) or test(task_model_routing) or test(account_failover) or test(failover) or test(account_transition) or test(tool_output_provenance) or test(preemptive) or test(account_request_auth) or test(opaque_history_migration) or test(account_window_warmup) or test(execution_auth) or test(account_pool_strategy) or test(account_pool_recovery) or test(account_pool_metadata_recovery) or test(account_recovered_reentry) or test(reset_credit) or test(captured_maintenance) or test(model_specific_limit) or test(durable_) or test(api_account_execution) or test(credit_expiry) or test(decision_advisor) or test(skill_advisor) or test(skill_suggestion) or test(tool_search) or test(search_tool))) or
    (package(=codex-tui) and (kind(lib) or (kind(test) and binary(=all))) and (test(new_session_runs_independently_when_daemon_features_conflict) or test(no_daemon_skips_startup_and_discovery) or test(daemon_recovery_requires_explicit_restart) or test(account) or test(warmup) or test(selection_row_layout) or test(model_routing) or test(model_selection) or test(model_auto) or test(reset_credit) or test(chatwidget::tests::usage))) or
    (package(=codex-cli) and ((kind(test) and ((binary(=app_server) and test(print_startup_config_sign_out_preserves_enrolled_pool_credentials)) or binary(=account_selection) or binary(=account_manager_names) or binary(=account_primary) or binary(=account_manager_preferences) or binary(=decision_advisor))) or (kind(bin) and binary(=codex) and (test(doctor::updates) or test(doctor::account_pool) or test(quota_observations) or test(account_cmd::quota_display) or test(account_cmd::display) or test(account_selector) or test(account_primary) or test(account_manager))))) or
    (package(=codex-app-server-transport) and (test(host_policy_retry) or test(primary_login_handover))) or
    (package(=codex-app-server) and (kind(lib) or (kind(test) and binary(=all))) and (test(model_selection) or test(reset_credit_journal) or test(logout_account) or test(logout_reloads_persisted) or test(account_pool) or test(mobile_account) or test(mobile_quota) or test(pool_warnings) or test(pool_warning) or test(guarded_pool_notification) or test(status_overlay) or test(workspace_routing) or test(rate_limit_reset) or test(rate_limits_identity) or test(native_status) or test(mobile_warmup) or test(get_account_rate_limits) or test(login_survives_same_owner_token_refresh) or test(account_management) or test(native_account) or test(primary_login) or test(host_login) or test(remote_control) or test(outgoing_owned_request) or test(user_verification_ownership)))
  '
