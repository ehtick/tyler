# cjconvert ad hoc report

Date: 2026-07-03

Inputs:

- 3DBAG cluster 4x: `../cityjson-corpus/artifacts/acquired/3dbag/v20250903/cluster_4x.city.json`
- Basisvoorziening 3D: `../cityjson-corpus/artifacts/acquired/basisvoorziening-3d/2022/3d_volledig_84000_450000.city.json`

Outputs and timing logs:

- [raw/cjconvert-ad-hoc-2026-07-03](./raw/cjconvert-ad-hoc-2026-07-03)

Results:

| Case | Status | Elapsed | User | System | Max RSS | File system outputs | Output size |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 3DBAG cluster 4x | success | `0:45.35` | `20.95s` | `24.38s` | `225,364 KB` | `246,072` | `124,002,304 bytes` |
| Basisvoorziening 3D | failed | `0:01.12` | `0.96s` | `0.14s` | `701,068 KB` | `64` | `4,096 bytes` |

Interpretation:

- The 3DBAG run is CPU-heavy but also strongly write-bound. `user + system` is almost the full wall time, and the file-system output count matches the ~124 MB GeoPackage size closely.
- The Basisvoorziening run fails before producing a real output file. The error is `duplicate column name: attributes__eindregistratie`, which comes from distinct source attributes that collapse to the same GeoPackage column name.
- `/usr/bin/time -v` does not report cache-miss counts or allocation counts directly. For these runs, the closest signals are max RSS, minor page faults, and the user/system split. On that basis, 3DBAG looks moderately memory-stable, while Basisvoorziening touches a much larger working set before aborting.
- Both runs show zero major page faults, so the observed cost is not disk paging. The runtime is dominated by in-memory work plus output generation, not swapping.
- The table above is historical. A fresh rerun after the collision fix now aborts earlier on a separate geometry validation error (`encode Solid boundary as WKB` / `Invalid ring: closed WKB polygon ring must contain at least four coordinates (vertex count: 3)`), so the failure is no longer the attribute-name collision that originally motivated this report.
