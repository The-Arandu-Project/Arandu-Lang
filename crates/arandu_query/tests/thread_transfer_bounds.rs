//! Canonical concurrency markers must inspect storage, not empty method lists.
use arandu_diagnostics::{DiagCode, Severity};
use arandu_query::{passes::type_check, DatabaseImpl};

fn check_storage(storage: &str, accepted: bool) {
    for capability in ["Send", "Sync"] {
        let mut db = DatabaseImpl::default();
        db.new_file(
            "stdlib/core/marker.aru".into(),
            include_str!("../../../stdlib/core/marker.aru").into(),
        );
        let file = db.new_file(
            "transfer.aru".into(),
            format!(
                r#"
import std.core.marker as marker
struct Wrapper<T> {{ value: T }}
enum Choice<T> {{ Empty
Present(T) }}
struct Payload {{ value: {storage} }}
func require<T: marker.{capability}>(value: T): void {{}}
func inspect(value: Payload): void {{ require(value) }}
func main(): int {{ return 0 }}
"#
            ),
        );
        let result = type_check(&db, file);
        if accepted {
            assert!(
                !result
                    .diagnostics
                    .iter()
                    .any(|d| d.severity == Severity::Error),
                "{capability}<{storage}>: {:?}",
                result.diagnostics
            );
        } else {
            assert!(
                result
                    .diagnostics
                    .iter()
                    .any(|d| d.code == DiagCode::T025InterfaceNotSatisfied),
                "{capability}<{storage}> was not rejected: {:?}",
                result.diagnostics
            );
        }
    }
}

#[test]
fn scalar_aggregates_satisfy_transfer_bounds() {
    check_storage("int", true);
    check_storage("Option<bool>", true);
    check_storage("Wrapper<Wrapper<int>>", true);
    check_storage("Choice<int>", true);
}

#[test]
fn raw_pointer_aggregates_do_not_satisfy_transfer_bounds() {
    check_storage("ptr[u8]", false);
    check_storage("Option<ptr[u8]>", false);
    check_storage("Wrapper<ptr[u8]>", false);
    check_storage("Choice<ptr[u8]>", false);
    check_storage("marker.PhantomData<ptr[u8]>", false);
}

#[test]
fn cooperative_task_handle_requires_its_own_transfer_contract() {
    let mut db = DatabaseImpl::default();
    db.new_file(
        "stdlib/core/marker.aru".into(),
        include_str!("../../../stdlib/core/marker.aru").into(),
    );
    db.new_file(
        "stdlib/std/runtime/executor.aru".into(),
        include_str!("../../../stdlib/std/runtime/executor.aru").into(),
    );
    let file = db.new_file(
        "task_handle.aru".into(),
        r#"
import std.core.marker as marker
import std.runtime.executor as rt
func require<T: marker.Send>(value: T): void {}
func inspect(value: rt.TaskHandle<int>): void { require(value) }
func main(): int { return 0 }
"#
        .into(),
    );
    let result = type_check(&db, file);
    assert!(
        result
            .diagnostics
            .iter()
            .any(|d| d.code == DiagCode::T025InterfaceNotSatisfied),
        "{:?}",
        result.diagnostics
    );
}

#[test]
fn imported_wrapper_does_not_hide_a_cooperative_handle() {
    let mut db = DatabaseImpl::default();
    db.new_file(
        "stdlib/core/marker.aru".into(),
        include_str!("../../../stdlib/core/marker.aru").into(),
    );
    db.new_file(
        "stdlib/std/runtime/executor.aru".into(),
        include_str!("../../../stdlib/std/runtime/executor.aru").into(),
    );
    db.new_file(
        "bridge.aru".into(),
        r#"
import std.runtime.executor as rt
public struct Envelope { handle: rt.TaskHandle<int> }
"#
        .into(),
    );
    let file = db.new_file(
        "consumer.aru".into(),
        r#"
import bridge
import std.core.marker as marker
func require<T: marker.Send>(value: T): void {}
func inspect(value: bridge.Envelope): void { require(value) }
func main(): int { return 0 }
"#
        .into(),
    );
    let result = type_check(&db, file);
    assert!(
        result
            .diagnostics
            .iter()
            .any(|d| d.code == DiagCode::T025InterfaceNotSatisfied),
        "{:?}",
        result.diagnostics
    );
}

#[test]
fn borrowed_storage_requires_a_separate_lifetime_proof() {
    check_storage("str", false);
    check_storage("[]u8", false);
    check_storage("ref int", false);
    check_storage("mut ref int", false);
}

fn check_program(source: &str, rejected: bool) {
    let mut db = DatabaseImpl::default();
    db.new_file(
        "stdlib/core/marker.aru".into(),
        include_str!("../../../stdlib/core/marker.aru").into(),
    );
    let file = db.new_file("capability.aru".into(), source.into());
    let result = type_check(&db, file);
    if rejected {
        assert!(
            result
                .diagnostics
                .iter()
                .any(|d| d.code == DiagCode::T025InterfaceNotSatisfied),
            "{:?}",
            result.diagnostics
        );
    } else {
        assert!(
            !result
                .diagnostics
                .iter()
                .any(|d| d.severity == Severity::Error),
            "{:?}",
            result.diagnostics
        );
    }
}

#[test]
fn canonical_marker_survives_named_import_alias() {
    check_program(
        r#"
from std.core.marker import { Send as Transfer }
struct Payload { pointer: ptr[u8] }
func require<T: Transfer>(value: T): void {}
func inspect(value: Payload): void { require(value) }
func main(): int { return 0 }
"#,
        true,
    );
}

#[test]
fn user_interface_named_send_is_not_a_compiler_capability() {
    check_program(
        r#"
interface Send {}
struct Payload { pointer: ptr[u8] }
func require<T: Send>(value: T): void {}
func inspect(value: Payload): void { require(value) }
func main(): int { return 0 }
"#,
        false,
    );
}

#[test]
fn user_bound_cannot_prove_canonical_transfer() {
    check_program(
        r#"
import std.core.marker as marker
interface Send {}
func require<T: marker.Send>(value: T): void {}
func forward<T: Send>(value: T): void { require(value) }
func main(): int { return 0 }
"#,
        true,
    );
}

#[test]
fn generic_fields_use_their_declared_capability() {
    for (bound, rejected) in [("Send", false), ("Sync", true)] {
        check_program(
            &format!(
                r#"
import std.core.marker as marker
struct Wrapper<T> {{ value: T }}
func require<T: marker.Send>(value: T): void {{}}
func forward<T: marker.{bound}>(value: Wrapper<T>): void {{ require(value) }}
func main(): int {{ return 0 }}
"#
            ),
            rejected,
        );
    }
}

#[test]
fn scalar_arguments_can_be_inferred() {
    check_program(
        r#"
import std.core.marker as marker
func require<T: marker.Send>(value: T): void {}
func main(): int { require(42); require(true); require(1.5); return 0 }
"#,
        false,
    );
}

#[test]
fn destructor_resource_is_not_approved_from_scalar_fields() {
    check_program(
        r#"
import std.core.marker as marker
struct Resource { id: int }
@Destructor
func Resource.close(own self): void {}
func require<T: marker.Send>(value: T): void {}
func inspect(value: Resource): void { require(value) }
func main(): int { return 0 }
"#,
        true,
    );
}
