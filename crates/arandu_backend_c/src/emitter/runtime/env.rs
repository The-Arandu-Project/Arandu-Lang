//! C runtime helpers for environment variables and command line arguments (std.env).

use std::fmt::Write;

use super::super::CEmitter;

impl<'a> CEmitter<'a> {
    pub(super) fn emit_env_runtime(&mut self, int_c_ty: &str) {
        let _ = writeln!(
            &mut self.output,
            r#"/* std.env host helpers (mirrors JIT ar_env_* runtime). */
static int ar_env_c_argc = 0;
static char **ar_env_c_argv = NULL;

#if defined(_WIN32)
static void ar_init_env_args_if_needed(void) {{
    if (!ar_env_c_argv) {{
        ar_env_c_argc = __argc;
        ar_env_c_argv = __argv;
    }}
}}
#elif defined(__GNUC__) || defined(__clang__)
__attribute__((constructor)) static void ar_capture_env_args(int argc, char **argv) {{
    ar_env_c_argc = argc;
    ar_env_c_argv = argv;
}}
#endif

static {int_c_ty} ar_env_args_len(void) {{
#if defined(_WIN32)
    ar_init_env_args_if_needed();
#endif
    return ({int_c_ty})ar_env_c_argc;
}}

static ArStr ar_env_arg({int_c_ty} index) {{
#if defined(_WIN32)
    ar_init_env_args_if_needed();
#endif
    if (index < 0 || index >= ({int_c_ty})ar_env_c_argc || !ar_env_c_argv) {{
        return ar_str_pack((const uint8_t*)"", 0);
    }}
    const char *s = ar_env_c_argv[index];
    size_t len = s ? strlen(s) : 0;
    return ar_str_pack((const uint8_t*)s, ({int_c_ty})len);
}}

static {int_c_ty} ar_env_var_is_set(ArStr name) {{
    if (name.len <= 0 || !name.ptr) return 0;
    char stack_buf[256];
    char *c_name = ar_str_to_c_path(name, stack_buf, sizeof(stack_buf));
    if (!c_name) return 0;
    char *val = getenv(c_name);
    if (c_name != stack_buf) free(c_name);
    return val != NULL ? 1 : 0;
}}"#
        );
    }
}
