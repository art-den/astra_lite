# starsolver

## What it is

A small, fast plate solver: a Rust reimplementation of a subset of
[astrometry.net](https://astrometry.net)'s `solve-field`.

Give it a list of detected star pixel positions (a `.xyls` FITS file) and a
directory of astrometry.net index files, and it figures out where on the sky
the image was pointing. On success it prints the field center, angular size,
rotation angle and parity to stdout:

```
Field center: (RA,Dec) = (60.030784, 36.376119) deg.
Field center: (RA H:M:S, Dec D:M:S) = (04:00:07.388, +36:22:34.027).
Field size: 3.50801 x 2.65361 degrees
Field rotation angle: up is -178.763 degrees E of N
Field parity: neg
```

The printed rotation/scale/center are the **field-center frame** values: the
solved WCS is refined on its matched star pairs and re-centred onto the
tangent plane of the field-center pixel before printing, so rebuilding a TAN
WCS from the printed numbers (tangent point at the image center) is accurate
to ~2 px at the frame corners instead of ~60 px. Relative to the original
`solve-field` print (an off-center tangent frame) the shipped fields differ by
at most ~0.2 deg of rotation and ~0.02% of scale.

It is intentionally minimal: `.xyls` input only (no image processing), stdout
output only (no `.wcs` files), old-format `QUAD` index files only (all-sky and
healpix sub-region tiles). It uses
little memory (a single index at a time) and solves a typical field in well
under a second.

Build with `cargo build --release`; index files can be downloaded from
<https://data.astrometry.net/4100/>.

## Command-line arguments

```
starsolver [FLAGS] <file.xyls>

  -I, --index-dir <DIR>   Directory of index files. Default: the system
                          astrometry.net indices (see "Index file discovery").
  -w, --width <N>         Image width, pixels. Default: the IMAGEW key of
                          the .xyls headers.
  -e, --height <N>        Image height, pixels. Default: the IMAGEH key.
  -L, --scale-low <V>     Lower bound of the image scale, in --scale-units.
  -H, --scale-high <V>    Upper bound of the image scale, in --scale-units.
  -u, --scale-units <U>   Units for the scale bounds (default degwidth):
                            degwidth    field width in degrees (default)
                            arcminwidth field width in arcminutes
                            arcsecperpix scale in arcsec/pixel
  -3, --ra <RA>           Expected field center RA, degrees or hh:mm:ss.
  -4, --dec <DEC>         Expected field center Dec, degrees or [+-]dd:mm:ss.
  -5, --radius <DEG>      Search radius around --ra/--dec, degrees (default 10).
  -l, --cpulimit <SEC>    Time limit in seconds (default 300).
  -v, --verbose           Print progress to stderr.
  -h, --help              Show help.
```

Notes:

- The frame size comes from `--width`/`--height`, or, when either flag is
  omitted, from the `IMAGEW`/`IMAGEH` keys of the `.xyls` (primary header
  first, then the first extension): one flag alone is not a frame, both values
  then come from the file. If neither source provides a size, the solver exits
  with code 2.
- If only one of `--scale-low`/`--scale-high` is given, it is treated as an
  exact scale. If neither is given, a wide 0.1-180 deg field-width range is
  searched (slower). Tighter bounds solve faster, but leave a little slack:
  an exact zero-width scale (e.g. `-L 2.71 -H 2.71` for a true scale of
  2.7115) may miss. Prefer a small range, e.g. 2.70-2.72.
- `--ra`/`--dec` must be given together; they reject solutions outside the
  hint circle and also prune healpix sub-region index files whose sky cell
  cannot overlap it (all-sky files are always kept; a blind solve without
  `--ra/--dec` tries every scale-fitting file).
- If no index file matches the requested scale range, the solver gives up
  (exit 1); unlike `solve-field` it does not fall back to `ibiggest`/`ismallest`.
- Exit code: `0` solved, `1` field not solved, `2` bad input or error.

## Index file discovery

With `--index-dir <DIR>` the solver uses exactly that directory and nothing
else; a file in it that is an astrometry.net index in an unsupported format is
an error (exit 2).

Without `--index-dir` the indices of the astrometry.net install found on the
system are used, following the config-file handling of `astrometry.net's
solve-field`:

1. The first config file that exists and lists `add_path` directories supplies
   them, tried in this order: `./astrometry.cfg` (current directory),
   `/etc/astrometry.cfg` (the Debian/FHS location), `astrometry.cfg` next to the
   solver binary, and `<install prefix>/etc/astrometry.cfg` (what `make install`
   writes).
2. If there is no config file, or it lists no `add_path`, the conventional
   install directories are scanned: `/usr/share/astrometry` (Debian package)
   and `/usr/local/astrometry/data` (`make install-indexes`).

Every `*.fits` file in the resulting directories is read, the way `solve-field`
does its index-directory discovery. Only its headers are read during the scan, so
the discovery costs a
few milliseconds even when the directory holds gigabytes of index data. Old-format
`QUAD` indices are used, both all-sky (`HEALPIX=-1`) and healpix sub-region
tiles (`HEALPIX>=0` + `HPNSIDE`, the astrometry.net xy pixel scheme; blind
mode tries every tile whose scale fits, `--ra/--dec` narrows the tiles by
overlap with the hint circle). Index files we cannot use - the new index
format, H4, or `HPNCOMP=NESTED` tiles - are then skipped with a warning
instead of aborting the run, because a system data directory may contain them
alongside the usable ones. `-v` prints the directories
and where they came from.

Caveats: relative `add_path` values are resolved against the current directory
(as in the original), a `~` prefix is expanded from `$HOME`, and the lines that
name individual indices (`index`, `indexset`, `multiindex`) are ignored - the
whole `add_path` directory is scanned, and the solver warns when it sees such a
line. A config file that cannot be read is reported and skipped, not fatal.

## Examples

Solve using the astrometry.net indices installed on the system (no
`--index-dir`):

```
./target/release/starsolver -w 4656 -e 3522 testdata/4656x3522-1.xyls
```

Solve a field with a known approximate scale (image is 4656x3522 px,
field about 3.5 deg wide):

```
./target/release/starsolver \
    -w 4656 -e 3522 \
    --scale-low 3 --scale-high 4 \
    testdata/4656x3522-1.xyls
```

Solve with a position hint instead of a scale hint (searches all scales,
accepts solutions only within 5 deg of the given point):

```
./target/release/starsolver \
    -w 4656 -e 3522 \
    --ra 60 --dec 36:22:33 --radius 5 \
    testdata/4656x3522-1.xyls
```

Known scale in arcsec/pixel (narrow range), with progress output and a 30 s
timeout:

```
./target/release/starsolver \
    -w 4656 -e 3522 \
    -u arcsecperpix -L 2.70 -H 2.72 \
    -l 30 -v testdata/4656x3522-2.xyls
```

Index files outside the system install (skips the config-file discovery):

```
./target/release/starsolver \
    --index-dir /usr/share/astrometry \
    -w 4656 -e 3522 testdata/4656x3522-1.xyls
```

## Library use

The crate is also a library: `starsolver::run_into(&args, out, progress)` runs
the same pipeline, writing the five-line success block to `out` and the progress
lines to `progress`, and returns the solved `starsolver::Solution` as data
(`starsolver::run` is the process-stdout/stderr wrapper the binary uses). The
`Args` struct comes from clap, e.g. `Args::parse_from([...])`.

## Testing

`cargo test` runs the unit and integration tests, including the tolerance gate
on the four shipped fields (`tests/ground_truth.rs`, run blind and with a
position hint), the public end-to-end glue (`tests/run_pipeline.rs`) and the
binary's exit-code and `--verbose` liveness contract
(`tests/cli_contract.rs`). Two kinds of data, gated independently:

- **Index files are not part of the repository.** The tests use the astrometry.net
  install found on the machine - the same discovery the solver uses without
  `--index-dir` - or the directories of `ASTROMETRY_TEST_INDEX_DIR` (a path list)
  when that is set. Because an installed set is not fixed, no test names an index
  id, a quad number or a log-odds value: tests pick an index by header properties
  or by which index actually solves a field, and assert invariants plus the
  published sky truth of those fields (the record of the `solve-field` runs
  behind them, kept in the gate's entry comments). Format-level
  tests build their FITS bytes in memory and need no data at all.
- **Field files** (`testdata/*.xyls`, gitignored) are the small fixtures every
  end-to-end test needs.

A test whose data is absent prints `skipping:` and passes as skipped. Set
`ASTROMETRY_REQUIRE_TEST_DATA=1` to turn any skip into a failure, so a run with
no index install cannot read as an all-pass:

```
ASTROMETRY_REQUIRE_TEST_DATA=1 cargo test     # fails if anything would be skipped
cargo test --release                          # the tolerance gate at its release budgets
GATE_FIELD=4656x3522-1 cargo test --test ground_truth   # one field of the gate
```
