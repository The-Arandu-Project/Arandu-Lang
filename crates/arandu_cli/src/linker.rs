//! Host-native linker selection for project AOT builds.

use crate::artifact;
use crate::cli_error::CliFailure;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Linker provenance recorded alongside the final artifact.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkerKind {
    System,
    RustcDevelopmentFallback,
}

impl LinkerKind {
    pub const fn label(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::RustcDevelopmentFallback => "rustc-development-fallback",
        }
    }
}

/// Finds the runtime library belonging to the current Arandu installation.
pub fn runtime_library() -> Result<PathBuf, CliFailure> {
    let filename = if cfg!(windows) {
        "arandu_runtime.lib"
    } else {
        "libarandu_runtime.a"
    };
    let mut candidates = Vec::new();
    if let Some(explicit) = std::env::var_os("ARANDU_RUNTIME_LIB") {
        candidates.push(PathBuf::from(explicit));
    }
    if let Ok(executable) = std::env::current_exe()
        && let Some(bin) = executable.parent()
    {
        // Cargo development layout: target/{debug,release}/arandu_cli.
        candidates.push(bin.join(filename));
        // Installed SDK layout: bin/arandu + lib/<host>/runtime.
        if let Some(prefix) = bin.parent() {
            candidates.push(
                prefix
                    .join("lib")
                    .join(artifact::host_triple())
                    .join(filename),
            );
            candidates.push(prefix.join("lib").join(filename));
        }
    }

    candidates
        .iter()
        .find(|candidate| candidate.is_file())
        .cloned()
        .ok_or_else(|| {
            CliFailure::operational(
                "locate Arandu AOT runtime",
                None,
                format!(
                    "expected {filename}; set ARANDU_RUNTIME_LIB or reinstall the SDK (searched: {})",
                    candidates
                        .iter()
                        .map(|path| path.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            )
        })
}

/// Links a Cranelift object with the target-matched Arandu runtime.
pub fn link(object: &Path, output: &Path) -> Result<LinkerKind, CliFailure> {
    let runtime = runtime_library()?;
    if let Some(explicit) = std::env::var_os("ARANDU_LINKER") {
        return match run_system_linker(&explicit, object, &runtime, output) {
            Ok(()) => Ok(LinkerKind::System),
            Err(LinkAttempt::NotFound) => Err(link_failure(
                output,
                format!(
                    "configured ARANDU_LINKER '{}' was not found",
                    Path::new(&explicit).display()
                ),
            )),
            Err(LinkAttempt::Failed(message)) => Err(link_failure(output, message)),
        };
    }

    for candidate in system_linker_candidates() {
        match run_system_linker(&candidate, object, &runtime, output) {
            Ok(()) => return Ok(LinkerKind::System),
            Err(LinkAttempt::NotFound) => {}
            Err(LinkAttempt::Failed(message)) => {
                return Err(link_failure(output, message));
            }
        }
    }

    #[cfg(windows)]
    match run_discovered_msvc(object, &runtime, output) {
        Ok(()) => return Ok(LinkerKind::System),
        Err(LinkAttempt::NotFound) => {}
        Err(LinkAttempt::Failed(message)) => return Err(link_failure(output, message)),
    }

    // This fallback keeps compiler-development checkouts testable without a
    // separately configured native linker. Public SDKs do not contain rustc;
    // their native release smoke must exercise the system path above.
    link_with_rustc(object, &runtime, output)?;
    Ok(LinkerKind::RustcDevelopmentFallback)
}

enum LinkAttempt {
    NotFound,
    Failed(String),
}

fn system_linker_candidates() -> Vec<OsString> {
    if cfg!(windows) {
        Vec::new()
    } else {
        vec![OsString::from("cc"), OsString::from("clang")]
    }
}

#[cfg(windows)]
fn run_discovered_msvc(object: &Path, runtime: &Path, output: &Path) -> Result<(), LinkAttempt> {
    use std::collections::HashMap;

    let Some(program_files) = std::env::var_os("ProgramFiles(x86)") else {
        return Err(LinkAttempt::NotFound);
    };
    let vswhere = PathBuf::from(program_files)
        .join("Microsoft Visual Studio")
        .join("Installer")
        .join("vswhere.exe");
    if !vswhere.is_file() {
        return Err(LinkAttempt::NotFound);
    }
    let discovery = Command::new(&vswhere)
        .args([
            "-latest",
            "-products",
            "*",
            "-requires",
            "Microsoft.VisualStudio.Component.VC.Tools.x86.x64",
            "-property",
            "installationPath",
        ])
        .output()
        .map_err(|error| LinkAttempt::Failed(format!("could not run vswhere: {error}")))?;
    if !discovery.status.success() {
        return Err(LinkAttempt::Failed(format_output(&vswhere, &discovery)));
    }
    let installation = String::from_utf8_lossy(&discovery.stdout).trim().to_owned();
    if installation.is_empty() {
        return Err(LinkAttempt::NotFound);
    }
    let dev_command = PathBuf::from(installation)
        .join("Common7")
        .join("Tools")
        .join("VsDevCmd.bat");
    if !dev_command.is_file() {
        return Err(LinkAttempt::NotFound);
    }
    let environment_script = output.with_extension("msvc-env.cmd");
    fs::write(
        &environment_script,
        format!(
            "@call \"{}\" -no_logo -arch=x64 -host_arch=x64 >nul\r\n@set\r\n",
            dev_command.display()
        ),
    )
    .map_err(|error| {
        LinkAttempt::Failed(format!(
            "could not create the temporary MSVC environment script: {error}"
        ))
    })?;
    let environment = Command::new(&environment_script).output();
    let _ = fs::remove_file(&environment_script);
    let environment = environment.map_err(|error| {
        LinkAttempt::Failed(format!(
            "could not initialize the MSVC environment: {error}"
        ))
    })?;
    if !environment.status.success() {
        return Err(LinkAttempt::Failed(format_output(
            "VsDevCmd.bat",
            &environment,
        )));
    }
    let variables = String::from_utf8_lossy(&environment.stdout)
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .collect::<HashMap<_, _>>();
    let Some(path_value) = variables
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("PATH"))
        .map(|(_, value)| value)
    else {
        return Err(LinkAttempt::Failed(
            "VsDevCmd did not provide PATH".to_string(),
        ));
    };
    let Some(linker) = std::env::split_paths(path_value)
        .map(|directory| directory.join("link.exe"))
        .find(|candidate| candidate.is_file())
    else {
        return Err(LinkAttempt::NotFound);
    };

    run_system_linker_with_environment(&linker, object, runtime, output, &variables)
}

#[cfg(windows)]
fn run_system_linker_with_environment(
    linker: &Path,
    object: &Path,
    runtime: &Path,
    output: &Path,
    environment: &std::collections::HashMap<String, String>,
) -> Result<(), LinkAttempt> {
    let result = Command::new(linker)
        .envs(environment)
        .arg("/NOLOGO")
        .arg("/INCREMENTAL:NO")
        .arg("/Brepro")
        .arg("/SUBSYSTEM:CONSOLE")
        .arg(format!("/OUT:{}", output.display()))
        .arg(object)
        .arg(runtime)
        .args([
            "kernel32.lib",
            "ntdll.lib",
            "userenv.lib",
            "ws2_32.lib",
            "dbghelp.lib",
            "msvcrt.lib",
        ])
        .output()
        .map_err(|error| LinkAttempt::Failed(format!("could not start MSVC linker: {error}")))?;
    if result.status.success() {
        Ok(())
    } else {
        Err(LinkAttempt::Failed(format_output(linker, &result)))
    }
}

fn run_system_linker(
    linker: &std::ffi::OsStr,
    object: &Path,
    runtime: &Path,
    output: &Path,
) -> Result<(), LinkAttempt> {
    let mut command = Command::new(linker);
    if cfg!(windows) {
        command
            .arg("/NOLOGO")
            .arg("/INCREMENTAL:NO")
            .arg("/Brepro")
            .arg("/SUBSYSTEM:CONSOLE")
            .arg(format!("/OUT:{}", output.display()))
            .arg(object)
            .arg(runtime)
            .args([
                "kernel32.lib",
                "ntdll.lib",
                "userenv.lib",
                "ws2_32.lib",
                "dbghelp.lib",
                "msvcrt.lib",
            ]);
    } else {
        command.arg(object).arg(runtime).arg("-o").arg(output);
        #[cfg(target_os = "linux")]
        command.args([
            "-Wl,--gc-sections",
            "-Wl,--build-id=sha1",
            "-lgcc_s",
            "-lutil",
            "-lrt",
            "-lpthread",
            "-lm",
            "-ldl",
            "-lc",
        ]);
        #[cfg(target_os = "macos")]
        command.args([
            "-Wl,-dead_strip",
            "-Wl,-no_uuid",
            "-framework",
            "Security",
            "-framework",
            "CoreFoundation",
            "-liconv",
            "-lSystem",
            "-lc",
            "-lm",
        ]);
    }
    match command.output() {
        Ok(result) if result.status.success() => Ok(()),
        Ok(result) => Err(LinkAttempt::Failed(format_output(linker, &result))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Err(LinkAttempt::NotFound),
        Err(error) => Err(LinkAttempt::Failed(format!(
            "could not start '{}': {error}",
            Path::new(linker).display()
        ))),
    }
}

fn link_with_rustc(object: &Path, runtime: &Path, output: &Path) -> Result<(), CliFailure> {
    let stub = output.with_extension("link.rs");
    fs::write(&stub, "#![no_main]\n").map_err(|error| {
        CliFailure::operational(
            "create development linker stub",
            Some(stub.clone()),
            error.to_string(),
        )
    })?;
    let result = Command::new("rustc")
        .args(["--crate-name", "arandu_link", "--edition", "2024"])
        .arg(&stub)
        .arg("-o")
        .arg(output)
        .arg("-C")
        .arg(format!("link-arg={}", object.display()))
        .arg("-C")
        .arg(format!("link-arg={}", runtime.display()))
        .args(rustc_reproducible_link_args())
        .output();
    let _ = fs::remove_file(&stub);
    match result {
        Ok(result) if result.status.success() => Ok(()),
        Ok(result) => Err(link_failure(output, format_output("rustc", &result))),
        Err(error) => Err(CliFailure::operational(
            "link native artifact",
            Some(output.to_path_buf()),
            format!(
                "no supported system linker was found and the checkout-only rustc fallback failed: {error}"
            ),
        )),
    }
}

fn rustc_reproducible_link_args() -> Vec<&'static str> {
    if cfg!(windows) {
        vec!["-C", "link-arg=/Brepro"]
    } else if cfg!(target_os = "macos") {
        vec!["-C", "link-arg=-Wl,-no_uuid"]
    } else {
        vec!["-C", "link-arg=-Wl,--build-id=sha1"]
    }
}

fn format_output(linker: impl AsRef<std::ffi::OsStr>, output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    format!(
        "'{}' exited with {}\n{}{}",
        Path::new(linker.as_ref()).display(),
        output.status,
        stdout,
        stderr
    )
}

fn link_failure(output: &Path, message: String) -> CliFailure {
    CliFailure::operational("link native artifact", Some(output.to_path_buf()), message)
}
