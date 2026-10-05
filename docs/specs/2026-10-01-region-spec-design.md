# Region specification design

Sub-project 1 of the decomposition in
[the architecture design](2026-10-01-oxo-architecture-design.md): the
data model for a regional scenery package specification, and the rules
that decide whether one is valid. A pure library and a thin CLI, with
no persistence, no runtime and no network.

The architecture document governs. Where this document appears to
contradict it, this document is wrong.

## Goal

A region specification that an operator can author, a validator can
reject with every fault listed at once, and a planner can atomize into
per-tile tasks without consulting anything else.

Success:

- A specification resolves to a complete, explicit parameter set for
  every tile it names, with no further lookups required.
- An invalid specification is rejected with all of its faults
  reported, not the first one found.
- Validation requires no filesystem, no network and no database, so
  the whole library is testable without fixtures or services.
- The canonical form of a tile identifier matches Ortho4XP's, so no
  translation layer exists to get wrong.

## Scope

In: the model, its canonical serialization, static validation, and a
CLI to validate and inspect a specification.

Out: how a human composes a tile set (the web interface's problem,
specified separately), storing specifications (sub-project 3),
enforcing the failure policy (sub-project 2), and any validation that
needs to look at the world (below).

## Shape

One Rust library crate, `oxo-spec`, plus a thin CLI binary over it.
The library's only I/O is parsing a specification it is handed. That
constraint is the point: it keeps every validation rule a pure
function of the model, so the test suite needs no fixtures, no
temporary directories and no services.

## The model

```
RegionSpec
  metadata            name, region code, revision
  tiles               set of TileId
  parameters          ProductionParameters   (region-level, one set)
  target              artifact target location
  failure_policy      FailurePolicy
```

### TileId

A tile is identified by the integer latitude and longitude of its
**southwest corner**. Its canonical string form is Ortho4XP's
`short_latlon` (`src/O4_File_Names.py`, read 2026-10-01): a signed
two-digit latitude followed by a signed three-digit longitude, zero
padded after the sign.

| Example | Meaning |
|---|---|
| `+50-002` | lat 50, lon -2 |
| `-07+110` | lat -7, lon 110 |
| `-90-180` | the southwest-most tile of the valid range |
| `+89+179` | the northeast-most tile of the valid range |

Latitude takes two digits because `90` is the largest magnitude;
longitude takes three because `180` is. Ortho4XP's tile directory is
`zOrtho4XP_` followed by this same string, which means produced
artifacts can be correlated with a specification by eye.

Reusing the tool's identifier rather than inventing one removes a
translation layer, and translation layers between two
nearly-identical coordinate encodings are a reliable source of
off-by-one and sign errors.

Valid range is latitude -90 to 89 and longitude -180 to 179 -- the
southwest corners of the 1x1 degree cells that tile the globe.

**Duplicates are a validation error, not a silent dedupe.** A
specification naming a tile twice reflects an authoring mistake, and
quietly collapsing it hides that mistake at exactly the moment it is
cheapest to fix.

### ProductionParameters

One set per region. Parameters do not vary per tile; a region needing
mixed zoom levels is expressed as more than one specification.

Two tiers:

- **Curated fields.** First-class, individually validated, named by
  OXO rather than by Ortho4XP. Certain members: provider code, zoom
  level, whether overlays are included. The full list is an open
  decision below.
- **Raw pass-through.** A string map handed to Ortho4XP's tile
  configuration untouched, so that no tuning is unreachable and OXO
  need not model all 44 of Ortho4XP's tile-level variables.

**A raw key that collides with a curated field is a validation
error.** Not a documented precedence order, and certainly not silent
shadowing. The fault class this guards against is recorded in the
manual process: a stray `overpass_server_choice=DE` silently disabled
Overpass failover for an entire install, because an unrecognised
value fell through to a default and nothing complained. An error at
validation time is the cheapest possible place to catch that shape of
mistake.

### FailurePolicy

Maximum attempts, backoff, and alert destinations. Modelled here
because the README makes it part of the specification; enforced by
the job server in sub-project 2. This separation is deliberate -- the
specification states intent, the job server is the only component
positioned to act on it.

## Validation

The split below is what keeps the library pure, and it is a design
commitment rather than an implementation convenience.

### Static — this library

A pure function of the model. No filesystem, no network, no clock.

- The tile set is non-empty.
- No duplicate tiles.
- Every tile is within range.
- Zoom level is within Ortho4XP's supported band.
- Provider code is syntactically well formed.
- No raw key collides with a curated field.
- Every raw key and value can be written into an Ortho4XP tile
  configuration: no empty key, no line break, `=` or edge whitespace in
  either half, and no control character in a key. Ortho4XP reads that
  file as
  `dict(line.strip().split("=") for line in f if line.strip())`, which
  constrains both halves. A **line break** means the write side turns one
  override into two configuration lines, reproducing the
  silent-shadowing fault class the previous rule exists to prevent. An
  **`=`** is worse: `dict()` requires exactly two items per element and
  `"foo=a=b".split("=")` gives three, so the call raises and the *whole*
  config read fails -- which Ortho4XP reports as a bare `Crash!` with no
  traceback, naming neither the override nor the file. Nothing keeps the
  first split; that would need `split("=", 1)`. **Whitespace at either
  edge** is the subtlest, and it defeats the reserved-key rule above:
  `strip()` runs before the split, so `" default_zl"` arrives as exactly
  `default_zl` and shadows the curated `zoom` field, having matched
  neither the reserved-key check nor a control-character check -- a space
  is Unicode `Zs`, not `Cc`. A value with edge whitespace cannot
  round-trip either, since the strip removes the trailing portion, so the
  value Ortho4XP reads is not the value the specification states. A value
  also may not carry a **control character at either edge**: Rust's
  `trim` covers Unicode `White_Space`, Python's `strip` additionally
  removes the C0 separators U+001C-U+001F, and the rule rejects both at a
  value edge. That boundary is exact, and checked exhaustively -- Python
  strips 29 codepoints in all of Unicode, and every one of them is
  rejected here, so no value this crate accepts is one Ortho4XP would
  silently truncate.
- Failure policy is coherent: at least one attempt, non-negative
  backoff.
- The target location is a well-formed path.
- Metadata is present: a name and a region code.

### Environmental — the control plane, at submission

Deliberately not here, because each of these requires looking at
something outside the specification:

- The provider code actually exists in Ortho4XP's `Providers/` tree.
  Codes are `.lay` filenames grouped by region, so the set of valid
  codes is a property of an Ortho4XP installation, not of this model.
- The provider permits the requested zoom. Provider definitions carry
  constraints -- `Providers/Global/EOX.lay` declares `max_zl=14`, so a
  zoom-16 request against EOX is invalid, and knowing that means
  reading that file.
- The target location exists and is writable.
- The overlay source directory is present, since Ortho4XP requires it
  as a real directory.
- Overpass endpoints are reachable.

### Reporting

Validation returns **every** fault, never the first. An operator
correcting a specification naming thousands of tiles cannot be made to
re-run once per mistake. This shapes the API: validation yields a
collection of errors, each carrying enough location information to
point at the offending tile or key.

That promise covers **validation faults**, and only those. A **schema
fault** -- a type error, an unknown key, a missing section -- is reported
first-only, by construction: deserialization stops at the first one serde
meets, and nothing downstream of it has a model to validate. A file
carrying `revision = "one"` plus eight validation faults therefore reports
the type error alone. The two-stage shape (deserialize to strings, then
validate) is what rescues the high-cardinality tile case, where reporting
one malformed identifier per run would be intolerable; it does not and
cannot rescue schema faults. The CLI's parse-failure message says so
explicitly, so that an operator does not read a schema fault as the only
problem with the file.

## Serialization

**TOML is the canonical on-disk format.** It is serde-native, it is
what a Rust project's operators will expect, and it is comfortable to
hand-author -- which matters for as long as the web interface does not
exist.

The tile set serializes as an array of canonical identifier strings.
That keeps a large specification greppable and gives line-oriented
diffs when a region changes, which an array of tables would not.

The same serde types produce JSON at the API boundary in sub-project 3.
One canonical format with a second representation falling out of the
same types -- not a format negotiation layer.

## CLI

| Command | Behaviour |
|---|---|
| `validate <file>` | Parse and statically validate. Non-zero exit on failure, with every fault printed. |
| `show <file>` | Print the parsed, normalised specification. |

No authoring or generation commands. Composing a tile set is the web
interface's task, decided in the architecture document; adding a
half-measure here would create a second authoring path to maintain and
then deprecate.

## Testing

Test-first throughout, per the project's principles.

- **Unit tests per validation rule.** Each rule is a pure function, so
  each gets a test that fails before the rule exists.
- **Property test** on tile identifiers: parse and format round-trip
  across the whole valid range, which is small enough to test
  exhaustively rather than sampling -- 180 x 360 cases.
- **Gherkin acceptance features**, covering: a valid specification is
  accepted; a duplicate tile is rejected; an out-of-range tile is
  rejected; a raw key colliding with a curated field is rejected; an
  empty tile set is rejected; a specification with several independent
  faults reports all of them.

The all-faults-reported scenario deserves an acceptance test rather
than a unit test, because it is a promise to the operator rather than
a property of one rule.

## Decisions

| Decision | Choice | Why |
|---|---|---|
| Crate shape | One pure library plus a thin CLI | No I/O beyond the specification file means no fixtures, no services, and every rule testable in isolation |
| Tile identity | Ortho4XP's `short_latlon` canonical form | Removes a translation layer between two near-identical coordinate encodings; artifacts correlate with specifications by eye |
| Duplicate tiles | Validation error | An authoring mistake, caught where it is cheapest to fix; silent dedupe hides it |
| Parameter scope | Region-level only | Mixed zoom is expressed as separate specifications; keeps every tile's parameter set trivially resolvable |
| Parameter model | Curated fields plus raw pass-through | The specification stays validatable without modelling Ortho4XP's 44 tile-level variables, and no tuning is unreachable |
| Raw/curated collision | Validation error | Silent shadowing is the fault class behind the `overpass_server_choice=DE` incident; precedence rules would reproduce it |
| Validation split | Static here, environmental in the control plane | Valid provider codes and zoom ceilings are properties of an Ortho4XP installation, not of this model |
| Error reporting | All faults, never the first | Correcting a large specification one fault per run is untenable |
| Serialization | TOML canonical; serde types reused for JSON | Human-authorable and diffable; one format, with the second representation falling out of the same types |
| Tile set encoding | Array of canonical id strings | Greppable, line-diffable; an array of tables is neither |
| CLI surface | `validate` and `show` only | Authoring belongs to the web interface; a half-measure here becomes a second path to maintain |
| Region code shape | `A-Z`, `0-9` and `-`, at most 512 characters (**amended 2026-10-04**, was 16) | XEarthLayer's published codes (`NA-USA-MX-CENTRAL`, 17; `NA-CANADA-GREENLAND`, 19) must be usable verbatim, so no mapping exists between OXO codes and published codes. The original 16 had no recorded reason; 512 is far beyond anything typed as an identifier and still bounds garbage. Storage is `text`, and the code never becomes a path (`target.root` does) |

## Open decisions

- **The exact curated parameter list.** Provider code, zoom level and
  overlay inclusion are certain. Which further Ortho4XP settings
  affect output enough to deserve first-class, validated treatment
  should be informed by spike 0, which will have run a real tile and
  seen which settings matter. Until then the raw pass-through covers
  them, so this does not block implementation.
- **Target location shape.** The README says "where to place the
  completed resources, ortho and overlay tiles", which may mean one
  path or two. One path with a known internal layout is simpler; two
  paths is more flexible. Unresolved.
- **Revision semantics.** Whether `revision` is operator-set or
  derived, and whether the job server keys tasks by it so that
  re-submitting an amended specification is distinguishable from
  re-running the original. Sub-project 2 may force this.
- **Alert destination representation.** A URI, a named channel, or
  something else. Likely better decided with sub-project 5, which owns
  alerting; modelled as an opaque string until then.

## Out of scope

- **Tile-set authoring and selection.** Bounding areas and similar
  broad selection methods belong to the web interface and get their
  own specification.
- **Storing specifications.** The control plane owns persistence.
- **Enforcing the failure policy.** Modelled here, enforced by the task
  server.
- **Environmental validation.** Listed above, and owned by the control
  plane at submission time.
- **Incremental production.** Out of scope for v1 entirely, per the
  architecture document.

## Rejected alternatives

**Per-tile parameter overrides.** A region default with any tile free
to override it. Rejected: region-level parameters are sufficient
because mixed zoom is expressible as separate specifications, and
overrides would make every tile's effective parameter set a
computation rather than a lookup.

**Named parameter profiles that tiles reference.** Compact for large
regions and preserves operator intent. Rejected for the same reason,
plus the indirection: profiles must be resolved before planning, and
validation grows cases for dangling and unused references.

**Full Ortho4XP configuration pass-through, with no curated fields.**
Nothing unreachable and no mapping to maintain. Rejected: it couples
the specification to Ortho4XP's configuration schema, reduces "valid
specification" to "well-formed TOML", and would make any future
non-Ortho4XP producer a breaking change.

**Curated fields only, with no escape hatch.** The smallest and most
validatable surface. Rejected: with 44 tile-level variables, tuning
that OXO did not anticipate would require a code change and a release.

**Silently deduplicating repeated tiles.** Rejected: it discards
evidence of an authoring mistake.

**A documented precedence order for raw-versus-curated collisions.**
Rejected: it is the shadowing behaviour that caused a real incident,
merely written down.

**Fail-fast validation.** Simpler to implement and to type. Rejected:
it makes correcting a large specification an iterative chore.

**YAML or JSON as the canonical format.** YAML is more compact for
long lists; JSON is the obvious API representation. Rejected as the
*canonical* form: YAML's implicit typing is a hazard in a file that
carries coordinates and provider codes, and JSON is unpleasant to hand
author. JSON remains available from the same serde types where it is
actually wanted.

## Dependencies

- `serde` and `toml` for the model and its canonical form.
- A Cucumber/Gherkin runner for Rust, for the acceptance features.
- No runtime dependency on Ortho4XP. This library reproduces
  Ortho4XP's tile identifier format and nothing else; everything that
  requires an Ortho4XP installation is environmental validation and
  lives elsewhere.

## Related

- [OXO architecture design](2026-10-01-oxo-architecture-design.md) --
  the execution model, the decomposition, and the decisions this
  sub-project inherits.
- `2026-10-01-oxo-high-level-design.md` -- the high-level specification, including the
  requirement that a specification carry failure policy.
