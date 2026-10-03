# The Ortho4XP pod contract, measured

Spike 0, folded into sub-project 4 as its first phase. One real tile —
**+51+000, provider GO2 (Google), ZL16** — built headless in the worker
image (`oxo-worker:dev`, Ortho4XP pinned to Shred86 `dev` @ `c363134`),
both task types, with the numbers, the directory contract and the failure
taxonomy the rest of the sub-project consumes. Every number below is
transcribed from captured run output, not recalled.

The spike also earned its keep by invalidating three assumptions; each is
recorded where it bit, and the corrections are implemented in the image
and the runner:

1. `O4_Config_Utils` imports tkinter at module level, so the headless
   image needs `libtk8.6` (`tk` in the apt layer) even though no GUI ever
   opens.
2. Importing the `O4_*` modules is not enough to build: the four
   provider-initialisation calls from `Ortho4XP.py:46-49` must run first
   (with `Provider_dir` on `sys.path`), or every texture fails
   `Unknown provider` — and the build still exits 0 (see taxonomy).
3. `custom_overlay_src` must be set on its owning module
   (`O4_Overlay_Utils`, per `O4_Cfg_Vars`' `module: "OVL"` binding), not
   on `CFG` — and the **ortho** path reads it too (`O4_DSF_Utils.py:365`
   extracts rasters from X-Plane's Global Scenery), so the runner sets it
   for both task types.

## (a) Wall-clock per phase, ortho task

| Phase | Run 1 (cold DEM, no providers) | Run 2 (warm DEM, providers initialised) |
|---|---|---|
| `build_poly_file` | 1572.9 s | 913.4 s |
| `build_mesh` | 11.9 s | 11.9 s |
| `build_masks` | 8.8 s | 8.8 s |
| `build_tile` | 37.7 s (no imagery — invalid, see taxonomy) | 276.9 s (real imagery) |
| **Total** | 1631.3 s | **1211.1 s (~20 min)** |

`build_poly_file` dominates and is Overpass/OSM-bound, not CPU-bound
(the process sat near idle CPU on network waits — sampled via `ps`
during the run; not captured in a log artifact). +51+000 covers
metropolitan London's eastern edge — a heavy OSM tile; lighter tiles
will be substantially faster in this phase. The run-to-run poly delta
(1573 → 913 s) mixes two causes that cannot be separated from these
logs: run 2's warm DEM cache (run 1 downloaded 209 MiB of elevation
inside this phase) and ordinary Overpass server variance. OSM data
itself was not cached — it lives on scratch, which was wiped between
runs.

## (b) Peak memory (cgroup `memory.peak`)

- Run 1 (no imagery): 2,204,291,072 B (2.05 GiB)
- Run 2 (real imagery): **5,163,413,504 B (4.81 GiB)**

The only difference between the runs is real imagery work in
`build_tile`, so the 2.7 GiB delta is attributable to it. Worker pods
should budget ≥ 6 GiB for ZL16 ortho tasks.

## (c) Peak scratch

- Final scratch after run 2: 3,575,747,672 B (3.33 GiB). Breakdown by
  byte count (`du -sb`): `Tiles/` 2,822,676,702 (the deliverable build
  directory, of which `textures/` is 2,595,953,294 across 282 DDS files,
  `Earth nav data/` 55,569,820, the mesh 82,248,820);
  `Orthophotos/` 743,412,712 (source jpegs — see the directory
  contract); `OSM_data/` 8,489,646; `Masks/` 1,167,354.
- The scratch sampler recorded a non-decreasing series ending at
  3,394,856,702 B (the samples are untimed, so this is a lower bound on
  the true high-water mark, not an observation of it; the final size
  above is the better number).
- At ZL17/18 expect roughly 4×/16× the texture and source-imagery
  volume.

## (d) The DEM cache

- One tile's elevation data: 219,244,952 B (~209 MB), fetched during run
  1's `build_poly_file`.
- Run 2 reused it: the cache byte count was unchanged and `build_mesh`
  time was identical (11.9 s both runs). The cache saves its download
  (and covers neighbouring tiles — elevation files span more than one
  1×1 tile).
- Download behaviour for concurrency: not directly observed under
  contention; the open decision in the worker-pod design stands.

## (e) Overlay task

- `build_overlay(51, 0)`: **2.9 s**, producing a 12,138,720 B DSF.
- Ran in a fresh container with only the content and scratch mounts —
  no ortho outputs. X-Plane 12's shipped DSFs are 7z-compressed;
  Ortho4XP checks the file magic and extracts before converting
  (`O4_Overlay_Utils.py:78-86`), which is why the image carries p7zip.
- The measured ratio to an ortho task (~2.9 s vs ~1200 s; 12 MB vs
  3.3 GiB scratch) confirms the architecture's premise that overlay work
  is orders of magnitude lighter — the overlay-only capacity fallback is
  meaningful.

## (f) The directory contract

Confirmed by inspection of real outputs (and consumed by the worker's
egress code):

- Ortho deliverable: `Tiles/zOrtho4XP_<tile>/` under scratch (via the
  image's install-dir symlinks), containing `textures/`, `terrain/`,
  `Earth nav data/<block>/<tile>.dsf`, the tile config and mesh
  intermediates. The whole directory is the deliverable.
- Overlay deliverable: the single file
  `yOrtho4XP_Overlays/Earth nav data/<block>/<tile>.dsf`, where
  `<block>` is each coordinate floored to a multiple of 10 (+51+000 →
  `+50+000`).
- All Ortho4XP paths resolve against the process cwd
  (`O4_File_Names.resource_path`), so the runner chdirs to the install
  root; the image's symlinks place every working directory on the right
  mount.
- Source imagery lands in `Orthophotos/<block>/<tile>/<provider>_<zl>/`
  as jpegs (743,412,712 B for this tile at GO2/ZL16) and is **not part
  of the deliverable** — the DDS textures in the build directory are
  derived from it. The worker's egress correctly ships only the build
  directory; the jpegs die with the wholesale scratch wipe. (An earlier
  draft of this document wrongly called `Orthophotos/` empty.)

## (g) Failure taxonomy, probed

- **Bad/unknown provider (observed, run 1):** every texture logs
  `Unknown provider GO2 or it has no data around <lat> <lon>` — yet the
  build **completes with exit 0**, producing a structurally valid tile
  with placeholder/water textures only. Outside the process this is
  indistinguishable from success except by output inspection. This is
  the sharpest confirmation of the architecture's "never trust
  Ortho4XP's exit status": the runner's phase wrapping catches
  exceptions, but a *silently degraded* success is only detectable by
  artifact inspection (texture count/bytes; run 1's `textures/` held
  about 1.1 MiB of placeholder PNGs where run 2's holds 2.4 GiB of
  DDS — observed before run 1's tree was wiped, not retained) —
  recorded as an open hardening idea for the worker (a cheap sanity
  floor on texture bytes before egress). The brief's `NOPE` provider
  probe was substituted by this naturally occurring run-1 failure,
  which exercises the same path with the same signature.
- **Missing X-Plane content (observed):** overlay extraction fails fast
  with `file Earth nav data/<block>/<tile>.dsf absent` and
  `build_overlay` returns 0 (its *success* value is 1) — no exception,
  no exit code. The runner treats a falsy return as failure for overlay
  tasks and for each ortho build step, stopping at the first failure and
  reporting `<fn> returned <r>` with that function as the phase. The
  convention is per function: `build_poly_file`, `build_mesh`,
  `build_tile` and `build_overlay` return 1 on success and 0 on every
  handled failure, so any falsy result fails. `build_masks` is the
  exception: it ends in a bare `return` (None) on success and returns 0
  only on its failure paths, so None counts as success there and only a
  falsy non-None result fails. The ortho path logs the same absence while
  extracting rasters and continues.
- **No network (probed, `--network none`):** `build_poly_file` stalls
  **silently** — for the probe's whole lifetime (several minutes before
  it was cut off) the process produced exactly one log line (the
  config-created notice) and no error; the 64-byte log IS the artifact.
  No exception, no exit, no retry chatter. From outside the process this is indistinguishable from a
  long build; the lease system's `max_task_duration` backstop is the
  only effective remedy, which is precisely why it exists. (The pinned
  commit includes the fix that stops a `tile.dem=None` crash when an
  Overpass airport query fails mid-run.)
- **Full scratch (probed, 8 MB tmpfs):** fails loudly and quickly —
  `OSError: Not enough free space to write 53963716 bytes after offset
  0` raised at 68.6 s inside `build_poly_file` (the ~54 MB elevation
  copy into the build directory). A real exception, so the runner's
  phase wrapping converts it to a failed result with the OS error as
  the reason. Disk exhaustion is a *failure*, not a wedge — the
  capacity check exists so pods don't start tasks they cannot finish.

## (h) Recommended defaults derived from these numbers

- **Worker `--min-free-scratch-bytes`:** 8 GiB (8,589,934,592) as a
  **ZL16 floor**: peak observed scratch was 3.33 GiB on a heavy tile,
  so 8 GiB gives better than 2× margin. It does NOT cover ZL17 —
  scaling textures and source imagery ~4× projects ~13 GiB — so
  operators producing above ZL16 must raise the flag with the zoom.
  (The shipped interim default was 50 GiB,
  `oxo-worker/src/config.rs:43`; trued to 8 GiB with this document.)
- **Operator guidance for `--max-task-duration-secs`:** the daemon
  default of 21600 s (`oxo-controld/src/config.rs:24`) is comfortable:
  the measured heavy-tile ZL16 ortho was ~20–27 min end to end. For
  ZL18 regions, scale the expectation ~16× on `build_tile` and
  revisit.
- **Memory budget per ortho worker:** ≥ 6 GiB.
- **Heartbeat interval 30 s against a 120 s timeout:** unchanged; the
  long network stalls in `build_poly_file` are in-process waits, not
  heartbeat gaps (the supervisor heartbeats independently of the build).

## Related

- `docs/specs/2026-10-02-worker-pod-design.md` — the design this phase
  belongs to; its directory assumptions are confirmed above with one
  correction (the `OVL` module binding) implemented in the runner.
- `worker/Containerfile`, `worker/README.md` — the image these runs used,
  including the spike-driven `tk` addition and the gdal pin deviation.

## Addendum: the deliverable under skip_converts (2026-10-02)

Probed after the operator-conventions ruling: the run-2 build directory's
`build_tile` products were removed (mesh/poly/alt intermediates and the
cached `Orthophotos/` jpegs kept), `TILE.skip_converts = True` set on its
owning module, and `build_tile` re-run alone in a fresh container over the
preserved volumes.

- **`build_tile` took 37.8 s** (against 276.9 s with conversion) over
  cached source jpegs — the DDS conversion was the bulk of the phase.
- **The build directory contains no jpegs and no DDS** under
  `skip_converts=True`; source jpegs stay in `Orthophotos/` (unchanged at
  743,412,712 B).
- **Ship** (the XEL tile, 64,027,287 B total):
  - `Earth nav data/+50+000/+51+000.dsf` — 62,910,971 B (this rebuild's
    DSF; run 2's was 55,569,820 B — DSF size varies run to run)
  - `terrain/*.ter` — 500 files, 71,886 B
  - `textures/*.png` — 50 files, 1,044,430 B: the water-mask textures the
    sea-overlay `.ter` descriptors reference. **The masks live in
    `textures/`**, which is the fact the egress filter needs.
- **Perish** (dies with the scratch wipe):
  - `Data+51+000.{mesh,alt,node,poly,apt}` — 171,080,857 B of build
    intermediates
  - `Ortho4XP_+51+000.cfg` and `.cfg.bak` — the tile config snapshot
  - everything under `Orthophotos/`, `OSM_data/`, `Masks/`, `tmp/`
- The deliverable is therefore ~64 MB instead of ~2.6 GiB per ZL16 tile,
  and the artifacts volume carries only what XEL consumes.
