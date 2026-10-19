// Integration test for `SolverPipes` (src/plate_solve/astrometry.rs):
// the solver child must never block on a full OS pipe (>=64 KB).
// Without concurrent draining, a child writing more than the pipe buffer
// hangs in write() forever, `try_wait()` never reports an exit, and plate
// solving is stuck in `Waiting` for all eternity.
//
// The fake `starsolver` script is installed next to the test executable,
// where `resolve_executable()` looks for it first. `HardwareLock` keeps the
// test away from the goto suite, whose real plate solves run in the same
// directory and must not pick up the fake binary.
#![cfg(unix)]

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use astra_lite::{
    image::stars::{Star, StarItems},
    options::PlateSolverType,
    plate_solve::{PlateSolveConfig, PlateSolveResult, PlateSolver, PlateSolverInData},
    sky_math::math::degree_to_radian,
};

mod common;

const FILLER_LINE: &str = "filler filler filler filler filler 0123456789abcdefghij"; // 56 B
/// ~1.1 MB: exceeds the Linux default pipe buffer (64 KiB) and even the
/// maximum configurable capacity (1 MiB), so the test cannot go vacuously green
const FILLER_LINES: usize = 20_000;

/// Upper bound for the fake solver to settle; it exits almost instantly.
/// Hitting this means the child blocked on a full pipe (drain regression).
/// Note: a `join()`-before-exit regression would HANG inside `get_result()`
/// instead of tripping this assertion.
const SETTLE_TIMEOUT: Duration = Duration::from_secs(20);

struct FakeSolver {
    path: PathBuf,
}

impl FakeSolver {
    fn install(&self, script: &str) {
        fs::write(&self.path, script).expect("writing fake starsolver");
        fs::set_permissions(&self.path, fs::Permissions::from_mode(0o755))
            .expect("making fake starsolver executable");
    }
}

impl Drop for FakeSolver {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// Installs a fake solver next to the test binary. Removed on drop; a run
/// aborted by panic=abort can leave it behind, so the goto suite cross-checks
/// its absence via `common::assert_no_fake_starsolver()`, and the pre-remove
/// here heals re-runs of this binary.
fn fake_starsolver() -> FakeSolver {
    let dir = std::env::current_exe()
        .expect("current exe")
        .parent()
        .expect("exe parent dir")
        .to_path_buf();
    let path = dir.join("starsolver");
    let _ = fs::remove_file(&path);
    FakeSolver { path }
}

/// Script body emitting FILLER_LINES lines on the given stream (POSIX awk:
/// no `yes`/SIGPIPE assumptions), preceded by the redirection target `redir`
fn filler_script(redir: &str) -> String {
    format!(
        "awk 'BEGIN{{for(i=0;i<{};i++)print \"{}\"}}' {}\n",
        FILLER_LINES, FILLER_LINE, redir
    )
}

fn run_solver_until_settled(sentinel: &std::path::Path) -> PlateSolveResult {
    let stars: StarItems = vec![
        Star { x: 100.0, y: 120.0, brightness: 5000, ..Default::default() },
        Star { x: 300.0, y: 250.0, brightness: 4000, ..Default::default() },
        Star { x: 480.0, y: 90.0, brightness: 3000, ..Default::default() },
    ];

    let mut solver = PlateSolver::new(PlateSolverType::Astrometry);
    // Default config: eq_coord is None, so no Failed->restart widening
    solver.start(
        &PlateSolverInData::Stars { stars: &stars, img_width: 640, img_height: 480 },
        &PlateSolveConfig::default(),
    ).expect("starting solver with fake starsolver");

    let deadline = Instant::now() + SETTLE_TIMEOUT;
    let result = loop {
        match solver.get_result().expect("get_result") {
            PlateSolveResult::Waiting => {
                assert!(
                    Instant::now() < deadline,
                    "Solver stuck in Waiting for {} s: the child is likely \
                     blocked on a full pipe (SolverPipes drain regression)",
                    SETTLE_TIMEOUT.as_secs()
                );
                thread::sleep(Duration::from_millis(20));
            }
            result => break result,
        }
    };
    solver.abort();

    assert!(
        sentinel.exists(),
        "Fake starsolver was not executed — the solver may have fallen back \
         to a real solve-field from PATH; the test would prove nothing"
    );
    result
}

/// Sentinel file touched by the fake script to prove the fake really ran
fn make_sentinel() -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let path = std::env::temp_dir()
        .join(format!("astra_fake_ps_{}_{}.used", std::process::id(), unique));
    let _ = fs::remove_file(&path);
    path
}

/// Exit 0 with >1 MB on stdout: all output must be drained concurrently
/// with polling, and the result lines at the END of the huge stream parsed.
#[test]
#[serial_test::serial]
fn solver_drains_huge_stdout_and_parses_result() {
    let _lock = common::HardwareLock::acquire().expect("acquiring the hardware test lock");
    let sentinel = make_sentinel();
    let fake = fake_starsolver();
    fake.install(&format!(
        "#!/bin/sh\n\
         : > '{}'\n\
         {}\
         echo 'Field center: (RA,Dec) = (10.123456, 20.654321) deg.'\n\
         echo 'Field size: 0.500000 x 0.400000 degrees'\n\
         exit 0\n",
        sentinel.display(),
        filler_script(""),
    ));

    match run_solver_until_settled(&sentinel) {
        PlateSolveResult::Done(result) => {
            assert!(
                (result.crd_j2000.ra - degree_to_radian(10.123456)).abs() < 1e-9,
                "unexpected ra: {}", result.crd_j2000.ra
            );
            assert!(
                (result.crd_j2000.dec - degree_to_radian(20.654321)).abs() < 1e-9,
                "unexpected dec: {}", result.crd_j2000.dec
            );
            assert!(
                (result.width - degree_to_radian(0.5)).abs() < 1e-9,
                "unexpected width: {}", result.width
            );
        }
        PlateSolveResult::Failed => panic!("expected Done for the parseable result lines"),
        PlateSolveResult::Waiting => unreachable!("settled loop never returns Waiting"),
    }
}

/// Exit non-zero with >1 MB on stderr + the "not solved" marker:
/// stderr must be drained too, and its content still delivered to the
/// marker check.
#[test]
#[serial_test::serial]
fn solver_drains_huge_stderr_and_reports_failed() {
    let _lock = common::HardwareLock::acquire().expect("acquiring the hardware test lock");
    let sentinel = make_sentinel();
    let fake = fake_starsolver();
    fake.install(&format!(
        "#!/bin/sh\n\
         : > '{}'\n\
         {}\
         echo 'Field not solved within time limit' 1>&2\n\
         exit 3\n",
        sentinel.display(),
        filler_script("1>&2"),
    ));

    match run_solver_until_settled(&sentinel) {
        PlateSolveResult::Failed => {}
        _ => panic!("expected Failed for the 'not solved' marker"),
    }
}
