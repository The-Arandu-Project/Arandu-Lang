//! Generic jobs must preserve the complete result contract when forwarded.
use arandu_diagnostics::DiagCode;
use arandu_query::{passes::type_check, DatabaseImpl};

fn check_forwarding(required: &str, provided: &str, expect_rejection: bool) {
    for arguments in ["<J>", ""] {
        let mut db = DatabaseImpl::default();
        let file = db.new_file(
            "job_bounds.aru".into(),
            format!(
                r#"
interface Job<R> {{ func run(shared self): R }}
func consume<J: Job<{required}>>(job: J): void {{}}
func forward<J: Job<{provided}>>(job: J): void {{ consume{arguments}(job) }}
func main(): int {{ return 0 }}
"#
            ),
        );
        let result = type_check(&db, file);
        if expect_rejection {
            assert!(
                result
                    .diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.code == DiagCode::T025InterfaceNotSatisfied),
                "different job result contracts were accepted: {:?}",
                result.diagnostics
            );
        } else {
            assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
        }
    }
}

#[test]
fn forwarding_rejects_different_job_result_types() {
    check_forwarding("int", "bool", true);
}

#[test]
fn forwarding_accepts_identical_job_result_types() {
    check_forwarding("int", "int", false);
}

#[test]
fn forwarding_rejects_different_nested_job_result_types() {
    check_forwarding("Option<int>", "Option<bool>", true);
}

#[test]
fn forwarding_accepts_identical_nested_job_result_types() {
    check_forwarding("Option<int>", "Option<int>", false);
}

#[test]
fn forwarding_substitutes_result_parameters_before_comparing_bounds() {
    let mut db = DatabaseImpl::default();
    let file = db.new_file(
        "dependent_job.aru".into(),
        r#"
interface Job<R> { func run(shared self): R }
func consume<R, J: Job<R>>(job: J): void {}
func forward<S, J: Job<S>>(job: J): void { consume<S, J>(job) }
func main(): int { return 0 }
"#
        .into(),
    );
    let result = type_check(&db, file);
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
}

#[test]
fn dependent_where_bound_rejects_incompatible_instantiation() {
    let mut db = DatabaseImpl::default();
    let file = db.new_file(
        "dependent_where_job.aru".into(),
        r#"
interface Job<R> { func run(shared self): R }
func consume<R, J>(job: J): void where J: Job<R> {}
func forward<J>(job: J): void where J: Job<bool> { consume<int, J>(job) }
func main(): int { return 0 }
"#
        .into(),
    );
    let result = type_check(&db, file);
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == DiagCode::T025InterfaceNotSatisfied),
        "{:?}",
        result.diagnostics
    );
}

#[test]
fn inferred_method_checks_job_bound() {
    for (result_type, rejected) in [("int", false), ("bool", true)] {
        let mut db = DatabaseImpl::default();
        let file = db.new_file(
            "method_job.aru".into(),
            format!(
                r#"
interface Job<R> {{ func run(shared self): R }}
struct Runner {{}}
func Runner.consume<J: Job<int>>(shared self, job: J): void {{}}
func forward<J: Job<{result_type}>>(job: J): void {{
    let runner = Runner {{}}
    runner.consume(job)
}}
func main(): int {{ return 0 }}
"#
            ),
        );
        let result = type_check(&db, file);
        if rejected {
            assert!(
                result
                    .diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.code == DiagCode::T025InterfaceNotSatisfied),
                "{:?}",
                result.diagnostics
            );
        } else {
            assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
        }
    }
}

#[test]
fn inferred_module_call_checks_imported_job_bound() {
    for (result_type, rejected) in [("int", false), ("bool", true)] {
        let mut db = DatabaseImpl::default();
        db.new_file(
            "jobs.aru".into(),
            r#"
public interface Job<R> { func run(shared self): R }
public func consume<J: Job<int>>(job: J): void {}
"#
            .into(),
        );
        let file = db.new_file(
            "main.aru".into(),
            format!(
                r#"
import jobs
func forward<J: jobs.Job<{result_type}>>(job: J): void {{ jobs.consume(job) }}
func main(): int {{ return 0 }}
"#
            ),
        );
        let result = type_check(&db, file);
        if rejected {
            assert!(
                result
                    .diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.code == DiagCode::T025InterfaceNotSatisfied),
                "{:?}",
                result.diagnostics
            );
        } else {
            assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
        }
    }
}
