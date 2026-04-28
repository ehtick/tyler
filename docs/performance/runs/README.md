# Profiling Runs

This directory stores committed per-run profiling summaries produced by
`just profile`.

The profiling wrapper now runs in profile-by-name mode. It resolves the input
dataset through Geodepot and reads the matching tyler arguments from the
`profile-tyler.json` data item stored at the case root. For example, the case
spec `bvz-dh-coast-5/bvz_dh` uses `bvz-dh-coast-5/profile-tyler.json` for the
profile data item. The Geodepot executable path is read from the repo-local
`.env` file via `GEODEPOT_BIN`.

The profile item should contain a JSON array of `tyler` arguments, or a JSON
object with a `tyler_args` array and an optional `env` object. Example:

```json
{
  "tyler_args": [
    "--3dtiles-implicit",
    "--object-type",
    "Building",
    "--object-type",
    "BuildingPart"
  ],
  "env": {
    "RUST_LOG": "info"
  }
}
```

Each run gets its own subdirectory containing:

- `manifest.json`
- `perf-stat.json` when `--runner perf` or `--runner all` is used
- `massif-summary.json` when `--runner massif` or `--runner all` is used
- `summary.md`

Raw `perf stat` and Valgrind Massif dumps are written to a temporary staging
area and are not meant to be committed.
Tyler's own output is also written to that staging area while the profiler is
running and is deleted before the final run directory is moved into place.
If a profiling run fails, the staging directory is preserved as a
`<run-id>.failed/` directory in this tree so you can inspect partial results
without rerunning Massif.

You can still override the input path manually with the script flags when you
need an ad hoc run, but the Geodepot profile mode is the default workflow.
The default runner mode is `all`; use `--runner perf` or `--runner massif`
when you only want one profiler.
