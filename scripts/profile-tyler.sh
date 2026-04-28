#!/usr/bin/env bash
set -euo pipefail

readonly DEFAULT_OUTPUT_ROOT="docs/performance/runs"
readonly DEFAULT_GEODEPOT_ENV_FILE=".env"
readonly DEFAULT_RUNNER="all"
readonly DEFAULT_RAW_OUTPUT_SUBDIR="raw"
readonly PROFILE_CONFIG_BASENAME="profile-tyler.json"
readonly PROFILE_CONFIG_TYPO_BASENAME="profile-tiler.json"
readonly RUSTFLAGS_VALUE="-C debuginfo=2 -C force-frame-pointers=yes"
readonly PERF_EVENTS="cycles,instructions,cache-references,cache-misses,page-faults,context-switches,cpu-migrations,branches,branch-misses"
readonly MASSIF_TIME_UNIT="B"

die() {
    printf 'error: %s\n' "$*" >&2
    exit 1
}

usage() {
    cat <<'EOF'
Usage: profile-tyler.sh [--profile CASESPEC] [--input PATH] [--label LABEL] [--output-root DIR] [--runner MODE]

Defaults:
  --output-root docs/performance/runs
  --runner      all
  .env          Provides GEODEPOT_BIN for profile resolution

Parameters:
  -h, --help       Show this help text and exit.
  --profile NAME   Geodepot case spec. The script resolves NAME to the input
                   root and reads CASE/profile-tyler.json for tyler args,
                   where CASE is the left side of the `/` separator.
  --input PATH     Dataset root to profile. Must be an existing tyler input root.
  --label LABEL    Optional run label appended to the run directory name.
  --output-root DIR  Directory that receives the per-run profiling artifacts.
  --runner MODE    Select which profiler(s) to run: perf, massif, all, or
                   none. The default is all.

The script builds tyler in release mode with debuginfo and frame pointers,
then captures perf stat and Valgrind Massif summaries into a new run directory.
When --profile is provided, the script reads the geodepot-resolved
profile-tyler.json data item for the matching tyler command-line arguments.
Tyler's own output, together with the raw perf and Massif dumps, is written to
an ignored sibling directory under the output root. The committed run
directory keeps the JSON summaries and markdown note.

Examples:
  just profile -- --profile bvz-dh-coast-5/bvz_dh
  just profile -- --profile bvz-dh-coast-5/bvz_dh --runner perf
  just profile -- --profile bvz-dh-coast-5/bvz_dh --runner massif
  just profile -- --profile bvz-dh-coast-5/bvz_dh --runner none
  just profile -- --profile bvz-dh-coast-5/bvz_dh --output-root docs/performance/runs
  just profile -- --input /data/cases/demo --label manual-smoke
EOF
}

sanitize_label() {
    local label="$1"
    label="${label//[^A-Za-z0-9._-]/_}"
    label="${label#_}"
    label="${label%_}"
    printf '%s' "${label:-run}"
}

require_tool() {
    local tool="$1"
    command -v "$tool" >/dev/null 2>&1 || die "required tool not found: $tool"
}

require_executable() {
    local path="$1"
    [[ -x "$path" ]] || die "required executable not found: $path"
}

json_bool() {
    local value="$1"
    if [[ "$value" == "true" ]]; then
        printf 'true'
    else
        printf 'false'
    fi
}

resolve_path() {
    local path="$1"
    (cd "$path" && pwd -P)
}

resolve_file_path() {
    local path="$1"
    local dir
    dir="$(dirname -- "$path")"
    printf '%s/%s\n' "$(resolve_path "$dir")" "$(basename -- "$path")"
}

format_ratio() {
    local numerator="$1"
    local denominator="$2"
    awk -v numerator="$numerator" -v denominator="$denominator" 'BEGIN {
        if (denominator == 0) {
            printf "n/a"
        } else {
            printf "%.4f", numerator / denominator
        }
    }'
}

format_mib() {
    local bytes="$1"
    awk -v bytes="$bytes" 'BEGIN { printf "%.2f", bytes / 1048576 }'
}

json_array_from_args() {
    if (($# == 0)); then
        printf '[]'
    else
        printf '%s\0' "$@" | jq -Rs 'split("\u0000")[:-1]'
    fi
}

print_command() {
    local part
    for part in "$@"; do
        printf '%q ' "$part"
    done
    printf '\n'
}

load_env_file() {
    local env_file="$1"
    [[ -f "$env_file" ]] || return 0
    # shellcheck disable=SC1090
    . "$env_file"
}

profile_case_root() {
    local profile_spec="$1"
    printf '%s\n' "${profile_spec%%/*}"
}

resolve_profile_file() {
    local geodepot_bin="$1"
    local profile_spec="$2"
    local profile_case_root_value
    local candidate_path
    local resolved_path

    profile_case_root_value="$(profile_case_root "$profile_spec")"
    for candidate_path in \
        "$profile_case_root_value/$PROFILE_CONFIG_BASENAME" \
        "$profile_case_root_value/$PROFILE_CONFIG_TYPO_BASENAME"
    do
        if resolved_path="$("$geodepot_bin" get "$candidate_path" 2>/dev/null)"; then
            [[ -n "$resolved_path" ]] || continue
            printf '%s\n' "$resolved_path"
            return 0
        fi
    done

    printf 'error: could not resolve profile config for %s (looked for %s and %s)\n' \
        "$profile_spec" \
        "$PROFILE_CONFIG_BASENAME" \
        "$PROFILE_CONFIG_TYPO_BASENAME" >&2
    return 1
}

input_root=""
profile_spec=""
profile_input_root=""
run_label=""
output_root="$DEFAULT_OUTPUT_ROOT"
runner_selector="$DEFAULT_RUNNER"

while (($#)); do
    case "$1" in
        -h|--help)
            usage
            exit 0
            ;;
        --profile)
            [[ $# -ge 2 ]] || die "--profile requires a value"
            profile_spec="$2"
            shift 2
            ;;
        --input)
            [[ $# -ge 2 ]] || die "--input requires a path"
            input_root="$2"
            shift 2
            ;;
        --label)
            [[ $# -ge 2 ]] || die "--label requires a value"
            run_label="$2"
            shift 2
            ;;
        --output-root)
            [[ $# -ge 2 ]] || die "--output-root requires a path"
            output_root="$2"
            shift 2
            ;;
        --runner)
            [[ $# -ge 2 ]] || die "--runner requires a value"
            runner_selector="$2"
            shift 2
            ;;
        --)
            shift
            continue
            ;;
        *)
            die "unknown argument: $1"
            ;;
    esac
done

if (($#)); then
    die "unexpected positional arguments: $*"
fi

if [[ -n "$profile_spec" && -n "$input_root" ]]; then
    die "--profile cannot be combined with --input"
fi

if [[ -n "$profile_spec" && -n "$run_label" ]]; then
    die "--profile cannot be combined with --label"
fi

run_perf="false"
run_massif="false"
case "$runner_selector" in
    all)
        run_perf="true"
        run_massif="true"
        ;;
    perf)
        run_perf="true"
        ;;
    massif)
        run_massif="true"
        ;;
    none)
        ;;
    *)
        die "invalid --runner value: $runner_selector (expected perf, massif, all, or none)"
        ;;
esac

require_tool cargo
require_tool git
require_tool jq
if [[ "$run_perf" == "true" ]]; then
    require_tool perf
fi
if [[ "$run_massif" == "true" ]]; then
    require_tool valgrind
fi

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
repo_root="$(cd "$script_dir/.." && pwd -P)"

if [[ -z "${GEODEPOT_BIN:-}" ]]; then
    load_env_file "$repo_root/$DEFAULT_GEODEPOT_ENV_FILE"
fi

geodepot_bin="${GEODEPOT_BIN:-}"

if [[ -n "$profile_spec" ]]; then
    [[ -n "$geodepot_bin" ]] || die "GEODEPOT_BIN is required when using --profile; set it in $DEFAULT_GEODEPOT_ENV_FILE"
    require_executable "$geodepot_bin"

    profile_input_root="$("$geodepot_bin" get "$profile_spec" 2>/dev/null)" || die "could not resolve input root for profile: $profile_spec"
    [[ -n "$profile_input_root" ]] || die "could not resolve input root for profile: $profile_spec"

    if ! profile_file="$(resolve_profile_file "$geodepot_bin" "$profile_spec")"; then
        exit 1
    fi

    if [[ -z "$input_root" ]]; then
        input_root="$profile_input_root"
    fi
fi

[[ -n "$input_root" ]] || die "either --profile or --input is required"

if [[ "$input_root" != /* ]]; then
    input_root="$PWD/$input_root"
fi
input_root="$(resolve_path "$input_root")"

if [[ "$output_root" != /* ]]; then
    output_root="$repo_root/$output_root"
fi
mkdir -p "$output_root"
output_root="$(resolve_path "$output_root")"
raw_output_root="$output_root/$DEFAULT_RAW_OUTPUT_SUBDIR"
mkdir -p "$raw_output_root"

[[ -d "$input_root" ]] || die "input dataset root does not exist: $input_root"

profile_tyler_args_json='[]'
profile_env_json='{}'
profile_tyler_args=()
if [[ -n "${profile_file:-}" ]]; then
    [[ -f "$profile_file" ]] || die "profile config file does not exist: $profile_file"
    profile_tyler_args_json="$(
        jq -c '
            if type == "array" then .
            elif type == "object" then (.tyler_args // .args // [])
            else error("profile config must be a JSON array or object")
            end
        ' "$profile_file"
    )"
    profile_env_json="$(
        jq -c '
            if type == "object" then (.env // {})
            else {}
            end
        ' "$profile_file"
    )"
    mapfile -t profile_tyler_args < <(
        jq -r '
            .[]
            | tostring
        ' <<< "$profile_tyler_args_json"
    )
    while IFS=$'\t' read -r key value; do
        [[ -n "$key" ]] || continue
        export "$key=$value"
    done < <(
        jq -r '
            to_entries[]
            | [.key, (.value | tostring)]
            | @tsv
        ' <<< "$profile_env_json"
    )
fi

git_sha="$(git -C "$repo_root" rev-parse HEAD)"
git_short_sha="$(git -C "$repo_root" rev-parse --short HEAD)"
git_branch="$(git -C "$repo_root" rev-parse --abbrev-ref HEAD)"
git_describe="$(git -C "$repo_root" describe --tags --always --dirty 2>/dev/null || true)"
git_dirty="false"
if [[ -n "$(git -C "$repo_root" status --porcelain=v1)" ]]; then
    git_dirty="true"
fi

hostname_value="$(hostname)"
uname_value="$(uname -srvmo)"
rustc_version="$(rustc --version)"
cargo_version="$(cargo --version)"
perf_version="$(perf --version)"
valgrind_version="$(valgrind --version)"
created_at="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
timestamp="$(date -u +%Y-%m-%dT%H-%M-%S-%NZ)"
run_id="${timestamp}__pid$$"
if [[ -n "$profile_spec" ]]; then
    run_id+="__$(sanitize_label "$profile_spec")"
fi
if [[ -n "$run_label" ]]; then
    run_id+="__$(sanitize_label "$run_label")"
fi

final_run_dir="$output_root/$run_id"
[[ ! -e "$final_run_dir" ]] || die "run directory already exists: $final_run_dir"
raw_run_dir="$raw_output_root/$run_id"
[[ ! -e "$raw_run_dir" ]] || die "raw run directory already exists: $raw_run_dir"

staging_dir="$(mktemp -d "$output_root/.tyler-profile-staging.XXXXXX")"
finalized="false"
failed_run_dir=""
cleanup() {
    local exit_status="$?"
    local target_dir
    if [[ "$finalized" == "true" || ! -d "$staging_dir" ]]; then
        return 0
    fi

    if [[ "$exit_status" -eq 0 ]]; then
        rm -rf "$staging_dir"
        return 0
    fi

    target_dir="$final_run_dir.failed"
    if [[ -e "$target_dir" ]]; then
        target_dir="${target_dir}.$(date -u +%Y-%m-%dT%H-%M-%S-%NZ)"
    fi

    failed_run_dir="$target_dir"
    {
        printf 'run_id=%s\n' "$run_id"
        printf 'created_at=%s\n' "$created_at"
        printf 'runner=%s\n' "$runner_selector"
        printf 'exit_status=%s\n' "$exit_status"
        printf 'raw_run_dir=%s\n' "$raw_run_dir"
    } > "$staging_dir/FAILED.txt"

    if mv "$staging_dir" "$target_dir"; then
        printf 'Preserved failed profiling run in %s\n' "$target_dir" >&2
    else
        printf 'warning: failed profiling run could not be moved to %s; staging remains at %s\n' \
            "$target_dir" "$staging_dir" >&2
    fi
}
trap cleanup EXIT

binary_path="$repo_root/target/release/tyler"
build_command=(cargo build --release --locked --bin tyler --manifest-path "$repo_root/Cargo.toml")
tyler_output_base="$raw_run_dir/outputs"
mkdir -p "$tyler_output_base"

printf 'Building profiling binary...\n'
(
    cd "$repo_root"
    RUSTFLAGS="$RUSTFLAGS_VALUE" "${build_command[@]}"
)

[[ -x "$binary_path" ]] || die "expected profiling binary not found: $binary_path"

perf_output_dir="$tyler_output_base/perf"
massif_output_dir="$tyler_output_base/massif"
if [[ "$run_perf" == "true" ]]; then
    mkdir -p "$perf_output_dir"
fi
if [[ "$run_massif" == "true" ]]; then
    mkdir -p "$massif_output_dir"
fi

perf_elapsed_seconds=""
perf_cycles=""
perf_instructions=""
perf_cache_refs=""
perf_cache_misses=""
perf_page_faults=""
perf_context_switches=""
perf_cpu_migrations=""
perf_branches=""
perf_branch_misses=""
perf_raw_json=""
perf_stderr=""
perf_summary_json=""
tyler_perf_command=()
perf_command=()

if [[ "$run_perf" == "true" ]]; then
    perf_raw_json="$raw_run_dir/perf-stat.raw.json"
    perf_stderr="$raw_run_dir/perf-stat.stderr.log"
    tyler_perf_command=("$binary_path" "$input_root")
    tyler_perf_command+=( "${profile_tyler_args[@]}" )
    tyler_perf_command+=( --output "$perf_output_dir" )
    perf_command=(perf stat -j -e "$PERF_EVENTS" -o "$perf_raw_json" -- "${tyler_perf_command[@]}")

    printf 'Running perf stat...\n'
    printf 'Tyler command: ' >&2
    print_command "${tyler_perf_command[@]}" >&2
    perf_start_ns="$(date +%s%N)"
    set +e
    "${perf_command[@]}" > /dev/null 2> "$perf_stderr"
    perf_status="$?"
    set -e
    perf_end_ns="$(date +%s%N)"
    perf_elapsed_seconds="$(awk -v start="$perf_start_ns" -v end="$perf_end_ns" 'BEGIN { printf "%.6f", (end - start) / 1000000000 }')"
    if [[ "$perf_status" -ne 0 ]]; then
        die "perf run failed with exit status $perf_status; see $perf_stderr"
    fi

    perf_summary_json="$staging_dir/perf-stat.json"
    perf_summary_filter='
        def event_value($name):
            ([.[] | select(.event == $name)] | first)."counter-value" | tonumber;
        def ratio($numerator; $denominator):
            if $denominator == 0 then null else ($numerator / $denominator) end;
        {
            wall_clock_seconds: ($wall_clock_seconds | tonumber),
            counters: {
                cycles: event_value("cycles"),
                instructions: event_value("instructions"),
                instructions_per_cycle: ratio(event_value("instructions"); event_value("cycles")),
                cache_references: event_value("cache-references"),
                cache_misses: event_value("cache-misses"),
                cache_miss_rate: ratio(event_value("cache-misses"); event_value("cache-references")),
                page_faults: event_value("page-faults"),
                context_switches: event_value("context-switches"),
                cpu_migrations: event_value("cpu-migrations"),
                branches: event_value("branches"),
                branch_misses: event_value("branch-misses")
            },
            raw_events: .
        }
    '
    jq -s \
        --arg wall_clock_seconds "$perf_elapsed_seconds" \
        "$perf_summary_filter" "$perf_raw_json" > "$perf_summary_json"

    perf_cycles="$(jq -r '.counters.cycles' "$perf_summary_json")"
    perf_instructions="$(jq -r '.counters.instructions' "$perf_summary_json")"
    perf_cache_refs="$(jq -r '.counters.cache_references' "$perf_summary_json")"
    perf_cache_misses="$(jq -r '.counters.cache_misses' "$perf_summary_json")"
    perf_page_faults="$(jq -r '.counters.page_faults' "$perf_summary_json")"
    perf_context_switches="$(jq -r '.counters.context_switches' "$perf_summary_json")"
    perf_cpu_migrations="$(jq -r '.counters.cpu_migrations' "$perf_summary_json")"
    perf_branches="$(jq -r '.counters.branches' "$perf_summary_json")"
    perf_branch_misses="$(jq -r '.counters.branch_misses' "$perf_summary_json")"
else
    printf 'Skipping perf stat (runner=%s)\n' "$runner_selector"
fi

massif_elapsed_seconds=""
peak_heap_bytes=""
peak_snapshot=""
peak_time=""
snapshot_count=""
max_heap_growth_bytes=""
max_heap_growth_from_snapshot=""
max_heap_growth_to_snapshot=""
max_heap_growth_time=""
massif_raw=""
massif_stdout=""
massif_stderr=""
massif_summary_json=""
tyler_massif_command=()
massif_command=()

if [[ "$run_massif" == "true" ]]; then
    massif_raw="$raw_run_dir/massif.out"
    massif_stdout="$raw_run_dir/massif.stdout.log"
    massif_stderr="$raw_run_dir/massif.stderr.log"
    tyler_massif_command=("$binary_path" "$input_root")
    tyler_massif_command+=( "${profile_tyler_args[@]}" )
    tyler_massif_command+=( --output "$massif_output_dir" )
    massif_command=(valgrind --tool=massif --time-unit="$MASSIF_TIME_UNIT" --massif-out-file="$massif_raw" -- "${tyler_massif_command[@]}")

    printf 'Running Massif...\n'
    printf 'Tyler command: ' >&2
    print_command "${tyler_massif_command[@]}" >&2
    massif_start_ns="$(date +%s%N)"
    set +e
    "${massif_command[@]}" > "$massif_stdout" 2> "$massif_stderr"
    massif_status="$?"
    set -e
    massif_end_ns="$(date +%s%N)"
    massif_elapsed_seconds="$(awk -v start="$massif_start_ns" -v end="$massif_end_ns" 'BEGIN { printf "%.6f", (end - start) / 1000000000 }')"
    if [[ "$massif_status" -ne 0 ]]; then
        die "Massif run failed with exit status $massif_status; see $massif_stderr"
    fi

    read -r peak_heap_bytes peak_snapshot peak_time snapshot_count max_heap_growth_bytes max_heap_growth_from_snapshot max_heap_growth_to_snapshot max_heap_growth_time < <(
        awk '
            BEGIN {
                snapshot = -1;
                time = 0;
                peak_heap = -1;
                peak_snapshot = -1;
                peak_time = 0;
                snapshot_count = 0;
                max_heap_growth = -1;
                max_from = -1;
                max_to = -1;
                max_growth_time = 0;
                prev_heap = -1;
                prev_snapshot = -1;
            }
            /^snapshot=/ {
                snapshot = substr($0, 10) + 0;
                snapshot_count += 1;
                next;
            }
            /^time=/ {
                time = substr($0, 6) + 0;
                next;
            }
            /^mem_heap_B=/ {
                heap = substr($0, 12) + 0;
                if (heap > peak_heap) {
                    peak_heap = heap;
                    peak_snapshot = snapshot;
                    peak_time = time;
                }
                if (prev_heap >= 0) {
                    growth = heap - prev_heap;
                    if (growth > max_heap_growth) {
                        max_heap_growth = growth;
                        max_from = prev_snapshot;
                        max_to = snapshot;
                        max_growth_time = time;
                    }
                }
                prev_heap = heap;
                prev_snapshot = snapshot;
                next;
            }
            END {
                if (peak_heap < 0) {
                    peak_heap = 0;
                    peak_snapshot = 0;
                    peak_time = 0;
                }
                if (max_heap_growth < 0) {
                    max_heap_growth = 0;
                    max_from = 0;
                    max_to = 0;
                    max_growth_time = 0;
                }
                printf "%s %s %s %s %s %s %s %s\n", peak_heap, peak_snapshot, peak_time, snapshot_count, max_heap_growth, max_from, max_to, max_growth_time;
            }
        ' "$massif_raw"
    )

    massif_summary_json="$staging_dir/massif-summary.json"
    massif_summary_filter='
        {
            time_unit: $time_unit,
            peak_heap_bytes: $peak_heap_bytes,
            peak_heap_mib: ($peak_heap_bytes / 1048576),
            peak_snapshot: $peak_snapshot,
            peak_time: $peak_time,
            snapshot_count: $snapshot_count,
            max_heap_growth_bytes: $max_heap_growth_bytes,
            max_heap_growth_from_snapshot: $max_heap_growth_from_snapshot,
            max_heap_growth_to_snapshot: $max_heap_growth_to_snapshot,
            max_heap_growth_time: $max_heap_growth_time
        }
    '
    jq -n \
        --arg time_unit "$MASSIF_TIME_UNIT" \
        --argjson peak_heap_bytes "$peak_heap_bytes" \
        --argjson peak_snapshot "$peak_snapshot" \
        --argjson peak_time "$peak_time" \
        --argjson snapshot_count "$snapshot_count" \
        --argjson max_heap_growth_bytes "$max_heap_growth_bytes" \
        --argjson max_heap_growth_from_snapshot "$max_heap_growth_from_snapshot" \
        --argjson max_heap_growth_to_snapshot "$max_heap_growth_to_snapshot" \
        --argjson max_heap_growth_time "$max_heap_growth_time" \
        "$massif_summary_filter" > "$massif_summary_json"
else
    printf 'Skipping Massif (runner=%s)\n' "$runner_selector"
fi

peak_heap_mib="n/a"
max_heap_growth_mib="n/a"
perf_cache_miss_rate_display="n/a"
perf_ipc_display="n/a"
if [[ "$run_massif" == "true" ]]; then
    peak_heap_mib="$(format_mib "$peak_heap_bytes")"
    max_heap_growth_mib="$(format_mib "$max_heap_growth_bytes")"
fi
if [[ "$run_perf" == "true" ]]; then
    perf_cache_miss_rate_display="$(format_ratio "$perf_cache_misses" "$perf_cache_refs")"
    perf_ipc_display="$(format_ratio "$perf_instructions" "$perf_cycles")"
fi

manifest_json="$staging_dir/manifest.json"
jq_profile_tyler_args="$(json_array_from_args "${profile_tyler_args[@]}")"
jq_perf_command="$(json_array_from_args "${perf_command[@]}")"
jq_massif_command="$(json_array_from_args "${massif_command[@]}")"
jq_tyler_perf_command="$(json_array_from_args "${tyler_perf_command[@]}")"
jq_tyler_massif_command="$(json_array_from_args "${tyler_massif_command[@]}")"
jq -n \
    --arg run_id "$run_id" \
    --arg run_label "$run_label" \
    --arg created_at "$created_at" \
    --arg repo_root "$repo_root" \
    --arg input_root "$input_root" \
    --arg profile_spec "$profile_spec" \
    --arg profile_input_root "$profile_input_root" \
    --arg profile_file "${profile_file:-}" \
    --argjson profile_tyler_args "$jq_profile_tyler_args" \
    --argjson profile_env "$profile_env_json" \
    --arg output_root "$output_root" \
    --arg staging_dir "$staging_dir" \
    --arg run_dir "$final_run_dir" \
    --arg runner_selector "$runner_selector" \
    --arg binary_path "$binary_path" \
    --arg geodepot_bin "${geodepot_bin:-}" \
    --arg git_sha "$git_sha" \
    --arg git_short_sha "$git_short_sha" \
    --arg git_branch "$git_branch" \
    --arg git_describe "$git_describe" \
    --argjson git_dirty "$(json_bool "$git_dirty")" \
    --arg hostname "$hostname_value" \
    --arg uname "$uname_value" \
    --arg rustc_version "$rustc_version" \
    --arg cargo_version "$cargo_version" \
    --arg perf_version "$perf_version" \
    --arg valgrind_version "$valgrind_version" \
    --arg rustflags "$RUSTFLAGS_VALUE" \
    --arg perf_elapsed_seconds "$perf_elapsed_seconds" \
    --arg massif_elapsed_seconds "$massif_elapsed_seconds" \
    --arg raw_output_root "$raw_output_root" \
    --arg raw_run_dir "$raw_run_dir" \
    --arg perf_raw_json "$perf_raw_json" \
    --arg massif_raw "$massif_raw" \
    --arg manifest_path "$repo_root/Cargo.toml" \
    --argjson run_perf "$(json_bool "$run_perf")" \
    --argjson run_massif "$(json_bool "$run_massif")" \
    --argjson jq_perf_command "$jq_perf_command" \
    --argjson jq_massif_command "$jq_massif_command" \
    --argjson tyler_perf_command "$jq_tyler_perf_command" \
    --argjson tyler_massif_command "$jq_tyler_massif_command" \
    '
    {
        run_id: $run_id,
        run_label: (if $run_label == "" then null else $run_label end),
        created_at: $created_at,
        runner: {
            selector: $runner_selector,
            perf: $run_perf,
            massif: $run_massif
        },
        profile: {
            spec: (if $profile_spec == "" then null else $profile_spec end),
            resolved_input_root: (if $profile_input_root == "" then null else $profile_input_root end),
            profile_file: (if $profile_file == "" then null else $profile_file end),
            tyler_args: $profile_tyler_args,
            env: $profile_env,
            geodepot_bin: (if $geodepot_bin == "" then null else $geodepot_bin end)
        },
        git: {
            sha: $git_sha,
            short_sha: $git_short_sha,
            branch: $git_branch,
            describe: $git_describe,
            dirty: $git_dirty
        },
        host: {
            hostname: $hostname,
            uname: $uname
        },
        versions: {
            rustc: $rustc_version,
            cargo: $cargo_version,
            perf: $perf_version,
            valgrind: $valgrind_version
        },
        paths: {
            repo_root: $repo_root,
            input_root: $input_root,
            output_root: $output_root,
            raw_output_root: $raw_output_root,
            raw_run_dir: $raw_run_dir,
            staging_dir: $staging_dir,
            run_dir: $run_dir,
            binary_path: $binary_path,
            manifest_path: $manifest_path
        },
        build: {
            command: ["cargo", "build", "--release", "--locked", "--bin", "tyler", "--manifest-path", $manifest_path],
            rustflags: $rustflags
        },
        perf: {
            enabled: $run_perf,
            command: (if $run_perf then $jq_perf_command else null end),
            tyler_command: (if $run_perf then $tyler_perf_command else null end),
            wall_clock_seconds: (if $run_perf then ($perf_elapsed_seconds | tonumber) else null end)
        },
        massif: {
            enabled: $run_massif,
            command: (if $run_massif then $jq_massif_command else null end),
            tyler_command: (if $run_massif then $tyler_massif_command else null end),
            wall_clock_seconds: (if $run_massif then ($massif_elapsed_seconds | tonumber) else null end)
        }
    }
    ' > "$manifest_json"

{
    cat <<EOF
# Tyler profiling run

- Run ID: $run_id
- Git SHA: $git_sha
- Branch: $git_branch
- Created: $created_at
- Runner: $runner_selector
- Geodepot profile: ${profile_spec:-manual}
- Geodepot input: ${profile_input_root:-n/a}
- Profile config: ${profile_file:-n/a}
- Input: $input_root
- Output directory: $final_run_dir
- Binary: $binary_path
EOF

    if [[ "$run_perf" == "true" ]]; then
        cat <<EOF
- Perf wall clock: ${perf_elapsed_seconds}s
- Perf IPC: ${perf_ipc_display}
- Perf cache references: $perf_cache_refs
- Perf cache misses: $perf_cache_misses
- Perf cache miss rate: ${perf_cache_miss_rate_display}
- Perf page faults: $perf_page_faults
- Perf context switches: $perf_context_switches
- Perf CPU migrations: $perf_cpu_migrations
- Perf branches: $perf_branches
- Perf branch misses: $perf_branch_misses
EOF
    else
        cat <<EOF
- Perf: skipped by --runner=$runner_selector
EOF
    fi

    if [[ "$run_massif" == "true" ]]; then
        cat <<EOF
- Massif wall clock: ${massif_elapsed_seconds}s
- Massif peak heap: ${peak_heap_mib} MiB (${peak_heap_bytes} bytes)
- Massif peak snapshot: $peak_snapshot
- Massif snapshots: $snapshot_count
- Massif max heap growth: ${max_heap_growth_mib} MiB (${max_heap_growth_bytes} bytes)
- Massif max heap growth snapshots: $max_heap_growth_from_snapshot -> $max_heap_growth_to_snapshot
EOF
    else
        cat <<EOF
- Massif: skipped by --runner=$runner_selector
EOF
    fi

    cat <<EOF

Committed artifacts are the JSON summaries and this markdown note. Raw perf and
Massif dumps, together with Tyler's own outputs, are retained under
$raw_run_dir.
EOF
} > "$staging_dir/summary.md"

mv "$staging_dir" "$final_run_dir"
finalized="true"
trap - EXIT

printf 'Wrote profiling run to %s\nRaw outputs retained in %s\n' "$final_run_dir" "$raw_run_dir"
