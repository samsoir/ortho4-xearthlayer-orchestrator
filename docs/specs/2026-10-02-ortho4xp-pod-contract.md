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
(the process sat at ~1% CPU on network waits). +51+000 covers
metropolitan London's eastern edge — a heavy OSM tile; lighter tiles
will be substantially faster in this phase. The run-to-run poly delta
(1573 → 913 s) is Overpass server variance, not local caching: OSM data
lives on scratch and was wiped between runs.

## (b) Peak memory (cgroup `memory.peak`)

- Run 1 (no imagery): 2,204,291,072 B (~2.1 GiB)
- Run 2 (real imagery): **5,163,413,504 B (~4.8 GiB)**

The DDS conversion in `build_tile` is the high-water mark. Worker pods
should budget ≥ 6 GiB for ZL16 ortho tasks.

## (c) Peak scratch

- Final scratch after run 2: 3,575,747,672 B (~3.3 GiB), of which the
  deliverable (`zOrtho4XP_+51+000/`) holds 2.5 GiB of textures (282 DDS
  files) plus a 53 MB DSF, 79 MB mesh and intermediates.
- Sampled high-water mark during run 2: 3,394,856,702 B one minute
  before completion — scratch grows monotonically through `build_tile`;
  peak ≈ final.
- At ZL17/18 expect roughly 4×/16× the texture volume.

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
- Ran in a fresh container with only the content mount — no DEM, no
  ortho outputs, no network. X-Plane 12's shipped DSFs are 7z-compressed;
  Ortho4XP detects and extracts via p7zip (in the image).
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
- `Orthophotos/` stayed empty in both runs at ZL16/GO2 — downloaded
  jpegs are assembled and converted under the build directory instead.

## (g) Failure taxonomy, probed

- **Bad/unknown provider (observed, run 1):** every texture logs
  `Unknown provider GO2 or it has no data around <lat> <lon>` — yet the
  build **completes with exit 0**, producing a structurally valid tile
  with placeholder/water textures only. Outside the process this is
  indistinguishable from success except by output inspection. This is
  the sharpest confirmation of the architecture's "never trust
  Ortho4XP's exit status": the runner's phase wrapping catches
  exceptions, but a *silently degraded* success is only detectable by
  artifact inspection (texture count/bytes) — recorded as an open
  hardening idea for the worker (cheap sanity floor on texture bytes
  before egress).
- **Missing X-Plane content (observed):** overlay extraction fails fast
  with `file Earth nav data/<block>/<tile>.dsf absent` and
  `build_overlay` returns 0 (its *success* value is 1) — no exception,
  no exit code. The runner treats a falsy return as failure for overlay
  tasks. The ortho path logs the same absence while extracting rasters
  and continues.
- **No network (probed, `--network none`):** `build_poly_file` stalls
  **silently** — over twelve minutes the process produced exactly one
  log line (the config-created notice) and no error, sitting at ~0% CPU
  inside its first Overpass attempt. No exception, no exit, no retry
  chatter. From outside the process this is indistinguishable from a
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

- **Worker `--min-free-scratch-bytes`:** 8 GiB (8,589,934,592). Peak
  observed scratch was ~3.3 GiB at ZL16 on a heavy tile; 8 GiB covers
  ZL17 on most tiles and concurrent overlay work, while letting the
  overlay-only fallback engage early. (The shipped interim default was
  50 GiB — overly conservative; trued to 8 GiB with this document.)
- **Operator guidance for `--max-task-duration-secs`:** the default
  21600 (6 h) is comfortable: the measured heavy-tile ZL16 ortho was
  ~20–27 min end to end. For ZL18 regions, scale the expectation ~16×
  on `build_tile` and revisit.
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
