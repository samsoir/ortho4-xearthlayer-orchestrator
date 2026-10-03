# worker: the Ortho4XP pod image

`make image` builds `oxo-worker:dev` from `worker/Containerfile` (podman, repo root as build context).

## What the image contains

- Python 3.12 (slim), `git` (used at build time), `p7zip-full`.
- A pinned Ortho4XP checkout at `/opt/ortho4xp` (the install root), `.git` removed, with its Python
  dependencies installed. The pin is the `ORTHO4XP_COMMIT` build arg and is recorded in the image label
  `oxo.ortho4xp-commit`. Source: `https://github.com/Shred86/Ortho4XP.git`, the operator's designated build target.
- Ortho4XP's fixed working directories re-homed onto mount points by symlink. They are not configurable
  (`O4_File_Names.py` resolves them against the cwd), so the image bakes the arrangement.

## Mount-point contract

| In-container path | Volume | Replaces |
|---|---|---|
| `/var/oxo/scratch` | scratch | `tmp`, `OSM_data`, `Orthophotos`, `Masks`, `Geotiffs`, `Tiles`, `yOrtho4XP_Overlays` (each a symlink from `/opt/ortho4xp/<dir>` to `/var/oxo/scratch/<dir>`) |
| `/var/oxo/dem` | dem-cache | `Elevation_data` |
| `/var/oxo/content` | content (read-only) | consumed via config, never symlinked: the X-Plane data (`custom_overlay_src`) and `patches/<set>/…` |
| *(artifacts mount)* | artifacts | no symlink — egress is the supervisor's explicit copy, and the operator mounts it so each region's `target_root` exists |

`Patches` is also a symlink, to `/var/oxo/patches-active`, which the supervisor points at
`/var/oxo/content/patches/<set>` per task — an indirection so the read-only content mount never needs to be writable.

## Run it

1. Build the image: `make image` (produces `oxo-worker:dev`).
2. Prepare the mounts per the table above: a scratch volume, the shared DEM cache, the read-only content tree
   (X-Plane data and patches), and an artifacts directory mounted at the region's `target_root`.
3. Edit the `EDIT ME` placeholders in `deploy/worker-pod.yaml` (control-plane URL, the three hostPaths, the
   artifacts mount path), then `podman kube play deploy/worker-pod.yaml`. `podman kube down deploy/worker-pod.yaml`
   removes it.

The settings an operator most often touches:

| Variable | Default | Notes |
|---|---|---|
| `OXO_CONTROL_URL` | none, required | Where `oxo-controld` is reachable from inside the pod. |
| `OXO_MODE` | `recycle` | `recycle` cleans scratch and takes the next task; `stop` performs one task and exits. |
| `OXO_MIN_FREE_SCRATCH_BYTES` | 8 GiB | A ZL16 floor (peak observed scratch 3.33 GiB). Raise it when producing above ZL16: ZL17 projects to about 13 GiB. |

The pod spec requests 6 GiB of memory: the documented budget for a ZL16 ortho worker (measured peak 4.81 GiB), in
`docs/specs/2026-10-02-ortho4xp-pod-contract.md`, section (h). The 8 GiB limit is chosen headroom above that
measured peak, not a documented figure.

## Tools, never data

No scenery, DEM, patches or imagery in any layer. Everything of that kind arrives through the mounts above at run time.

## Recorded deviations from the original apt/pip plan

- `build-essential` in the apt layer: `scikit-fmm` has no wheel and builds from source.
- `libgdal-dev` in the apt layer, and the pip step installs requirements minus the `gdal==` pin, then
  `gdal==$(gdal-config --version)`: Debian ships libgdal 3.10.3 while Ortho4XP pins 3.9.0 on Linux, and the
  Python binding must match the native library.

## Known benign message

Importing the Ortho4XP modules prints `ERROR: Providers/O4_Custom_URL.py contains invalid code. The corresponding
providers won't probably work.` It is Ortho4XP's own warning about an optional custom-URL provider stub. The GO2
provider is a `.lay` file and is unaffected.

## How the spike runs it

Task 2's spike runs the image with the scratch, dem-cache and content volumes mounted at the paths above
(content read-only) and the artifacts volume mounted at the region's `target_root`, then drives the worker
entry point inside it. The no-build probe: import `O4_File_Names` and the `O4_*` build modules with
`/opt/ortho4xp/src` on `sys.path` and assert `/opt/ortho4xp/Elevation_data` is a symlink.
