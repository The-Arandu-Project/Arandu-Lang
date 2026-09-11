//! C preamble, headers, string runtime, coroutine polling, and memory helpers.

use std::fmt::Write;

use arandu_middle::amir::{AmirOperand, AmirStmt};
use arandu_middle::literal_pool::AmirLiteralEntry;
use arandu_middle::types::{ArType, Primitive};

use super::CEmitter;

impl<'a> CEmitter<'a> {
    /// True if any call targets prelude `io.println` (symbol name or C sanitization).
    pub(super) fn program_uses_println(&self) -> bool {
        for func in &self.program.funcs {
            for stmt in func.stmts.payloads.iter() {
                if let AmirStmt::Call { callee, .. } = stmt
                    && let AmirOperand::FunctionRef(id) = callee
                {
                    let name = self.symbols.get(*id).name.as_str();
                    if name == "io.println" || name.ends_with(".println") || name == "println" {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// Emit `io__println` matching sanitize_c_ident("io.println").
    pub(super) fn emit_prelude_println(&mut self) {
        let _ = writeln!(&mut self.output, "static void io__println(ArStr s) {{");
        let _ = writeln!(
            &mut self.output,
            "    if (s.len > 0 && s.ptr) {{ fwrite(s.ptr, 1, (size_t)s.len, stdout); }}"
        );
        let _ = writeln!(&mut self.output, "    fputc('\\n', stdout);");
        let _ = writeln!(&mut self.output, "    fflush(stdout);");
        let _ = writeln!(&mut self.output, "}}");
        let _ = writeln!(&mut self.output);
    }

    /// Whether any local/temp/return or pool entry needs the ArStr runtime.
    pub(super) fn program_uses_str(&self) -> bool {
        if self
            .program
            .literal_pool
            .entries
            .iter()
            .any(|e| matches!(e, AmirLiteralEntry::Str(_)))
        {
            return true;
        }
        for func in &self.program.funcs {
            let ret = self.interner.resolve(func.return_type);
            if matches!(ret, ArType::Primitive(Primitive::Str)) {
                return true;
            }
            for local in &func.locals {
                if matches!(
                    self.interner.resolve(local.ty),
                    ArType::Primitive(Primitive::Str)
                ) {
                    return true;
                }
            }
            for temp in &func.temps {
                if matches!(
                    self.interner.resolve(temp.ty),
                    ArType::Primitive(Primitive::Str)
                ) {
                    return true;
                }
            }
        }
        false
    }

    pub(super) fn emit_gen_arena_runtime(&mut self) {
        let _ = writeln!(
            &mut self.output,
            r#"/* G4 type-erased GenRef ABI: monotonic tokens, target layout, ordered drops. */
typedef void (*ar_gen_drop_fn)(void *);
typedef struct ar_gen_entry {{
    uint64_t token;
    void *data;
    void *allocation;
    size_t size;
    size_t align;
    ar_gen_drop_fn drop;
    struct ar_gen_entry *next;
}} ar_gen_entry;
static ar_gen_entry *ar_gen_head = NULL;
static ar_gen_entry *ar_gen_tail = NULL;
static uint64_t ar_gen_next_token = 0;
static int ar_gen_valid_layout(size_t size, size_t align) {{
    return align != 0 && (align & (align - 1)) == 0 && size <= SIZE_MAX - (align - 1);
}}
static void *ar_gen_alloc_aligned(size_t size, size_t align, void **allocation) {{
    if (!ar_gen_valid_layout(size, align)) return NULL;
    size_t bytes = size == 0 ? 1 : size;
    if (bytes > SIZE_MAX - (align - 1)) return NULL;
    void *raw = malloc(bytes + align - 1);
    if (!raw) return NULL;
    uintptr_t base = (uintptr_t)raw;
    uintptr_t aligned = (base + (align - 1)) & ~(uintptr_t)(align - 1);
    *allocation = raw;
    return (void *)aligned;
}}
static ar_gen_entry *ar_gen_find(uint64_t token) {{
    for (ar_gen_entry *entry = ar_gen_head; entry; entry = entry->next)
        if (entry->token == token) return entry;
    return NULL;
}}
static uint64_t ar_gen_insert_raw(void *source, size_t size, size_t align, ar_gen_drop_fn drop) {{
    if (!source || ar_gen_next_token == UINT64_MAX) return 0;
    ar_gen_entry *entry = (ar_gen_entry *)malloc(sizeof(ar_gen_entry));
    if (!entry) return 0;
    entry->data = ar_gen_alloc_aligned(size, align, &entry->allocation);
    if (!entry->data) {{ free(entry); return 0; }}
    if (size != 0) memcpy(entry->data, source, size);
    entry->token = ++ar_gen_next_token;
    entry->size = size; entry->align = align; entry->drop = drop; entry->next = NULL;
    if (ar_gen_tail) ar_gen_tail->next = entry; else ar_gen_head = entry;
    ar_gen_tail = entry;
    return entry->token;
}}
static int ar_gen_get_raw(uint64_t token, void *destination, size_t size, size_t align) {{
    ar_gen_entry *entry = ar_gen_find(token);
    if (!entry || !destination || entry->size != size || entry->align != align) return 0;
    if (size != 0) memcpy(destination, entry->data, size);
    return 1;
}}
static int ar_gen_set_raw(uint64_t token, void *source, size_t size, size_t align, ar_gen_drop_fn drop) {{
    ar_gen_entry *entry = ar_gen_find(token);
    if (!entry || !source || entry->size != size || entry->align != align) return 0;
    void *new_allocation = NULL;
    void *new_data = ar_gen_alloc_aligned(size, align, &new_allocation);
    if (!new_data) return 0;
    if (size != 0) memcpy(new_data, source, size);
    void *old_data = entry->data; void *old_allocation = entry->allocation;
    ar_gen_drop_fn old_drop = entry->drop;
    entry->data = new_data; entry->allocation = new_allocation; entry->drop = drop;
    if (old_drop) old_drop(old_data);
    free(old_allocation);
    return 1;
}}
static uint64_t ar_gen_upsert_raw(uint64_t token, void *source, size_t size, size_t align, ar_gen_drop_fn drop) {{
    if (token == 0) return ar_gen_insert_raw(source, size, align, drop);
    return ar_gen_set_raw(token, source, size, align, drop) ? token : 0;
}}
static int ar_gen_remove_raw(uint64_t token, void *destination, size_t size, size_t align) {{
    ar_gen_entry **link = &ar_gen_head;
    while (*link && (*link)->token != token) link = &(*link)->next;
    ar_gen_entry *entry = *link;
    if (!entry || !destination || entry->size != size || entry->align != align) return 0;
    *link = entry->next;
    if (ar_gen_tail == entry) {{
        ar_gen_tail = NULL;
        for (ar_gen_entry *cursor = ar_gen_head; cursor; cursor = cursor->next) ar_gen_tail = cursor;
    }}
    if (size != 0) memcpy(destination, entry->data, size);
    free(entry->allocation); free(entry);
    return 1;
}}
static void ar_gen_shutdown_raw(void) {{
    ar_gen_entry *entries = ar_gen_head;
    ar_gen_head = NULL; ar_gen_tail = NULL;
    while (entries) {{
        ar_gen_entry *next = entries->next;
        if (entries->drop) entries->drop(entries->data);
        free(entries->allocation); free(entries);
        entries = next;
    }}
}}"#
        );
    }

    /// Raw buffer hosts for `std.alloc.vec` / `std.alloc.gen_arena` pure-buffer path.
    pub(super) fn emit_vec_buf_runtime(&mut self, uint_c_ty: &str) {
        let _ = writeln!(
            &mut self.output,
            r#"/* Pure-buffer alloc (Vec / GenArena thin) — mirrors JIT ar_vec_*. */
static void *ar_vec_malloc({uint_c_ty} size) {{
    if (size == 0) return NULL;
    void *p = malloc((size_t)size);
    return p;
}}
static void ar_vec_buf_free(void *p, {uint_c_ty} size) {{
    (void)size;
    free(p);
}}
static void *ar_vec_realloc(void *p, {uint_c_ty} old_size, {uint_c_ty} new_size) {{
    if (new_size == 0) {{ free(p); return NULL; }}
    void *q = realloc(p, (size_t)new_size);
    (void)old_size;
    return q;
}}
typedef struct {{ uint8_t *data; {uint_c_ty} len; {uint_c_ty} capacity; }} ArOwnedStringRuntime;
static bool ar_string_push_str(void *raw, const uint8_t *value_ptr, int64_t value_len) {{
    ArOwnedStringRuntime *s = (ArOwnedStringRuntime*)raw;
    if (!s || value_len < 0 || (value_len > 0 && !value_ptr)) return false;
    {uint_c_ty} n = ({uint_c_ty})value_len;
    if (n > UINT32_MAX || s->len > UINT32_MAX - n) return false;
    {uint_c_ty} required = s->len + n;
    if (required > s->capacity) {{
        {uint_c_ty} capacity = s->capacity < 8 ? 8 : s->capacity;
        while (capacity < required) {{
            capacity = capacity > UINT32_MAX / 2 ? UINT32_MAX : capacity * 2;
            if (capacity == UINT32_MAX && capacity < required) return false;
        }}
        uint8_t *replacement = (uint8_t*)ar_vec_realloc(s->data, s->capacity, capacity);
        if (!replacement) return false;
        s->data = replacement;
        s->capacity = capacity;
    }}
    if (n > 0) memcpy(s->data + s->len, value_ptr, (size_t)n);
    s->len = required;
    return true;
}}"#
        );
    }

    /// Host path helpers for PROMOTE-L4 (`std.path` join / file_name / is_absolute).
    pub(super) fn emit_path_runtime(&mut self, len_c_ty: &str) {
        let _ = writeln!(
            &mut self.output,
            r#"/* std.path host (Unix-oriented gold; mirrors JIT Path helpers). */
static {len_c_ty} ar_path_is_absolute(ArStr p) {{
    if (p.len <= 0 || !p.ptr) return 0;
    return p.ptr[0] == '/' ? 1 : 0;
}}
static {len_c_ty} ar_path_is_empty(ArStr p) {{
    return p.len <= 0 ? 1 : 0;
}}
static ArStr ar_path_join(ArStr a, ArStr b) {{
    /* Absolute b replaces (Unix Path::join). */
    if (b.len > 0 && b.ptr && b.ptr[0] == '/') return b;
    if (a.len <= 0) return b;
    if (b.len <= 0) return a;
    int need_sep = !(a.ptr[a.len - 1] == '/');
    {len_c_ty} total = a.len + b.len + (need_sep ? 1 : 0);
    uint8_t *buf = (uint8_t*)malloc((size_t)total + 1);
    if (!buf) abort();
    memcpy(buf, a.ptr, (size_t)a.len);
    {len_c_ty} off = a.len;
    if (need_sep) buf[off++] = '/';
    memcpy(buf + off, b.ptr, (size_t)b.len);
    buf[total] = 0;
    return ar_str_pack(buf, total);
}}
static bool ar_path_join_owned(ArStr a, ArStr b, uint8_t **out_buf, {len_c_ty} *out_len, {len_c_ty} *out_cap) {{
    if (!out_buf || !out_len || !out_cap || a.len < 0 || b.len < 0 ||
        (a.len > 0 && !a.ptr) || (b.len > 0 && !b.ptr)) return false;
    *out_buf = NULL; *out_len = 0; *out_cap = 0;
    bool absolute = b.len > 0 && b.ptr[0] == '/';
    {len_c_ty} al = absolute ? 0 : a.len;
    bool need_sep = al > 0 && b.len > 0 && a.ptr[al - 1] != '/';
    if (al > INT64_MAX - b.len - (need_sep ? 1 : 0)) return false;
    {len_c_ty} total = al + b.len + (need_sep ? 1 : 0);
    if (total == 0) return true;
    uint8_t *buf = (uint8_t*)ar_vec_malloc(({len_c_ty})total);
    if (!buf) return false;
    {len_c_ty} off = 0;
    if (al > 0) {{ memcpy(buf, a.ptr, (size_t)al); off = al; }}
    if (need_sep) buf[off++] = '/';
    if (b.len > 0) memcpy(buf + off, b.ptr, (size_t)b.len);
    *out_buf = buf; *out_len = total; *out_cap = total;
    return true;
}}
static ArStr ar_path_file_name(ArStr p) {{
    if (p.len <= 0 || !p.ptr) return ar_str_pack((const uint8_t*)"", 0);
    {len_c_ty} i = p.len;
    while (i > 0 && p.ptr[i - 1] != '/') i--;
    {len_c_ty} n = p.len - i;
    if (n <= 0) return ar_str_pack((const uint8_t*)"", 0);
    uint8_t *buf = (uint8_t*)malloc((size_t)n + 1);
    if (!buf) abort();
    memcpy(buf, p.ptr + i, (size_t)n);
    buf[n] = 0;
    return ar_str_pack(buf, n);
}}
/* std.core.str thin hosts */
static {len_c_ty} ar_str_len(ArStr s) {{
    return ({len_c_ty})(s.len < 0 ? 0 : s.len);
}}
static ArStr ar_str_concat(ArStr a, ArStr b) {{
    {len_c_ty} al = a.len < 0 ? 0 : a.len;
    {len_c_ty} bl = b.len < 0 ? 0 : b.len;
    {len_c_ty} total = al + bl;
    uint8_t *buf = (uint8_t*)malloc((size_t)total + 1);
    if (!buf) abort();
    if (al > 0 && a.ptr) memcpy(buf, a.ptr, (size_t)al);
    if (bl > 0 && b.ptr) memcpy(buf + al, b.ptr, (size_t)bl);
    buf[total] = 0;
    return ar_str_pack(buf, total);
}}
static {len_c_ty} ar_str_eq(ArStr a, ArStr b) {{
    if (a.len != b.len) return 0;
    if (a.len <= 0) return 1;
    if (!a.ptr || !b.ptr) return a.ptr == b.ptr ? 1 : 0;
    return memcmp(a.ptr, b.ptr, (size_t)a.len) == 0 ? 1 : 0;
}}
static {len_c_ty} ar_str_starts_with(ArStr s, ArStr p) {{
    if (p.len <= 0) return 1;
    if (s.len < p.len || !s.ptr || !p.ptr) return 0;
    return memcmp(s.ptr, p.ptr, (size_t)p.len) == 0 ? 1 : 0;
}}
static {len_c_ty} ar_str_ends_with(ArStr s, ArStr p) {{
    if (p.len <= 0) return 1;
    if (s.len < p.len || !s.ptr || !p.ptr) return 0;
    return memcmp(s.ptr + (s.len - p.len), p.ptr, (size_t)p.len) == 0 ? 1 : 0;
}}
static {len_c_ty} ar_str_contains(ArStr s, ArStr needle) {{
    if (needle.len <= 0) return 1;
    if (s.len < needle.len || !s.ptr || !needle.ptr) return 0;
    for (int64_t i = 0; i + needle.len <= s.len; i++) {{
        if (memcmp(s.ptr + i, needle.ptr, (size_t)needle.len) == 0) return 1;
    }}
    return 0;
}}
static {len_c_ty} ar_str_find(ArStr s, ArStr needle) {{
    if (needle.len <= 0) return 0;
    if (s.len < needle.len || !s.ptr || !needle.ptr) return -1;
    for (int64_t i = 0; i + needle.len <= s.len; i++) {{
        if (memcmp(s.ptr + i, needle.ptr, (size_t)needle.len) == 0) return i;
    }}
    return -1;
}}
static ArStr ar_str_split_last(ArStr s, ArStr sep) {{
    if (s.len <= 0 || !s.ptr) return ar_str_pack((const uint8_t*)"", 0);
    if (sep.len <= 0 || !sep.ptr) {{
        uint8_t *buf = (uint8_t*)malloc((size_t)s.len + 1);
        if (!buf) abort();
        memcpy(buf, s.ptr, (size_t)s.len);
        buf[s.len] = 0;
        return ar_str_pack(buf, s.len);
    }}
    {len_c_ty} last = -1;
    for ({len_c_ty} i = 0; i + sep.len <= s.len; i++) {{
        if (memcmp(s.ptr + i, sep.ptr, (size_t)sep.len) == 0) last = i;
    }}
    {len_c_ty} start = last < 0 ? 0 : last + sep.len;
    {len_c_ty} n = s.len - start;
    uint8_t *buf = (uint8_t*)malloc((size_t)n + 1);
    if (!buf) abort();
    if (n > 0) memcpy(buf, s.ptr + start, (size_t)n);
    buf[n] = 0;
    return ar_str_pack(buf, n);
}}"#
        );
    }

    pub(super) fn emit_co_poll_runtime(&mut self) {
        // Typed await in expr.rs inlines disc/payload loads for the real C type.
        // Keep i64 helpers only for host/test parity paths that still use them.
        let _ = writeln!(
            &mut self.output,
            r#"/* A3.6: disc 0=Ready payload@8; disc 1=PendingOnce then Ready.
 * Prefer typed inline await (no i64 cast). i64 helpers remain for MVP host tests. */
static int ar_co_poll_i64(uint8_t *state, int64_t *out) {{
    uint32_t disc = *(uint32_t*)state;
    if (disc == 0) {{ *out = *(int64_t*)(state + 8); return 0; }}
    if (disc == 1) {{ *(uint32_t*)state = 0; return 1; }}
    *out = *(int64_t*)(state + 8); return 0;
}}
static int64_t ar_co_block_on_i64(uint8_t *state) {{
    int64_t out = 0;
    for (;;) {{
        if (ar_co_poll_i64(state, &out) == 0) return out;
    }}
}}

/* Standard C99 Range and Coroutine helper functions */
static inline void** ar_make_range(intptr_t left, intptr_t right) {{
    void** r = (void**)malloc(sizeof(void*) * 2);
    if (!r) abort();
    r[0] = (void*)left;
    r[1] = (void*)right;
    return r;
}}

static inline void* ar_co_make_ready_heap(size_t size, void* val_ptr, size_t val_size) {{
    uint8_t* co = (uint8_t*)malloc(size);
    if (!co) abort();
    *(uint32_t*)co = 0;
    *(uint32_t*)(co + 4) = 0x4152434f;
    if (val_size > 0 && val_ptr) {{
        memcpy(co + 8, val_ptr, val_size);
    }}
    return (void*)co;
}}

static inline int64_t ar_co_await_i64(uint8_t* aw) {{
    for (;;) {{
        uint32_t d = *(uint32_t*)aw;
        if (d == 0) return *(int64_t*)(aw + 8);
        if (d == 1) {{ *(uint32_t*)aw = 0; continue; }}
        return *(int64_t*)(aw + 8);
    }}
}}

static inline double ar_co_await_f64(uint8_t* aw) {{
    for (;;) {{
        uint32_t d = *(uint32_t*)aw;
        if (d == 0) return *(double*)(aw + 8);
        if (d == 1) {{ *(uint32_t*)aw = 0; continue; }}
        return *(double*)(aw + 8);
    }}
}}

static inline void* ar_co_await_ptr(uint8_t* aw) {{
    for (;;) {{
        uint32_t d = *(uint32_t*)aw;
        if (d == 0) return *(void**)(aw + 8);
        if (d == 1) {{ *(uint32_t*)aw = 0; continue; }}
        return *(void**)(aw + 8);
    }}
}}"#
        );
    }

    /// SL_R.0 cooperative task table mirror (JIT `ar_rt_*`) for the C backend.
    ///
    /// Mirrors `arandu_runtime::rt_runtime`: a growable table of slots with
    /// Pending/Running/Completed states. A Pending blob is owned by the row;
    /// a join claims it, drives it, releases it, then caches the result. A
    /// cancel releases a Pending blob or marks a Running row for retirement.
    /// Cooperative only: the joining thread drives the blob; standalone C has
    /// no OS-worker scheduling at this surface.
    pub(super) fn emit_task_runtime(&mut self) {
        let _ = writeln!(
            &mut self.output,
            r#"/* SL_R.0 cooperative task table (mirrors JIT ar_rt_*). */
typedef struct {{
    int32_t state;          /* 0 EMPTY, 1 PENDING, 2 RUNNING, 3 COMPLETED */
    int32_t cancel_requested;
    uint8_t *blob;          /* owned while PENDING; RUNNING stores none */
    int64_t result;         /* COMPLETED cache */
}} ar_rt_slot;
static ar_rt_slot *ar_rt_slots = NULL;
static uint64_t ar_rt_capacity = 0;
static uint64_t ar_rt_len = 0;

static int64_t ar_rt_spawn_i64(uint8_t *state) {{
    if (!state) abort();
    uint64_t index = ar_rt_capacity;
    for (uint64_t i = 0; i < ar_rt_len; i++) {{
        if (ar_rt_slots[i].state == 0) {{ index = i; break; }}
    }}
    if (index == ar_rt_capacity) {{
        if (ar_rt_len == ar_rt_capacity) {{
            uint64_t new_cap = ar_rt_capacity == 0 ? 8 : ar_rt_capacity * 2;
            if (new_cap > (uint64_t)INT64_MAX) abort();
            ar_rt_slot *next = (ar_rt_slot*)realloc(
                ar_rt_slots, (size_t)(new_cap * sizeof(ar_rt_slot)));
            if (!next) abort();
            memset(next + ar_rt_capacity, 0,
                (size_t)((new_cap - ar_rt_capacity) * sizeof(ar_rt_slot)));
            ar_rt_slots = next;
            ar_rt_capacity = new_cap;
        }}
        index = ar_rt_len++;
    }}
    ar_rt_slots[index].state = 1;
    ar_rt_slots[index].cancel_requested = 0;
    ar_rt_slots[index].blob = state;
    return (int64_t)index;
}}

static int64_t ar_rt_join_i64(int64_t handle) {{
    if (handle < 0 || (uint64_t)handle >= ar_rt_len) abort();
    ar_rt_slot *slot = &ar_rt_slots[(uint64_t)handle];
    if (slot->state == 3) return slot->result;
    if (slot->state != 1) abort(); /* RUNNING = concurrent join; EMPTY = invalid */
    uint8_t *blob = slot->blob;
    slot->state = 2;
    slot->cancel_requested = 0;
    int64_t result = ar_co_block_on_i64(blob);
    if (*(uint32_t*)(blob + 4) != 0x4152434fu) abort(); /* fail-closed magic */
    free(blob);
    if (slot->cancel_requested) {{
        slot->state = 0;
        slot->blob = NULL;
    }} else {{
        slot->state = 3;
        slot->result = result;
    }}
    return result;
}}

static void ar_rt_cancel_i64(int64_t handle) {{
    if (handle < 0 || (uint64_t)handle >= ar_rt_len) return;
    ar_rt_slot *slot = &ar_rt_slots[(uint64_t)handle];
    if (slot->state == 1) {{
        uint8_t *blob = slot->blob;
        slot->state = 0;
        slot->blob = NULL;
        if (*(uint32_t*)(blob + 4) != 0x4152434fu) abort(); /* fail-closed magic */
        free(blob);
    }} else if (slot->state == 2) {{
        slot->cancel_requested = 1;
    }} else if (slot->state == 3) {{
        slot->state = 0; /* cached row is retired by cancel (JIT slot.take) */
    }} /* EMPTY: no-op */
}}

static int64_t ar_rt_block_on_i64(uint8_t *state) {{
    return ar_co_block_on_i64(state);
}}

static int32_t ar_rt_parallel_fold_run(
    uint64_t num_chunks,
    uint8_t **contexts,
    int32_t (*thunk)(uint8_t*, uint8_t*),
    uint8_t **results,
    uint64_t workers,
    int64_t *stop_flag
) {{
    if (num_chunks == 0) return 0;
    for (uint64_t i = 0; i < num_chunks; i++) {{
        if (stop_flag && *stop_flag != 0) return 2;
        int32_t code = thunk(contexts[i], results[i]);
        if (code != 0) {{
            if (stop_flag) *stop_flag = 1;
            return code;
        }}
    }}
    return 0;
}}"#
        );
    }

    /// Host filesystem helpers for `std.fs` (mirrors JIT `ar_fs_*`).
    pub(super) fn emit_fs_runtime(&mut self, uint_c_ty: &str, int_c_ty: &str) {
        let _ = writeln!(
            &mut self.output,
            r#"/* std.fs host helpers (mirrors JIT ar_fs_* runtime). */
static inline char *ar_str_to_c_path(ArStr s, char *stack_buf, size_t stack_sz) {{
    if (s.len < 0 || (!s.ptr && s.len > 0)) return NULL;
    size_t len = (size_t)s.len;
    if (len + 1 <= stack_sz) {{
        if (len > 0 && s.ptr) memcpy(stack_buf, s.ptr, len);
        stack_buf[len] = '\0';
        return stack_buf;
    }}
    char *heap = (char *)malloc(len + 1);
    if (!heap) return NULL;
    if (len > 0 && s.ptr) memcpy(heap, s.ptr, len);
    heap[len] = '\0';
    return heap;
}}

static inline {int_c_ty} ar_map_errno(int err) {{
    switch (err) {{
        case 0: return 0;
        case ENOENT: return 1; /* NotFound */
        case EACCES:
        case EPERM: return 2; /* PermissionDenied */
        case EEXIST: return 3; /* AlreadyExists */
#ifdef EAGAIN
        case EAGAIN: return 4; /* WouldBlock */
#endif
#if defined(EWOULDBLOCK) && (!defined(EAGAIN) || EAGAIN != EWOULDBLOCK)
        case EWOULDBLOCK: return 4; /* WouldBlock */
#endif
        case EINVAL: return 5; /* InvalidInput */
#ifdef EISDIR
        case EISDIR: return 6; /* IsADirectory */
#endif
#ifdef ENOTDIR
        case ENOTDIR: return 7; /* NotADirectory */
#endif
        default: return 8; /* Other */
    }}
}}

static bool exists(ArStr path) {{
    if (path.len <= 0 || !path.ptr) return false;
    char stack_buf[1024];
    char *c_path = ar_str_to_c_path(path, stack_buf, sizeof(stack_buf));
    if (!c_path) return false;
    struct stat st;
    int res = stat(c_path, &st);
    if (c_path != stack_buf) free(c_path);
    return res == 0;
}}

static {int_c_ty} ar_fs_open(ArStr path, {int_c_ty} flags, {int_c_ty} mode) {{
    (void)path; (void)flags; (void)mode;
    return -1;
}}

static {int_c_ty} ar_fs_read({int_c_ty} fd, uint8_t *buf, {uint_c_ty} count) {{
    (void)fd; (void)buf; (void)count;
    return -1;
}}

static {int_c_ty} ar_fs_write({int_c_ty} fd, const uint8_t *buf, {uint_c_ty} count) {{
    (void)fd; (void)buf; (void)count;
    return -1;
}}

static {int_c_ty} ar_fs_close({int_c_ty} fd) {{
    (void)fd;
    return 0;
}}

static void ar_fs_read_all(ArStr path, uint8_t **out_buf, {uint_c_ty} *out_len, {uint_c_ty} *out_cap, {int_c_ty} *err) {{
    if (!out_buf || !out_len || !out_cap || !err) return;
    if (path.len < 0 || (path.len > 0 && !path.ptr)) {{
        *out_buf = NULL; *out_len = 0; *out_cap = 0; *err = 5;
        return;
    }}
    char stack_buf[1024];
    char *c_path = ar_str_to_c_path(path, stack_buf, sizeof(stack_buf));
    if (!c_path) {{
        *out_buf = NULL; *out_len = 0; *out_cap = 0; *err = 8;
        return;
    }}
    struct stat st;
    if (stat(c_path, &st) != 0) {{
        int e = errno;
        if (c_path != stack_buf) free(c_path);
        *out_buf = NULL; *out_len = 0; *out_cap = 0; *err = ar_map_errno(e);
        return;
    }}
    if (S_ISDIR(st.st_mode)) {{
        if (c_path != stack_buf) free(c_path);
        *out_buf = NULL; *out_len = 0; *out_cap = 0; *err = 6;
        return;
    }}
    FILE *f = fopen(c_path, "rb");
    if (!f) {{
        int e = errno;
        if (c_path != stack_buf) free(c_path);
        *out_buf = NULL; *out_len = 0; *out_cap = 0; *err = ar_map_errno(e);
        return;
    }}
    if (c_path != stack_buf) free(c_path);
    size_t initial_cap = (st.st_size > 0 && (uint64_t)st.st_size <= 268435456ULL) ? (size_t)st.st_size : 0;
    if (initial_cap == 0) {{
        size_t cap = 1024;
        size_t len = 0;
        uint8_t *buf = (uint8_t *)ar_vec_malloc(({uint_c_ty})cap);
        if (!buf) {{ fclose(f); *out_buf = NULL; *out_len = 0; *out_cap = 0; *err = 8; return; }}
        for (;;) {{
            if (len == cap) {{
                size_t new_cap = cap * 2;
                if (new_cap > 268435456ULL) {{ ar_vec_buf_free(buf, ({uint_c_ty})cap); fclose(f); *out_buf = NULL; *out_len = 0; *out_cap = 0; *err = 8; return; }}
                uint8_t *next = (uint8_t *)ar_vec_realloc(buf, ({uint_c_ty})cap, ({uint_c_ty})new_cap);
                if (!next) {{ ar_vec_buf_free(buf, ({uint_c_ty})cap); fclose(f); *out_buf = NULL; *out_len = 0; *out_cap = 0; *err = 8; return; }}
                buf = next;
                cap = new_cap;
            }}
            size_t n = fread(buf + len, 1, cap - len, f);
            if (n == 0) break;
            len += n;
        }}
        fclose(f);
        if (len == 0) {{
            ar_vec_buf_free(buf, ({uint_c_ty})cap);
            *out_buf = NULL; *out_len = 0; *out_cap = 0; *err = 0;
            return;
        }}
        uint8_t *shrunk = (uint8_t *)ar_vec_realloc(buf, ({uint_c_ty})cap, ({uint_c_ty})len);
        *out_buf = shrunk ? shrunk : buf;
        *out_len = ({uint_c_ty})len;
        *out_cap = shrunk ? ({uint_c_ty})len : ({uint_c_ty})cap;
        *err = 0;
        return;
    }}
    uint8_t *buf = (uint8_t *)ar_vec_malloc(({uint_c_ty})initial_cap);
    if (!buf) {{
        fclose(f);
        *out_buf = NULL; *out_len = 0; *out_cap = 0; *err = 8;
        return;
    }}
    size_t read_bytes = fread(buf, 1, initial_cap, f);
    fclose(f);
    *out_buf = buf;
    *out_len = ({uint_c_ty})read_bytes;
    *out_cap = ({uint_c_ty})initial_cap;
    *err = 0;
}}

typedef struct {{
    char *name;
    size_t len;
    uint8_t is_dir;
}} ArHostDirEntry;

static void ar_fs_readdir(ArStr path, uint8_t **out_buf, {uint_c_ty} *out_count, {uint_c_ty} *out_cap, {int_c_ty} *err) {{
    if (!out_buf || !out_count || !out_cap || !err) return;
    if (path.len < 0 || (path.len > 0 && !path.ptr)) {{
        *out_buf = NULL; *out_count = 0; *out_cap = 0; *err = 5;
        return;
    }}
    char stack_buf[1024];
    char *c_path = ar_str_to_c_path(path, stack_buf, sizeof(stack_buf));
    if (!c_path) {{
        *out_buf = NULL; *out_count = 0; *out_cap = 0; *err = 8;
        return;
    }}
#if !defined(_WIN32) || defined(__MINGW32__)
    DIR *d = opendir(c_path);
    if (!d) {{
        int e = errno;
        if (c_path != stack_buf) free(c_path);
        *out_buf = NULL; *out_count = 0; *out_cap = 0; *err = ar_map_errno(e);
        return;
    }}
    size_t entries_cap = 16;
    size_t count = 0;
    ArHostDirEntry *entries = (ArHostDirEntry *)malloc(entries_cap * sizeof(ArHostDirEntry));
    if (!entries) {{
        closedir(d);
        if (c_path != stack_buf) free(c_path);
        *out_buf = NULL; *out_count = 0; *out_cap = 0; *err = 8;
        return;
    }}
    size_t names_total_len = 0;
    struct dirent *ent;
    while ((ent = readdir(d)) != NULL) {{
        if (strcmp(ent->d_name, ".") == 0 || strcmp(ent->d_name, "..") == 0) continue;
        size_t nlen = strlen(ent->d_name);
        uint8_t is_dir = 0;
#ifdef DT_DIR
        if (ent->d_type == DT_DIR) is_dir = 1;
        else if (ent->d_type == DT_UNKNOWN) {{
            char sub_path[4096];
            snprintf(sub_path, sizeof(sub_path), "%s/%s", c_path, ent->d_name);
            struct stat st;
            if (stat(sub_path, &st) == 0 && S_ISDIR(st.st_mode)) is_dir = 1;
        }}
#else
        char sub_path[4096];
        snprintf(sub_path, sizeof(sub_path), "%s/%s", c_path, ent->d_name);
        struct stat st;
        if (stat(sub_path, &st) == 0 && S_ISDIR(st.st_mode)) is_dir = 1;
#endif
        if (count == entries_cap) {{
            size_t new_cap = entries_cap * 2;
            ArHostDirEntry *next = (ArHostDirEntry *)realloc(entries, new_cap * sizeof(ArHostDirEntry));
            if (!next) {{
                for (size_t j = 0; j < count; j++) free(entries[j].name);
                free(entries);
                closedir(d);
                if (c_path != stack_buf) free(c_path);
                *out_buf = NULL; *out_count = 0; *out_cap = 0; *err = 8;
                return;
            }}
            entries = next;
            entries_cap = new_cap;
        }}
        char *copy = (char *)malloc(nlen + 1);
        if (!copy) {{
            for (size_t j = 0; j < count; j++) free(entries[j].name);
            free(entries);
            closedir(d);
            if (c_path != stack_buf) free(c_path);
            *out_buf = NULL; *out_count = 0; *out_cap = 0; *err = 8;
            return;
        }}
        memcpy(copy, ent->d_name, nlen);
        copy[nlen] = '\0';
        entries[count].name = copy;
        entries[count].len = nlen;
        entries[count].is_dir = is_dir;
        count++;
        names_total_len += nlen;
    }}
    closedir(d);
    if (c_path != stack_buf) free(c_path);

    if (count == 0) {{
        free(entries);
        *out_buf = NULL; *out_count = 0; *out_cap = 0; *err = 0;
        return;
    }}

    size_t blob_len = 8 + count * 16 + names_total_len;
    uint8_t *blob = (uint8_t *)ar_vec_malloc(({uint_c_ty})blob_len);
    if (!blob) {{
        for (size_t j = 0; j < count; j++) free(entries[j].name);
        free(entries);
        *out_buf = NULL; *out_count = 0; *out_cap = 0; *err = 8;
        return;
    }}
    *(uint32_t *)blob = (uint32_t)count;
    *(uint32_t *)(blob + 4) = (uint32_t)blob_len;
    size_t names_base = 8 + count * 16;
    size_t cursor = names_base;
    for (size_t i = 0; i < count; i++) {{
        size_t base = 8 + i * 16;
        size_t off = cursor - names_base;
        *(uint32_t *)(blob + base) = (uint32_t)off;
        *(uint32_t *)(blob + base + 4) = (uint32_t)entries[i].len;
        blob[base + 8] = entries[i].is_dir;
        memset(blob + base + 9, 0, 7);
        if (entries[i].len > 0) {{
            memcpy(blob + cursor, entries[i].name, entries[i].len);
            cursor += entries[i].len;
        }}
        free(entries[i].name);
    }}
    free(entries);
    *out_buf = blob;
    *out_count = ({uint_c_ty})count;
    *out_cap = ({uint_c_ty})blob_len;
    *err = 0;
#else
    if (c_path != stack_buf) free(c_path);
    *out_buf = NULL; *out_count = 0; *out_cap = 0; *err = 8;
#endif
}}"#
        );
    }

    /// Host environment helpers for `std.env` (mirrors JIT `ar_env_*`).
    pub(super) fn emit_env_runtime(&mut self, int_c_ty: &str) {
        let _ = writeln!(
            &mut self.output,
            r#"/* std.env host helpers (mirrors JIT ar_env_* runtime). */
static int ar_env_c_argc = 0;
static char **ar_env_c_argv = NULL;

#if defined(__GNUC__) || defined(__clang__)
__attribute__((constructor)) static void ar_capture_env_args(int argc, char **argv) {{
    ar_env_c_argc = argc;
    ar_env_c_argv = argv;
}}
#endif

static {int_c_ty} ar_env_args_len(void) {{
    return ({int_c_ty})ar_env_c_argc;
}}

static ArStr ar_env_arg({int_c_ty} index) {{
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

    pub(super) fn emit_headers(&mut self, needs_str: bool) {
        let _ = writeln!(&mut self.output, "#include <stdint.h>");
        let _ = writeln!(&mut self.output, "#include <stdbool.h>");
        let _ = writeln!(&mut self.output, "#include <stdlib.h>");
        let _ = writeln!(&mut self.output, "#include <string.h>");
        let _ = writeln!(&mut self.output, "#include <stdio.h>");
        let _ = writeln!(&mut self.output, "#include <errno.h>");
        let _ = writeln!(&mut self.output, "#include <sys/types.h>");
        let _ = writeln!(&mut self.output, "#include <sys/stat.h>");
        let _ = writeln!(
            &mut self.output,
            "#if !defined(_WIN32) || defined(__MINGW32__)\n#include <dirent.h>\n#endif"
        );
        let _ = writeln!(
            &mut self.output,
            "#if defined(__GNUC__) || defined(__clang__)\n#define AR_MAY_ALIAS __attribute__((__may_alias__))\n#else\n#define AR_MAY_ALIAS\n#endif"
        );
        if needs_str {
            let _ = writeln!(&mut self.output, "#include <stdarg.h>");
            let _ = writeln!(&mut self.output, "#include <math.h>");
        }
        let _ = writeln!(&mut self.output, "#ifndef AR_UNREACHABLE");
        let _ = writeln!(&mut self.output, "#define AR_UNREACHABLE() abort()");
        let _ = writeln!(&mut self.output, "#endif");
        let _ = writeln!(
            &mut self.output,
            "#if defined(__GNUC__) || defined(__clang__)\n#define AR_BENCH_NOINLINE __attribute__((noinline))\n#elif defined(_MSC_VER)\n#define AR_BENCH_NOINLINE __declspec(noinline)\n#else\n#define AR_BENCH_NOINLINE\n#endif"
        );
        let _ = writeln!(
            &mut self.output,
            "static AR_BENCH_NOINLINE int64_t ar_bench_black_box_i64(int64_t value) {{ volatile int64_t opaque = value; return opaque; }}"
        );
        let _ = writeln!(
            &mut self.output,
            "static AR_BENCH_NOINLINE double ar_bench_black_box_f64(double value) {{ volatile double opaque = value; return opaque; }}"
        );
        let _ = writeln!(
            &mut self.output,
            "static AR_BENCH_NOINLINE void *ar_bench_black_box_ptr(void *value) {{ void * volatile opaque = value; return opaque; }}"
        );
        // `ArStr` is also part of the owned-string runtime ABI.  Keep the
        // two-word descriptor available even when the user program contains
        // no `str` value: `String.pushStr` is emitted with this ABI and the
        // allocation runtime below is unconditional.
        let len_c_ty = if self.layout.pointer_width() == 4 {
            "int32_t"
        } else {
            "int64_t"
        };
        let _ = writeln!(
            &mut self.output,
            "typedef struct {{ const uint8_t *ptr; {len_c_ty} len; }} ArStr;"
        );
        self.emitted_types.insert("ArStr".to_string());
        // Runtime helpers for fat-pointer strings (string interpolation & hosts).
        let _ = writeln!(
            &mut self.output,
            "static inline void ar_str_unpack(ArStr s, const uint8_t **ptr, {len_c_ty} *len) {{"
        );
        let _ = writeln!(&mut self.output, "    *ptr = s.ptr;");
        let _ = writeln!(&mut self.output, "    *len = s.len;");
        let _ = writeln!(&mut self.output, "}}");
        let _ = writeln!(
            &mut self.output,
            "static inline ArStr ar_str_pack(const uint8_t *ptr, {len_c_ty} len) {{"
        );
        let _ = writeln!(
            &mut self.output,
            "    return (ArStr){{ .ptr = ptr, .len = len }};"
        );
        let _ = writeln!(&mut self.output, "}}");
        // F2.3.runtime: process-lifetime gen arena (i64 payload MVP; mirrors JIT host).
        self.emit_gen_arena_runtime();
        // Pure-buffer host used by std.alloc.vec / gen_arena product surface.
        let uint_c_ty = if self.layout.pointer_width() == 4 {
            "uint32_t"
        } else {
            "uint64_t"
        };
        self.emit_vec_buf_runtime(uint_c_ty);
        // A3.6: poll / block_on for coroutine state blobs (disc@0, payload@8).
        self.emit_co_poll_runtime();
        // SL_R.0: cooperative task table mirror (SyncExecutor host surface).
        self.emit_task_runtime();
        // std.fs / std.env runtime hosts.
        self.emit_fs_runtime(uint_c_ty, len_c_ty);
        self.emit_env_runtime(len_c_ty);
        let _ = writeln!(&mut self.output);
        if !needs_str {
            return;
        }
        // PROMOTE-L4 path hosts (need ArStr + ar_str_pack).
        self.emit_path_runtime(len_c_ty);
        let _ = writeln!(
            &mut self.output,
            "static ArStr ar_str_concat_n(int n, ...) {{"
        );
        let _ = writeln!(
            &mut self.output,
            "    if (n <= 0) return ar_str_pack((const uint8_t*)\"\", 0);"
        );
        let _ = writeln!(&mut self.output, "    va_list ap;");
        let _ = writeln!(&mut self.output, "    va_start(ap, n);");
        let _ = writeln!(
            &mut self.output,
            "    ArStr *parts = (ArStr*)malloc((size_t)n * sizeof(ArStr));"
        );
        let _ = writeln!(
            &mut self.output,
            "    if (!parts) {{ va_end(ap); abort(); }}"
        );
        let _ = writeln!(&mut self.output, "    {len_c_ty} total = 0;");
        let _ = writeln!(&mut self.output, "    for (int i = 0; i < n; i++) {{");
        let _ = writeln!(&mut self.output, "        parts[i] = va_arg(ap, ArStr);");
        let _ = writeln!(&mut self.output, "        const uint8_t *p; {len_c_ty} l;");
        let _ = writeln!(&mut self.output, "        ar_str_unpack(parts[i], &p, &l);");
        let _ = writeln!(&mut self.output, "        if (l > 0) total += l;");
        let _ = writeln!(&mut self.output, "    }}");
        let _ = writeln!(&mut self.output, "    va_end(ap);");
        let _ = writeln!(
            &mut self.output,
            "    uint8_t *buf = (uint8_t*)malloc((size_t)total + 1);"
        );
        let _ = writeln!(
            &mut self.output,
            "    if (!buf) {{ free(parts); abort(); }}"
        );
        let _ = writeln!(&mut self.output, "    {len_c_ty} off = 0;");
        let _ = writeln!(&mut self.output, "    for (int i = 0; i < n; i++) {{");
        let _ = writeln!(&mut self.output, "        const uint8_t *p; {len_c_ty} l;");
        let _ = writeln!(&mut self.output, "        ar_str_unpack(parts[i], &p, &l);");
        let _ = writeln!(
            &mut self.output,
            "        if (l > 0 && p) {{ memcpy(buf + off, p, (size_t)l); off += l; }}"
        );
        let _ = writeln!(&mut self.output, "    }}");
        let _ = writeln!(&mut self.output, "    buf[total] = 0;");
        let _ = writeln!(&mut self.output, "    free(parts);");
        let _ = writeln!(&mut self.output, "    return ar_str_pack(buf, total);");
        let _ = writeln!(&mut self.output, "}}");
        // ToStr v0.1 helpers (malloc + snprintf; process-lifetime leak OK for debug).
        let _ = writeln!(&mut self.output, "static ArStr ar_i64_to_str(int64_t v) {{");
        let _ = writeln!(&mut self.output, "    char tmp[32];");
        let _ = writeln!(
            &mut self.output,
            "    int n = snprintf(tmp, sizeof(tmp), \"%lld\", (long long)v);"
        );
        let _ = writeln!(&mut self.output, "    if (n < 0) abort();");
        let _ = writeln!(
            &mut self.output,
            "    uint8_t *buf = (uint8_t*)malloc((size_t)n + 1);"
        );
        let _ = writeln!(&mut self.output, "    if (!buf) abort();");
        let _ = writeln!(&mut self.output, "    memcpy(buf, tmp, (size_t)n);");
        let _ = writeln!(&mut self.output, "    buf[n] = 0;");
        let _ = writeln!(
            &mut self.output,
            "    return ar_str_pack(buf, ({len_c_ty})n);"
        );
        let _ = writeln!(&mut self.output, "}}");
        let _ = writeln!(
            &mut self.output,
            "static ArStr ar_u64_to_str(uint64_t v) {{"
        );
        let _ = writeln!(&mut self.output, "    char tmp[32];");
        let _ = writeln!(
            &mut self.output,
            "    int n = snprintf(tmp, sizeof(tmp), \"%llu\", (unsigned long long)v);"
        );
        let _ = writeln!(&mut self.output, "    if (n < 0) abort();");
        let _ = writeln!(
            &mut self.output,
            "    uint8_t *buf = (uint8_t*)malloc((size_t)n + 1);"
        );
        let _ = writeln!(&mut self.output, "    if (!buf) abort();");
        let _ = writeln!(&mut self.output, "    memcpy(buf, tmp, (size_t)n);");
        let _ = writeln!(&mut self.output, "    buf[n] = 0;");
        let _ = writeln!(
            &mut self.output,
            "    return ar_str_pack(buf, ({len_c_ty})n);"
        );
        let _ = writeln!(&mut self.output, "}}");
        // Keep in sync with arandu_runtime::to_str_runtime::format_f64_v01
        // (specials + integer-looking values + %.15g for the rest).
        let _ = writeln!(&mut self.output, "static ArStr ar_f64_to_str(double v) {{");
        let _ = writeln!(&mut self.output, "    char tmp[64];");
        let _ = writeln!(&mut self.output, "    int n;");
        let _ = writeln!(
            &mut self.output,
            "    if (isnan(v)) {{ n = snprintf(tmp, sizeof(tmp), \"nan\"); }}"
        );
        let _ = writeln!(
            &mut self.output,
            "    else if (isinf(v)) {{ n = snprintf(tmp, sizeof(tmp), \"%s\", (v < 0) ? \"-inf\" : \"inf\"); }}"
        );
        let _ = writeln!(
            &mut self.output,
            "    else if (v == (double)(long long)v && v < 1e15 && v > -1e15) {{ n = snprintf(tmp, sizeof(tmp), \"%lld\", (long long)v); }}"
        );
        let _ = writeln!(
            &mut self.output,
            "    else {{ n = snprintf(tmp, sizeof(tmp), \"%.15g\", v); }}"
        );
        let _ = writeln!(&mut self.output, "    if (n < 0) abort();");
        let _ = writeln!(
            &mut self.output,
            "    uint8_t *buf = (uint8_t*)malloc((size_t)n + 1);"
        );
        let _ = writeln!(&mut self.output, "    if (!buf) abort();");
        let _ = writeln!(&mut self.output, "    memcpy(buf, tmp, (size_t)n);");
        let _ = writeln!(&mut self.output, "    buf[n] = 0;");
        let _ = writeln!(
            &mut self.output,
            "    return ar_str_pack(buf, ({len_c_ty})n);"
        );
        let _ = writeln!(&mut self.output, "}}");
        let _ = writeln!(&mut self.output, "static ArStr ar_bool_to_str(bool v) {{");
        let _ = writeln!(
            &mut self.output,
            "    const char *s = v ? \"true\" : \"false\";"
        );
        let _ = writeln!(&mut self.output, "    {len_c_ty} n = v ? 4 : 5;");
        let _ = writeln!(
            &mut self.output,
            "    uint8_t *buf = (uint8_t*)malloc((size_t)n + 1);"
        );
        let _ = writeln!(&mut self.output, "    if (!buf) abort();");
        let _ = writeln!(&mut self.output, "    memcpy(buf, s, (size_t)n);");
        let _ = writeln!(&mut self.output, "    buf[n] = 0;");
        let _ = writeln!(&mut self.output, "    return ar_str_pack(buf, n);");
        let _ = writeln!(&mut self.output, "}}");
        let _ = writeln!(
            &mut self.output,
            "static ArStr ar_char_to_str(uint32_t cp) {{"
        );
        let _ = writeln!(&mut self.output, "    uint8_t tmp[4];");
        let _ = writeln!(&mut self.output, "    int n = 0;");
        let _ = writeln!(
            &mut self.output,
            "    if (cp <= 0x7F) {{ tmp[0] = (uint8_t)cp; n = 1; }}"
        );
        let _ = writeln!(
            &mut self.output,
            "    else if (cp <= 0x7FF) {{ tmp[0] = (uint8_t)(0xC0 | (cp >> 6)); tmp[1] = (uint8_t)(0x80 | (cp & 0x3F)); n = 2; }}"
        );
        let _ = writeln!(
            &mut self.output,
            "    else if (cp <= 0xFFFF) {{ tmp[0] = (uint8_t)(0xE0 | (cp >> 12)); tmp[1] = (uint8_t)(0x80 | ((cp >> 6) & 0x3F)); tmp[2] = (uint8_t)(0x80 | (cp & 0x3F)); n = 3; }}"
        );
        let _ = writeln!(
            &mut self.output,
            "    else {{ tmp[0] = (uint8_t)(0xF0 | (cp >> 18)); tmp[1] = (uint8_t)(0x80 | ((cp >> 12) & 0x3F)); tmp[2] = (uint8_t)(0x80 | ((cp >> 6) & 0x3F)); tmp[3] = (uint8_t)(0x80 | (cp & 0x3F)); n = 4; }}"
        );
        let _ = writeln!(
            &mut self.output,
            "    uint8_t *buf = (uint8_t*)malloc((size_t)n + 1);"
        );
        let _ = writeln!(&mut self.output, "    if (!buf) abort();");
        let _ = writeln!(&mut self.output, "    memcpy(buf, tmp, (size_t)n);");
        let _ = writeln!(&mut self.output, "    buf[n] = 0;");
        let _ = writeln!(
            &mut self.output,
            "    return ar_str_pack(buf, ({len_c_ty})n);"
        );
        let _ = writeln!(&mut self.output, "}}");
        let _ = writeln!(&mut self.output);
    }
}
