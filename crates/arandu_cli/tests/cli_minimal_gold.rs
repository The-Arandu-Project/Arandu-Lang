//! Arandu Minimal 0.1 gold suite — `examples/minimal/*`.
//!
//! Tracking: docs/arandu-compiler-roadmap-v0.1.md (Minimal 0.1 campaign).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::process::Command;

fn workspace_root() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn run_cli(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_arandu_cli"))
        .current_dir(workspace_root())
        .args(args)
        .output()
        .expect("cli")
}

struct Gold {
    path: &'static str,
    /// Expected process exit code from `run` (main return value).
    exit: i32,
}

const GOLD: &[Gold] = &[
    Gold {
        path: "examples/minimal/m01_hello.aru",
        exit: 0,
    },
    Gold {
        path: "examples/minimal/m02_structs_enums.aru",
        exit: 3,
    },
    Gold {
        path: "examples/minimal/m03_result_option.aru",
        exit: 7,
    },
    Gold {
        path: "examples/minimal/m04_generics_bounds.aru",
        exit: 10,
    },
    Gold {
        path: "examples/minimal/m05_borrow_shared.aru",
        exit: 5,
    },
    Gold {
        path: "examples/minimal/m06_async_await.aru",
        exit: 42,
    },
    Gold {
        path: "examples/minimal/m07_async_spawn_join.aru",
        exit: 42,
    },
    Gold {
        path: "examples/minimal/m08_modules/main.aru",
        exit: 9,
    },
    Gold {
        path: "examples/minimal/m09_interp_tostr.aru",
        exit: 0,
    },
    Gold {
        path: "examples/minimal/m10_path_empty.aru",
        exit: 0,
    },
    Gold {
        path: "examples/minimal/m11_process_exit.aru",
        exit: 17,
    },
    Gold {
        path: "examples/minimal/m12_time_env.aru",
        exit: 0,
    },
    Gold {
        path: "examples/minimal/m13_vec.aru",
        exit: 78,
    },
    Gold {
        path: "examples/minimal/m14_mem_intrinsics.aru",
        exit: 46,
    },
    Gold {
        path: "examples/minimal/m15_vec_capacity.aru",
        exit: 21,
    },
    Gold {
        path: "examples/minimal/m16_gen_arena.aru",
        exit: 83,
    },
    Gold {
        path: "examples/minimal/m17_pod_copy.aru",
        exit: 60,
    },
    Gold {
        path: "examples/minimal/m18_vec_methods.aru",
        exit: 78,
    },
    Gold {
        path: "examples/minimal/m19_allocator.aru",
        exit: 112,
    },
    Gold {
        path: "examples/minimal/m20_str.aru",
        exit: 0,
    },
    Gold {
        path: "examples/minimal/m21_result_custom_e.aru",
        exit: 7,
    },
    Gold {
        path: "examples/minimal/m22_iface_param.aru",
        exit: 42,
    },
    Gold {
        path: "examples/minimal/m23_match_result.aru",
        exit: 13,
    },
    Gold {
        path: "examples/minimal/m24_expect_or_abort.aru",
        exit: 13,
    },
    Gold {
        path: "examples/minimal/TEMPLATE_main.aru",
        exit: 0,
    },
];

#[test]
fn minimal_gold_check_and_run() {
    for g in GOLD {
        let path = if cfg!(windows) && g.path == "examples/minimal/m10_path_empty.aru" {
            "examples/minimal/m10_path_empty_windows.aru"
        } else {
            g.path
        };
        let check = run_cli(&["check", path]);
        assert!(
            check.status.success(),
            "check failed {}: {}",
            path,
            String::from_utf8_lossy(&check.stderr)
        );
        let run = run_cli(&["run", path]);
        assert_eq!(
            run.status.code(),
            Some(g.exit),
            "run exit mismatch {}: stderr={}",
            path,
            String::from_utf8_lossy(&run.stderr)
        );
    }
}

#[test]
fn ptr_compat_module_checks() {
    // P0.1: broken twin fixed — offset uses ptrOffset.
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_minimal_ptr.aru");
    std::fs::write(
        &file,
        r#"
module tests.minimal.ptr
import std.core.ptr as p
import std.core.pointer as pointer

func main(): int {
    // Module resolves; no need to call offset without a real pointer.
    return 0
}
"#,
    )
    .unwrap();
    let root = workspace_root();
    let out = Command::new(env!("CARGO_BIN_EXE_arandu_cli"))
        .current_dir(&root)
        .args(["check", file.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "ptr compat: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
