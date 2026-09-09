//! Build-time behaviour of the `events!` macro: what it accepts and what it refuses to compile.
//!
//! The expected compiler output is in `tests/ui/*.stderr`. After a toolchain update changes its wording,
//! regenerate it with `TRYBUILD=overwrite cargo test -p tracer-sdk --test catalog` and review the diff.

#[test]
fn events_macro() {
    let cases = trybuild::TestCases::new();
    cases.pass("tests/ui/pass.rs");
    cases.compile_fail("tests/ui/invalid_event_name.rs");
    cases.compile_fail("tests/ui/invalid_category_name.rs");
    cases.compile_fail("tests/ui/duplicate_event_name.rs");
    cases.compile_fail("tests/ui/duplicate_category_name.rs");
    cases.compile_fail("tests/ui/undeclared_category.rs");
    cases.compile_fail("tests/ui/undeclared_event.rs");
}
