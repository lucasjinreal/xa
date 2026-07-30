#!/usr/bin/env bash
# Download a completed GitHub Actions wheel build and publish it with local Twine
# credentials.  Authentication is deliberately left to ~/.pypirc or keyring.
set -euo pipefail

workflow="${PYPI_WORKFLOW:-pypi.yml}"
repository="${TWINE_REPOSITORY:-pypi}"
run_id="${1:-}"
expected_wheels="${EXPECTED_WHEELS:-6}"

if ! command -v gh >/dev/null; then
  echo "gh CLI is required; install it and run: gh auth login" >&2
  exit 1
fi

if ! python -m twine --version >/dev/null 2>&1; then
  echo "Twine is required; install it with: python -m pip install --upgrade twine" >&2
  exit 1
fi

if [[ -z "$run_id" ]]; then
  run_id="$(
    gh run list --workflow "$workflow" --limit 20 --json databaseId,conclusion \
      --jq '.[] | select(.conclusion == "success") | .databaseId' | sed -n '1p'
  )"
fi

if [[ -z "$run_id" ]]; then
  echo "No successful run for $workflow was found. Pass a run ID explicitly." >&2
  exit 1
fi

download_dir="$(mktemp -d "${TMPDIR:-/tmp}/xacli-pypi-${run_id}.XXXXXX")"
trap 'rm -rf "$download_dir"' EXIT

echo "Downloading wheels from GitHub Actions run $run_id..."
gh run download "$run_id" --pattern 'wheel-*' --dir "$download_dir"

wheels=()
while IFS= read -r -d '' wheel; do
  wheels+=("$wheel")
done < <(find "$download_dir" -type f -name '*.whl' -print0)

if (( ${#wheels[@]} != expected_wheels )); then
  echo "Expected $expected_wheels wheel(s), downloaded ${#wheels[@]}; refusing partial upload." >&2
  printf '  %s\n' "${wheels[@]:-<none>}" >&2
  exit 1
fi

echo "Checking ${#wheels[@]} wheel(s)..."
python -m twine check "${wheels[@]}"

echo "Uploading to the '$repository' repository with your local Twine credentials..."
python -m twine upload --repository "$repository" "${wheels[@]}"
