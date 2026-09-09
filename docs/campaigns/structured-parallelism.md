# Structured parallel processing — active design

Status: implementation campaign started; no parallel language API is shipped.
The execution queue lives only in the [master roadmap](../arandu-compiler-roadmap-v0.1.md).
This document records design constraints and evidence, not an implemented contract.

## First delivery

Pypor consumes a general, bounded batch-processing and reduction operation. It
supplies discovery, counting, error policy and combination of statistics. The
standard library/runtime owns scheduling, admission and structured completion.
A disjoint-array workload must exercise the same foundation without filesystem
knowledge. Public syntax and callable representation remain to be selected.

The first delivery does not require an async socket reactor or detached tasks.
Blocking file work must have explicit bounded execution; it must not silently
occupy a cooperative async executor. No task may outlive its scope. Cancellation
requests stopping and stops admission; it cannot forcibly reclaim resources
still used by running callbacks. Process abort is not recoverable cancellation.

## Evidence from the current implementation

- `stdlib/std/runtime/executor.aru` exposes a cooperative `SyncExecutor`.
  At campaign start its handle erased the result type; `TaskHandle<T>` now
  preserves it through spawn/join/cancel without adding runtime fields.
  `join<T>` still bridges an i64 host result to T. This is not an ABI for
  arbitrary job results or a proof of thread transfer safety.
- `crates/arandu_runtime/src/rt_runtime.rs` parks coroutine state at spawn and
  drives it during join. Adding OS workers to that table would not establish
  compiler-checked transfer, scoped borrowing or generic result layout.
- The canonical `std.core.marker.Send` and `Sync` bounds now inspect owned
  storage in the pure type checker. Typed `LangItem` identities distinguish
  them from user interfaces with the same names and survive import aliases.
  Scalars and aggregates of eligible storage pass; views, raw pointers,
  coroutine state and resources with destructors are conservatively rejected.
  Generic arguments participate even in empty `PhantomData<T>` wrappers.
  The cooperative `TaskHandle<T>` has an explicit negative contract identified
  by a lang item, preserving its existing layout. Its integer ID is not proof
  of safe transfer between workers.
- Type checking currently rejects indirect calls with T033. Regression coverage
  is in `crates/arandu_query/tests/indirect_call.rs`. A callback API needs a
  coherent callable contract through typing, AMIR and both backends first.
  `job.callback(...)` currently takes method resolution, whereas
  `(job.callback)(...)` reaches T033 for a function-valued field. Resolve that
  language distinction deliberately before choosing callback surface syntax.

## Sequential callable evidence

The generic-job probe exposed an existing type-checking bug: forwarding a
parameter constrained by `Job<bool>` into a callee requiring `Job<int>` was
accepted. Bound checking compared only the interface symbol. It now compares
the complete instantiated bound, substituting callee parameters first; regression
cases cover mismatched/equal nested results, dependent parameters and `where`.
Inferred calls also skipped constraint validation entirely. Inference now calls
the same validator after resolving type arguments; explicit/inferred free calls,
method calls and imported module calls have positive and negative regressions.
This repair is in the pure type checker and does not add thread-transfer safety.

`cli_structured_job` executes an interface-constrained generic job with explicit
context and a three-integer result under Cranelift JIT, with and without AMIR
optimization. This establishes a static-dispatch candidate, not thread safety.
An equivalent probe emitted as host C compiled with the local C compiler and
returned the expected value on Linux. Native C/backend matrix coverage and
borrowed/owned resource cases remain required before choosing the public API.

The executor probe additionally exposed a C representation mismatch: coroutine
values were formatted as byte structs despite runtime constructors returning
state pointers. The C formatter now uses the pointer ABI; a compiled C/JIT
regression returns 42 through the shared `ar_co_block_on_i64` primitive.
Standalone C still lacks the cooperative queue host surface (`ar_rt_spawn_i64`,
`ar_rt_join_i64`, `ar_rt_cancel_i64` and the block-on alias). This is a distinct
backend-parity requirement, not supplied by fixing the coroutine value type.

The owned job/result probe found three existing ownership faults that also
affected sequential code. A nominal type with an explicit destructor could be
classified as bitwise-copyable when all its fields were scalar, duplicating its
cleanup obligation. Passing a scalar field from that value was also rejected as
if it moved the field. Copy classification now treats the destructor itself as
an ownership obligation, while call lowering permits reads of fields whose
concrete type is copyable and continues to reject partial moves.

Drop elaboration and both backends also resolved a generic field from its
declaration type (`T`) instead of its instantiated owner
(`Envelope<Resource>`). The shared layout helper now performs that substitution
once for backend consumers, and drop elaboration substitutes concrete fields
before deciding and emitting cleanup. Regressions move an owned job context,
return a three-field aggregate containing a resource, verify every field, and
observe exactly-once cleanup before and after AMIR optimization. The CLI covers
generic static dispatch through the complete pipeline; C emission covers the
concrete backend ownership/layout contract because the low-level parity helper
does not run package-query monomorphization.

## Ownership and phase boundaries

Type checking establishes transfer/sharing eligibility and callable signatures;
ownership/dataflow establishes moves, borrow validity and disjoint access.
AMIR retains these operations and their effects through optimization. Backends
lower arguments, context, results and destruction using target layout. Runtime
owns workers and queues; the standard library provides typed operations.
Salsa remains exclusively in `arandu_query`, with existing item-level cutoff.

First evaluate statically resolved functions or typed job implementations with
explicit context. Do not add string callback lookup, a Pypor intrinsic, or an
unchecked integer/pointer conversion to bypass missing callable support.
Closures must use the language's canonical representation if required.

Transfer rules must account for nested fields, shared mutable state, raw
pointers, FFI and thread-affine resources. A borrowed view remains tied to its
owner; copying the view does not extend the owner's lifetime. Cleanup follows
the existing memory model, including partial initialization and failure paths.

## Execution and allocation decisions

Compare a bounded scheduler with an established work-stealing implementation
before selecting dependencies. A new lock-free queue is not a prerequisite.
One worker and nested work must make progress without spawning recursive pools.
The C backend currently has standalone C host mirrors; a Rust scheduler cannot
silently become its new linkage requirement. Select and document backend parity
before exposing the operation as portable.

Bound pending jobs and library-owned buffers separately. Callback allocations
are outside that budget unless explicitly supplied through a budgeted facility.
Do not retain one result per input merely to reduce it. Use exclusive partial
accumulators; define ordering, associativity requirements and overflow behavior.
Failure selection by completion order is not deterministic input-order failure
selection. The API must state which policy applies, including cancellation.

Prefer moving existing path storage and borrowing stable batch storage. A path
copy is justified when storage otherwise expires, with its bytes charged to the
batch. Reuse read buffers where measured beneficial. Bound chunk size for large
files rather than assuming file size is bounded. Task records use typed identities;
reused externally observable handles require stale-handle protection.

Each allocation/copy decision records owner, lifetime, upper bound and why a
borrow/move is insufficient. SoA, arenas, atomics, padding and shared ownership
require evidence about access patterns; none are blanket requirements.

## Acceptance evidence

Initial Linux kernel measurements are preserved in
[pypor-sequential-baseline.json](./pypor-sequential-baseline.json), including
binary hash, revisions, raw output and measurement method. All three runs
returned identical counts: 65,370 files and 37,900,970 physical lines. Wall
times were 55.03, 23.71 and 23.70 seconds; peak RSS was 74,460, 74,268 and
73,760 KiB. Cache state was unmanaged and other desktop processes were active;
the first run is retained, not discarded or attributed to a compiler change.
These are initial observations, not an isolated performance benchmark or an
allocation profile. The small/large/mixed and array corpus baseline is still
pending, as is allocation instrumentation.

Capture compiler/Pypor/corpus revisions, build options, thread count, cache
conditions, repeated wall/CPU measurements, peak RSS and allocation counts/bytes.
Use small-file, large-file, mixed and disjoint-array workloads. Preserve the
sequential implementation as the semantic oracle; cloc/tokei comparisons must
account for differing classification rules. Inspect representative AMIR and
assembly before attributing costs to the scheduler.

Compile-fail cases cover escaped views, use after transfer, overlapping mutable
partitions and unsafe shared resources. Runtime cases cover producer failure,
consumer failure, cancellation with full admission, nested execution with one
worker, exact-once cleanup, partial thread creation, and repeated scope reuse.
Coordinate race tests with explicit synchronization rather than sleep timing.
Native Linux, Windows and macOS evidence is required; host-only results do not
establish portability. Run the validation sequence and affected invariant tests
specified by AGENTS.md for each implementation delivery.

## References

- [Rust scoped threads](https://doc.rust-lang.org/std/thread/fn.scope.html):
  scope completion and borrowed data lifetime.
- [Rayon pool configuration](https://docs.rs/rayon/latest/rayon/struct.ThreadPoolBuilder.html):
  reusable configurable workers; not a substitute for admission budgets.
- [Swift structured concurrency](https://github.com/swiftlang/swift-evolution/blob/main/proposals/0304-structured-concurrency.md):
  task hierarchy and cooperative cancellation.

At campaign completion, consolidate decisions and evidence into runtime/memory
architecture documentation and remove this temporary document.

## Initial storage proof and limits

The proof uses an iterative breadth-first traversal of interned type IDs, with
limits of 256 edges of depth and 4096 distinct types. Exhaustion rejects the
bound rather than accepting unknown storage. Scalars require no traversal
allocation; aggregate graphs use a temporary work queue and visited set to
avoid recursive stack growth and repeated work. This is not an allocation
optimization claim. A malformed expanding generic graph is covered directly
in typeck, independently of declaration validation.

Regression tests cover nested structs/enums, phantom arguments, raw pointers,
borrowed views, destructors, numeric inference, generic bound propagation,
canonical import aliases, unrelated user interfaces and transitive task handles.
The same conservative storage subset currently implements both capabilities;
this does not establish scoped borrowing, disjoint mutation, task effects or
thread-safe FFI resource operations. Opaque resource wrappers must retain their
negative storage contract (for example a phantom pointer or a destructor);
scalar fields alone cannot describe external thread affinity. There is no
unsafe opt-in or public parallel executor in this delivery.

Validation of owned generic context/result transport (Linux, 2026-09-09): the
ordered workspace fmt/check/Clippy/test/diagnostic-catalog/rustdoc sequence
passed, followed by architecture and line-ending checks. Pypor again passed
`check .` and its four smoke tests with the checkout CLI. Native Windows/macOS
execution and a worker ABI remain subsequent gates.

## Worker transport ABI

The runtime now has an internal `WorkerTask` transport built on the existing
validated `OwnedPayload` allocation contract. It moves a Send-proven context
into a worker, reserves result storage using the result's exact size and
alignment, and returns an opaque `WorkerResult` that retains the Send proof.
`OwnedPayload` itself remains thread-confined, so GenRef storage does not gain
an accidental global Send implementation.

The erased thunk has one typed status contract: it consumes context on every
return, initializes result only on completion, and never unwinds across its C
ABI. Dropping a task before execution runs context cleanup; failure and unknown
status release uninitialized result storage without invoking result drop glue.
Tests exercise an actual OS-thread crossing, exact-once context/result drops,
failure, invalid status and 64-byte result alignment. Construction is unsafe
because only compiler-generated glue can prove the erased thunk matches the
declared context/result descriptors.

Zero-sized results use an aligned sentinel derived from their descriptor rather
than a byte-aligned dangling pointer; a 128-byte-aligned ZST regression guards
the strict pointer-alignment requirement even when no allocation occurs.

This is not yet a public language ABI. Compiler thunk generation, a standalone
C mirror, bounded admission, worker reuse and running-task cancellation remain
required before exposing a parallel operation. No scheduler dependency or
second payload allocator was introduced.

Validation of the initial storage proof (Linux, 2026-09-09): the ordered workspace
fmt/check/Clippy/test/diagnostic-catalog/rustdoc sequence passed, followed by
single-vs-eight-thread diagnostic determinism, architecture and line-ending
checks. The Pypor consumer passed `check .` and all four smoke tests using the
checkout CLI. This does not replace native Windows/macOS validation or establish
parallel execution performance.
