//! Test-only support for the parity suite.
//!
//! No slide images are committed — the smallest useful one is larger than the
//! source tree, so `docs/test-data.md` has you fetch them into `data/`, which
//! is gitignored. Tests that need one therefore skip where it is absent, and a
//! skipped test in Rust is indistinguishable from a passing one: both print
//! `ok`, and both count toward the total the run reports at the end.
//!
//! That is fine for a working checkout and misleading everywhere else. A fresh
//! clone with no slides runs the whole suite, sees the same green as a full
//! run, and has exercised materially less of it — including most of what the
//! parity claim rests on. The summary number is identical either way, which is
//! the property that makes it worth nothing.
//!
//! [`skipped`] is the seam that fixes this. It still skips by default, so
//! ordinary local work is unchanged. With [`REQUIRE_DATA`] set it panics
//! instead, so a run being taken as evidence cannot quietly not be one.

/// The one spelling of the variable name, so no call site can hold a different
/// one and silently never fire.
pub const REQUIRE_DATA: &str = "QUPATH_RUST_REQUIRE_DATA";

/// Reports that a test is being skipped for want of untracked input.
///
/// Call it and then return. With [`REQUIRE_DATA`] set in the environment this
/// panics rather than returning, turning every skip in the suite into a failure
/// that names what was missing.
///
/// Set that variable wherever a green run is being read as evidence — CI, a
/// release check, or before quoting a figure the suite is supposed to back.
/// Leave it unset for ordinary local work.
///
/// ```no_run
/// # fn a_slide() -> Option<()> { None }
/// let Some(slide) = a_slide() else {
///     qupath_testing::skipped("data/CMU-1.svs not present");
///     return;
/// };
/// ```
#[track_caller]
pub fn skipped(reason: &str) {
    decide(reason, std::env::var_os(REQUIRE_DATA).is_some());
}

/// [`skipped`] with the environment already read, which is what the tests below
/// drive.
///
/// Split out because setting a variable to test the behaviour would mean
/// `unsafe` — `std::env::set_var` is unsafe in edition 2024, this workspace
/// denies `unsafe_code`, and a process-wide variable set inside one test is
/// visible to every other test in the binary. Passing the answer in tests the
/// branch that carries the behaviour and leaves exactly one untested line: the
/// `var_os` call above.
#[track_caller]
fn decide(reason: &str, required: bool) {
    assert!(
        !required,
        "{REQUIRE_DATA} is set, so this test was required to run, and it skipped: {reason}. \
         Fetch the missing input (see docs/test-data.md), or unset {REQUIRE_DATA} to allow skips."
    );
    eprintln!("skipping: {reason}");
}

#[cfg(test)]
mod tests {
    /// Unset is a skip, because an ordinary checkout has no slides and must
    /// still be able to run the suite.
    #[test]
    fn returns_when_the_data_is_not_required() {
        super::decide("data/CMU-1.svs not present", false);
    }

    /// Set is a failure, and this is the assertion the whole crate exists for.
    ///
    /// Without it `skipped` would be an `eprintln!` with extra steps: the
    /// witness would be in place at every call site and would never fire, which
    /// is the same silence it was added to break.
    #[test]
    #[should_panic(expected = "and it skipped: data/CMU-1.svs not present")]
    fn panics_when_the_data_is_required() {
        super::decide("data/CMU-1.svs not present", true);
    }

    /// The message names the variable, so a failing run says how to proceed
    /// without anyone having to find this file.
    #[test]
    fn the_failure_names_the_variable_and_the_way_out() {
        let message = std::panic::catch_unwind(|| super::decide("a missing slide", true))
            .expect_err("required data must panic");
        let message = message
            .downcast_ref::<String>()
            .expect("the panic carries a formatted message");
        assert!(message.contains(super::REQUIRE_DATA), "{message}");
        assert!(message.contains("docs/test-data.md"), "{message}");
    }
}
