#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

bin_input="${TOMTE_BIN:-target/release/tomte}"
timeout_cmd="${TOMTE_SMOKE_TIMEOUT:-180s}"

fail() {
  printf 'smoke failed: %s\n' "$*" >&2
  exit 1
}

contains() {
  local haystack="$1"
  local needle="$2"
  [[ "$haystack" == *"$needle"* ]]
}

run_with_clean_stderr() {
  local label="$1"
  shift
  local out
  local err
  out="$(mktemp)"
  err="$(mktemp)"
  if ! "$@" >"$out" 2>"$err"; then
    printf '%s stderr:\n' "$label" >&2
    tail -n 40 "$err" >&2 || true
    fail "$label exited non-zero"
  fi
  if [[ "$(wc -c <"$err")" != "0" ]]; then
    printf '%s stderr:\n' "$label" >&2
    cat "$err" >&2
    fail "$label wrote to stderr"
  fi
  cat "$out"
}

version="$(awk -F '"' '/^version = / { print $2; exit }' Cargo.toml)"

case "$bin_input" in
  /*) bin="$bin_input" ;;
  */*) bin="$repo_root/$bin_input" ;;
  *)
    bin="$(command -v "$bin_input" || true)"
    [[ -n "$bin" ]] || fail "TOMTE_BIN not found on PATH: $bin_input"
    ;;
esac

if [[ ! -x "$bin" ]]; then
  cargo build --release --bin tomte
fi

actual_version="$("$bin" --version)"
contains "$actual_version" "tomte $version" || {
  fail "expected tomte $version, got: $actual_version"
}
printf 'ok version: %s\n' "$actual_version"

tmp_root="$(mktemp -d)"
trap 'rm -rf "$tmp_root"' EXIT

# A retired id (gpt-5-pro no longer resolves at the API) is normalized to its
# current equivalent; the `openai/` prefix is stripped. (gpt-5 and gpt-5.2 are
# real current models and are intentionally NOT remapped.)
openai_cfg="$(XDG_CONFIG_HOME="$tmp_root/openai-config" "$bin" config --set-model openai/gpt-5-pro --set-reasoning max --show)"
contains "$openai_cfg" '"model": "gpt-5.5-pro"' || fail "OpenAI model normalization missing"
contains "$openai_cfg" '"reasoning_effort": "max"' || fail "OpenAI max reasoning should persist"
printf 'ok config: OpenAI legacy model normalized\n'

anthropic_cfg="$(XDG_CONFIG_HOME="$tmp_root/anthropic-config" "$bin" config --set-model anthropic/claude-opus-4-7 --set-reasoning max --show)"
contains "$anthropic_cfg" '"model": "claude-opus-4-7"' || fail "Anthropic model normalization missing"
contains "$anthropic_cfg" '"reasoning_effort": "xhigh"' || fail "Anthropic max should persist as xhigh"
printf 'ok config: Anthropic max persistence downgraded\n'

auth_home="$tmp_root/auth-preservation"
mkdir -p "$auth_home/tomte"
printf '%s\n' '{"mode":"openai_oauth","tokens":{"access_token":"oauth-token","refresh_token":"refresh-token"}}' > "$auth_home/tomte/auth.json"
printf 'sk-test\n' | XDG_CONFIG_HOME="$auth_home" "$bin" login --api-key --provider openai >/dev/null 2>/dev/null
auth_status="$(XDG_CONFIG_HOME="$auth_home" "$bin" status)"
contains "$auth_status" 'OpenAI OAuth token is also stored' || fail "OpenAI OAuth was not preserved after API key login"
contains "$auth_status" 'OpenAI API key:     stored' || fail "OpenAI API key status missing after login"
printf 'ok auth: API key login preserves OAuth credential\n'

if ! package_output="$("$repo_root/scripts/package-release.sh" 2>&1)"; then
  printf '%s\n' "$package_output" >&2
  fail "package script failed"
fi
rustc_version="$(rustc -vV)"
host_target="$(printf '%s\n' "$rustc_version" | awk '/^host: / { print $2; exit }')"
archive="dist/tomte-${host_target}.tar.gz"
[[ -f "$archive" ]] || fail "package archive missing: $archive"
if command -v sha256sum >/dev/null 2>&1; then
  sha256sum -c "$archive.sha256" >/dev/null
else
  shasum -a 256 -c "$archive.sha256" >/dev/null
fi
extract_dir="$tmp_root/package"
mkdir -p "$extract_dir"
tar -xzf "$archive" -C "$extract_dir"
packaged_bin="$(find "$extract_dir" -type f -name tomte -perm -111 -print -quit)"
[[ -n "$packaged_bin" ]] || fail "packaged tomte binary missing"
packaged_version="$("$packaged_bin" --version)"
contains "$packaged_version" "tomte $version" || fail "packaged binary version mismatch: $packaged_version"
printf 'ok package: archive checksum and binary version verified\n'

run_live_json_chat() {
  local label="$1"
  local model="$2"
  local marker="$3"
  local prompt="$4"
  local out
  out="$(run_with_clean_stderr "$label" timeout "$timeout_cmd" "$bin" chat --model "$model" --reasoning none --output-format json "$prompt")"
  contains "$out" "$marker" || fail "$label did not contain marker $marker"
  printf 'ok live: %s\n' "$label"
}

run_live_tool_chat() {
  local label="$1"
  local model="$2"
  local marker="$3"
  local tmp
  tmp="$(mktemp -d)"
  printf '%s\n' "$marker" > "$tmp/marker.txt"
  local out
  out="$(
    cd "$tmp"
    run_with_clean_stderr "$label" timeout "$timeout_cmd" "$bin" chat --model "$model" --reasoning none --output-format json "Use the read_file tool to read marker.txt, then reply exactly $marker and nothing else."
  )"
  contains "$out" "$marker" || fail "$label did not contain marker $marker"
  contains "$out" "ToolCallStarted" || fail "$label did not start a tool call"
  printf 'ok live: %s\n' "$label"
}

if [[ "${TOMTE_LIVE_SMOKE:-}" == "1" ]]; then
  openai_model="${TOMTE_SMOKE_OPENAI_MODEL:-openai/gpt-5.5}"
  anthropic_model="${TOMTE_SMOKE_ANTHROPIC_MODEL:-anthropic/claude-haiku-4-5}"
  run_live_json_chat "OpenAI JSON chat" "$openai_model" "TOMTE_SMOKE_OPENAI_OK" "Reply exactly TOMTE_SMOKE_OPENAI_OK and nothing else."
  run_live_tool_chat "OpenAI read_file tool" "$openai_model" "TOMTE_SMOKE_OPENAI_TOOL_OK"
  run_live_json_chat "Anthropic JSON chat" "$anthropic_model" "TOMTE_SMOKE_ANTHROPIC_OK" "Reply exactly TOMTE_SMOKE_ANTHROPIC_OK and nothing else."
  run_live_tool_chat "Anthropic read_file tool" "$anthropic_model" "TOMTE_SMOKE_ANTHROPIC_TOOL_OK"
else
  printf 'skip live provider smoke: set TOMTE_LIVE_SMOKE=1 to enable\n'
fi

printf 'smoke ok\n'
