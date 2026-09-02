//! Project native object build and linking.

use std::path::Path;

use crate::artifact;
use crate::cli_error::{CliFailure, CliResult, CliSuccess};
use crate::linker;
use crate::pipeline::{
    open_entry_file, optimize_amir_or_exit, optimize_amir_with_level_or_exit, pipeline_lower,
    print_diagnostics_and_exit,
};
use crate::project::{self, ProjectFlags};
use arandu_middle::layout::DataLayout;

pub fn cmd_project_build(
    start: &Path,
    flags: &ProjectFlags,
    opt: bool,
    _debug: bool,
    data_layout: DataLayout,
) -> CliResult {
    let backend = project::BackendChoice::from_release_flag(flags.release);
    let (mut db, rebuild_log) = arandu_query::DatabaseImpl::with_rebuild_log();
    db.set_target_config(data_layout);
    let ctx = match project::load_project(&mut db, start, flags) {
        Ok(c) => c,
        Err(e) => {
            return Err(CliFailure::operational(
                "load project",
                Some(start.into()),
                e,
            ));
        }
    };
    let mut registry = arandu_base::SourceRegistry::default();
    let (file, filepath) = open_entry_file(&db, &mut registry, &ctx.entry_path);
    let artifacts = pipeline_lower(&db, file, &filepath);
    eprintln!("{}", rebuild_log.status_line());

    // Dev "build" = typecheck + lower + relocatable native object emission.
    let type_check = &artifacts.type_check;
    let mut amir_owned = if opt || flags.release {
        Some(artifacts.amir.clone())
    } else {
        None
    };
    if let Some(ref mut amir) = amir_owned {
        if flags.release {
            optimize_amir_with_level_or_exit(
                amir,
                type_check,
                arandu_semantics::OptLevel::O2,
                &filepath,
            );
        } else {
            optimize_amir_or_exit(amir, type_check, &filepath);
        }
    }
    let amir = match &amir_owned {
        Some(a) => a,
        None => &artifacts.amir,
    };

    let backend_impl = {
        let Some(target) =
            arandu_backend_cranelift::aot_triple_for_pointer_width(data_layout.pointer_width())
        else {
            return Err(CliFailure::operational(
                "select AOT target",
                Some(format!("pointer-width={}", data_layout.pointer_width()).into()),
                format!(
                    "no Cranelift AOT target for pointer width {} (host is {}-bit); \
                     32-bit Cranelift emission is unsupported — use a layout matching \
                     the host or the C backend",
                    data_layout.pointer_width(),
                    std::mem::size_of::<usize>() * 8,
                ),
            ));
        };
        let optimization = match backend {
            project::BackendChoice::CraneliftDev => {
                arandu_backend_cranelift::AotOptimization::Baseline
            }
            project::BackendChoice::CraneliftRelease => {
                arandu_backend_cranelift::AotOptimization::Speed
            }
        };
        let backend_impl =
            arandu_backend_cranelift::CraneliftObjectBackend::for_target_with_optimization(
                target,
                optimization,
            );
        match backend_impl {
            Ok(b) => b,
            Err(diag) => print_diagnostics_and_exit(std::iter::once(diag), &filepath),
        }
    };
    match backend_impl.compile(
        amir,
        type_check.symbols.as_ref(),
        type_check.type_info.as_ref(),
    ) {
        Ok(object) => {
            let artifact = artifact::publish_native_artifact(
                &ctx.root,
                &ctx.name,
                &ctx.version,
                if flags.release {
                    artifact::NativeProfile::Release
                } else {
                    artifact::NativeProfile::Dev
                },
                object.bytes(),
                |object, output| linker::link(object, output).map(|kind| kind.label()),
            )?;
            println!(
                "built {} v{} (backend={}, entry={}, artifact={})",
                ctx.name,
                ctx.version,
                backend.label(),
                ctx.entry_rel,
                artifact.display()
            );
            Ok(CliSuccess::Done)
        }
        Err(diag) => print_diagnostics_and_exit(std::iter::once(diag), &filepath),
    }
}
