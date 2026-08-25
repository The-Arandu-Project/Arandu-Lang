//! Ahead-of-time emission of relocatable native object files.
//!
//! This backend deliberately shares AMIR lowering with the JIT. Its target ISA
//! uses Cranelift's baseline feature set for the selected triple instead of
//! probing the build machine, so an artifact never silently inherits host-only
//! CPU instructions.

use crate::jit::{AranduModule, codegen_ice};
use arandu_semantics::amir::AmirProgram;
use arandu_semantics::{Diagnostic, SymbolTable, TypeInfo};
use cranelift_codegen::settings::{self, Configurable};
use cranelift_module::default_libcall_names;
use cranelift_object::{ObjectBuilder, ObjectModule};
use target_lexicon::Triple;

/// A relocatable object emitted for a specific target triple.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjectArtifact {
    bytes: Vec<u8>,
    target: Triple,
}

impl ObjectArtifact {
    /// Returns the complete object-file payload.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Consumes the artifact and returns the object-file payload.
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }

    /// Returns the target triple encoded by the object format and ISA.
    pub fn target(&self) -> &Triple {
        &self.target
    }
}

/// Cranelift AOT backend that emits one relocatable object file.
pub struct CraneliftObjectBackend {
    target: Triple,
    compiler: AranduModule<ObjectModule>,
}

impl CraneliftObjectBackend {
    /// Creates a baseline backend for the current Rust host target.
    ///
    /// "Baseline" is important here: unlike the interactive JIT, this path
    /// does not call `cranelift_native`, which could select instructions only
    /// available on the machine performing the build.
    pub fn host_baseline() -> Result<Self, Diagnostic> {
        Self::for_target(Triple::host())
    }

    /// Creates a baseline backend for `target`.
    ///
    /// A target is accepted only when this Cranelift build contains its ISA.
    /// Linking and target runtime selection are intentionally later stages.
    pub fn for_target(target: Triple) -> Result<Self, Diagnostic> {
        let mut flag_builder = settings::builder();
        for (key, value) in [
            ("enable_verifier", "true"),
            ("use_colocated_libcalls", "false"),
            (
                "is_pic",
                if target.operating_system == target_lexicon::OperatingSystem::Windows {
                    "false"
                } else {
                    "true"
                },
            ),
            ("opt_level", "none"),
            ("preserve_frame_pointers", "true"),
        ] {
            flag_builder.set(key, value).map_err(|err| {
                codegen_ice(format!(
                    "failed to configure Cranelift AOT flag {key}={value}: {err}"
                ))
            })?;
        }

        let isa_builder = cranelift_codegen::isa::lookup(target.clone()).map_err(|err| {
            codegen_ice(format!(
                "Cranelift does not support AOT target '{target}': {err}"
            ))
        })?;
        let isa = isa_builder
            .finish(settings::Flags::new(flag_builder))
            .map_err(|err| {
                codegen_ice(format!(
                    "failed to build baseline ISA for AOT target '{target}': {err}"
                ))
            })?;
        let builder = ObjectBuilder::new(isa, b"arandu".to_vec(), default_libcall_names())
            .map_err(|err| {
                codegen_ice(format!(
                    "failed to create object module for target '{target}': {err}"
                ))
            })?;

        Ok(Self {
            target,
            compiler: AranduModule {
                module: ObjectModule::new(builder),
            },
        })
    }

    /// Lowers `program` and emits a relocatable native object.
    pub fn compile(
        mut self,
        program: &AmirProgram,
        symbols: &SymbolTable,
        type_info: &TypeInfo,
    ) -> Result<ObjectArtifact, Diagnostic> {
        self.compiler.compile_module(program, symbols, type_info)?;
        let product = self.compiler.module.finish();
        let bytes = product.emit().map_err(|err| {
            codegen_ice(format!(
                "failed to serialize object for target '{}': {err}",
                self.target
            ))
        })?;

        Ok(ObjectArtifact {
            bytes,
            target: self.target,
        })
    }
}
