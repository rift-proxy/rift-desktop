#!/usr/bin/env bash

# Permanent fuzz targets (docs/build.md). The replay is a gate, so it
# must not pass without replaying anything: `cargo fuzz list` failing or printing nothing, a
# missing permanent target, or a seed directory without a single input all fail it.
FUZZ_REQUIRED_TARGETS=(subscription persisted_state ipc_payload clash_api helper_frame dns_query sidecar_log journal_details)

# Runs from fuzz after `cargo fuzz build`. Replays committed seeds and regressions only
# (-runs=0): nothing is mutated or written back.
fuzz_replay_targets() {
  local listed target required
  local -a targets inputs
  listed="$(cargo fuzz list)" || {
    echo "cargo fuzz list failed" >&2
    return 1
  }
  mapfile -t targets < <(printf '%s\n' "$listed" | sed '/^[[:space:]]*$/d')
  if [ "${#targets[@]}" -eq 0 ]; then
    echo "cargo fuzz list reported no targets" >&2
    return 1
  fi
  for required in "${FUZZ_REQUIRED_TARGETS[@]}"; do
    if ! printf '%s\n' "${targets[@]}" | grep -Fxq -- "$required"; then
      echo "Permanent fuzz target $required is not built by fuzz" >&2
      return 1
    fi
  done
  for target in "${targets[@]}"; do
    if [ ! -d "seeds/$target" ]; then
      echo "Fuzz target $target has no seed corpus in fuzz/seeds/$target" >&2
      return 1
    fi
    if [ -z "$(find "seeds/$target" -type f ! -name '.*' -print -quit)" ]; then
      echo "Fuzz target $target has an empty seed corpus in fuzz/seeds/$target" >&2
      return 1
    fi
    inputs=("seeds/$target")
    if [ -d "regressions/$target" ]; then
      inputs+=("regressions/$target")
    fi
    cargo fuzz run "$target" "${inputs[@]}" -- -runs=0 || return 1
  done
}
