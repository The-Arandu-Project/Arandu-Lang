#![cfg(target_pointer_width = "64")]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use arandu_backend_cranelift::CraneliftObjectBackend;
use arandu_semantics::{lower_to_amir, lower_to_hir, resolve_for_test, type_check};
use cranelift_object::object::{self, Object, ObjectSection, ObjectSymbol};
use std::sync::Arc;

fn compile_object(src: &str) -> arandu_backend_cranelift::ObjectArtifact {
    let program = arandu_parser::parse(src).expect("parse failed");
    let resolution = resolve_for_test(0, &program);
    let mut tc = type_check(resolution, &program);
    let hir = lower_to_hir(&mut tc, &program).expect("HIR lowering failed");
    let amir = lower_to_amir(&tc, &hir).expect("AMIR lowering failed");
    let symbols = Arc::unwrap_or_clone(tc.symbols);
    let type_info = Arc::unwrap_or_clone(tc.type_info);

    CraneliftObjectBackend::host_baseline()
        .expect("host baseline ISA should be supported")
        .compile(&amir, &symbols, &type_info)
        .expect("object emission should succeed")
}

#[test]
fn emits_parseable_host_object_with_defined_function() {
    let artifact = compile_object("func main(): int { return 42; }");
    let file = object::File::parse(artifact.bytes()).expect("valid native object");

    #[cfg(target_os = "windows")]
    assert_eq!(file.format(), object::BinaryFormat::Coff);
    #[cfg(target_os = "linux")]
    assert_eq!(file.format(), object::BinaryFormat::Elf);
    #[cfg(target_os = "macos")]
    assert_eq!(file.format(), object::BinaryFormat::MachO);

    #[cfg(target_arch = "x86_64")]
    assert_eq!(file.architecture(), object::Architecture::X86_64);
    #[cfg(target_arch = "aarch64")]
    assert_eq!(file.architecture(), object::Architecture::Aarch64);

    let main = file
        .symbols()
        .find(|symbol| symbol.name() == Ok("main"))
        .expect("object must define the source function");
    assert!(main.is_definition());
    assert!(main.is_global());
    assert!(
        file.sections()
            .any(|section| section.kind() == object::SectionKind::Text && section.size() > 0),
        "object must contain native code"
    );
}

#[test]
fn baseline_object_emission_is_byte_deterministic() {
    let src = "func add(a: int, b: int): int { return a + b; }";
    let first = compile_object(src);
    let second = compile_object(src);

    assert_eq!(first.target(), second.target());
    assert_eq!(first.bytes(), second.bytes());
}

#[test]
fn release_object_emission_is_byte_deterministic() {
    let src = "func add(a: int, b: int): int { return a + b; }";
    let program = arandu_parser::parse(src).expect("parse failed");
    let resolution = resolve_for_test(0, &program);
    let mut tc = type_check(resolution, &program);
    let hir = lower_to_hir(&mut tc, &program).expect("HIR lowering failed");
    let amir = lower_to_amir(&tc, &hir).expect("AMIR lowering failed");
    let symbols = Arc::unwrap_or_clone(tc.symbols);
    let type_info = Arc::unwrap_or_clone(tc.type_info);

    let emit = || {
        CraneliftObjectBackend::host_release()
            .expect("host release ISA should be supported")
            .compile(&amir, &symbols, &type_info)
            .expect("release object emission should succeed")
    };
    let first = emit();
    let second = emit();
    assert_eq!(first.target(), second.target());
    assert_eq!(first.bytes(), second.bytes());
}
