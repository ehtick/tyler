#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

usage() {
  echo "Usage: $0 METADATA_FILE FEATURE_FOLDER [OUTPUT_FILE]" >&2
}

if [[ $# -lt 2 || $# -gt 3 ]]; then
  usage
  exit 1
fi

metadata_file="$1"
input_dir="$2"
output_file="${3:-${script_dir}/combined.city.jsonl}"
tmp_file="$(mktemp "${output_file}.tmp.XXXXXX")"

cleanup() {
  rm -f "${tmp_file}"
}
trap cleanup EXIT

if [[ ! -f "${metadata_file}" ]]; then
  echo "Missing ${metadata_file}" >&2
  exit 1
fi

if [[ ! -d "${input_dir}" ]]; then
  echo "Missing ${input_dir}" >&2
  exit 1
fi

append_file() {
  local file="$1"

  jq -c 'if .type == "CityJSONFeature" and ((has("id") | not) or .id == null) then . + {id: (.CityObjects | keys_unsorted | .[0])} else . end' "${file}" >> "${tmp_file}"
}

jq -c . "${metadata_file}" > "${tmp_file}"

while IFS= read -r -d '' file; do
  append_file "${file}"
done < <(find "${input_dir}" -maxdepth 1 -type f -name '*.city.jsonl' -print0 | LC_ALL=C sort -z)

mv "${tmp_file}" "${output_file}"
trap - EXIT

echo "Wrote ${output_file}"
