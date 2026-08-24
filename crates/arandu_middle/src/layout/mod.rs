mod data_layout;

pub use data_layout::{DataLayout, SizeAlign};

use crate::SymbolId;
use crate::index_vec::IdIndex;
use crate::types::{ArType, Primitive, TypeId, TypeInterner};
use rustc_hash::FxHashMap;

/// A compact contiguous range into a dense backing table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct DenseRange {
    pub start: u32,
    pub len: u32,
}

impl DenseRange {
    #[must_use]
    pub const fn empty() -> Self {
        Self { start: 0, len: 0 }
    }

    #[must_use]
    pub const fn new(start: usize, len: usize) -> Self {
        Self {
            start: start as u32,
            len: len as u32,
        }
    }

    #[must_use]
    pub const fn start_usize(self) -> usize {
        self.start as usize
    }

    #[must_use]
    pub const fn len_usize(self) -> usize {
        self.len as usize
    }

    #[must_use]
    pub const fn end_usize(self) -> usize {
        self.start as usize + self.len as usize
    }

    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.len == 0
    }

    #[must_use]
    pub fn as_range(self) -> std::ops::Range<usize> {
        self.start_usize()..self.end_usize()
    }

    #[must_use]
    pub fn iter_ids<I: IdIndex>(self) -> DenseRangeIds<I> {
        DenseRangeIds {
            next: self.start_usize(),
            end: self.end_usize(),
            _marker: std::marker::PhantomData,
        }
    }
}

pub struct DenseRangeIds<I: IdIndex> {
    next: usize,
    end: usize,
    _marker: std::marker::PhantomData<I>,
}

impl<I: IdIndex> Iterator for DenseRangeIds<I> {
    type Item = I;

    fn next(&mut self) -> Option<Self::Item> {
        if self.next >= self.end {
            return None;
        }
        let id = I::from_usize(self.next);
        self.next += 1;
        Some(id)
    }
}

/// Physical memory layout metadata for a resolved type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TypeLayout {
    pub size: u64,               // Total size in bytes (including trailing padding)
    pub align: u64,              // Alignment required in bytes (power of 2)
    pub field_offsets: Vec<u64>, // Field offsets (populated for structs, tuples, etc.)
}

/// Target-derived runtime contract for a promoted GenRef payload.
///
/// Drop glue is attached during backend lowering; size and alignment always
/// originate in [`LayoutEngine`] rather than the compiler host.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct GenPayloadLayout {
    pub size: u64,
    pub align: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayoutOperation {
    ArrayRepeat,
    FieldOffset,
    AggregatePadding,
    EnumPayload,
    ResultPayload,
    OptionPayload,
    RangePair,
    GenPayload,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayoutError {
    SizeOverflow {
        operation: LayoutOperation,
        limit: u64,
    },
    InvalidAlignment {
        align: u64,
    },
}

impl std::fmt::Display for LayoutError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SizeOverflow { operation, limit } => write!(
                f,
                "type layout overflow during {operation:?}; target object size must be below {limit} bytes"
            ),
            Self::InvalidAlignment { align } => {
                write!(f, "invalid type alignment {align}; expected a power of two")
            }
        }
    }
}

impl std::error::Error for LayoutError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnumPayloadShape {
    pub payload_ty: Option<TypeId>,
}

/// Decoupled metadata provider to resolve struct fields and generic parameters.
pub trait StructLayoutProvider {
    fn get_struct_fields(&self, struct_id: SymbolId) -> Option<&FxHashMap<String, TypeId>>;
    fn get_struct_field_indices(&self, struct_id: SymbolId) -> Option<&FxHashMap<String, usize>>;
    fn get_generic_params(&self, struct_id: SymbolId) -> Option<&[SymbolId]>;
    fn get_enum_variants(&self, enum_id: SymbolId) -> Option<Vec<EnumPayloadShape>>;
}

/// The physical memory layout engine.
///
/// All target-dependent sizes/alignments come from [`DataLayout`]. Prefer
/// [`LayoutEngine::from_data_layout`] / [`LayoutEngine::host`];
/// [`LayoutEngine::new`] remains as sugar for [`DataLayout::ptr_width`].
///
/// Language `float` is always IEEE f64 (see [`DataLayout`]); platform `int`
/// follows pointer width. i686 uses [`DataLayout::i686_sysv`] for i64/f64
/// abi_align=4.
#[derive(Debug, Clone)]
pub struct LayoutEngine {
    pub data_layout: DataLayout,
}

impl LayoutEngine {
    /// Sugar for [`DataLayout::ptr_width`] (standard LP64/ILP32-style rules).
    #[must_use]
    pub fn new(pointer_width: u64) -> Self {
        Self::from_data_layout(DataLayout::ptr_width(pointer_width))
    }

    #[must_use]
    pub fn from_data_layout(data_layout: DataLayout) -> Self {
        Self { data_layout }
    }

    /// Host process layout (Cranelift JIT / host C parity).
    #[must_use]
    pub fn host() -> Self {
        Self::from_data_layout(DataLayout::host())
    }

    #[must_use]
    pub fn pointer_width(&self) -> u64 {
        self.data_layout.pointer_width()
    }

    /// Fat pointer ABI for `str` and `[]T`: `{ ptr, len }` where `len` is
    /// target `usize` (same width as a pointer). Offsets: ptr@0, len@W.
    /// 64-bit → size 16; 32-bit → size 8 (matches `arandu-abi-layout`).
    #[must_use]
    pub fn fat_pointer_layout(&self) -> TypeLayout {
        let w = self.pointer_width();
        let align = self.data_layout.pointer_align();
        TypeLayout {
            size: w * 2,
            align,
            field_offsets: vec![0, w],
        }
    }

    /// Byte offset of the `len` field in a fat pointer (`str` / slice).
    #[must_use]
    pub fn fat_ptr_len_offset(&self) -> u64 {
        self.pointer_width()
    }

    /// Byte size of the `len` field (`usize` of the target).
    #[must_use]
    pub fn fat_ptr_len_size(&self) -> u64 {
        self.pointer_width()
    }

    /// Compute the memory layout of any canonical `TypeId`.
    pub fn layout_of(
        &self,
        type_id: TypeId,
        interner: &TypeInterner,
        provider: &dyn StructLayoutProvider,
    ) -> Result<TypeLayout, LayoutError> {
        self.layout_of_type(&interner.resolve(type_id), interner, provider)
    }

    /// Derive the checked physical contract consumed by GenRef runtime slots.
    pub fn gen_payload_layout(
        &self,
        type_id: TypeId,
        interner: &TypeInterner,
        provider: &dyn StructLayoutProvider,
    ) -> Result<GenPayloadLayout, LayoutError> {
        let layout = self.layout_of(type_id, interner, provider)?;
        let limit = self.data_layout.object_size_bound();
        if layout.size >= limit {
            return Err(LayoutError::SizeOverflow {
                operation: LayoutOperation::GenPayload,
                limit,
            });
        }
        if layout.align == 0 || !layout.align.is_power_of_two() {
            return Err(LayoutError::InvalidAlignment {
                align: layout.align,
            });
        }
        Ok(GenPayloadLayout {
            size: layout.size,
            align: layout.align,
        })
    }

    /// Compute the memory layout of a structural `ArType`.
    #[tracing::instrument(
        level = "trace",
        target = "arandu_middle::layout",
        skip(self, interner, provider)
    )]
    pub fn layout_of_type(
        &self,
        ty: &ArType,
        interner: &TypeInterner,
        provider: &dyn StructLayoutProvider,
    ) -> Result<TypeLayout, LayoutError> {
        Ok(match ty {
            ArType::Primitive(p) => match p {
                Primitive::I8
                | Primitive::U8
                | Primitive::Byte
                | Primitive::Bool
                | Primitive::Char => TypeLayout {
                    size: 1,
                    align: 1,
                    field_offsets: Vec::new(),
                },
                Primitive::I16 | Primitive::U16 => TypeLayout {
                    size: 2,
                    align: 2,
                    field_offsets: Vec::new(),
                },
                Primitive::I32 | Primitive::U32 | Primitive::F32 => TypeLayout {
                    size: 4,
                    align: 4,
                    field_offsets: Vec::new(),
                },
                Primitive::Float => {
                    let f = self.data_layout.float;
                    TypeLayout {
                        size: f.size,
                        align: f.abi_align,
                        field_offsets: Vec::new(),
                    }
                }
                Primitive::I64 | Primitive::U64 => {
                    let t = self.data_layout.i64;
                    TypeLayout {
                        size: t.size,
                        align: t.abi_align,
                        field_offsets: Vec::new(),
                    }
                }
                Primitive::F64 => {
                    let t = self.data_layout.f64;
                    TypeLayout {
                        size: t.size,
                        align: t.abi_align,
                        field_offsets: Vec::new(),
                    }
                }
                Primitive::Int | Primitive::Uint => {
                    let p = self.data_layout.pointer;
                    TypeLayout {
                        size: p.size,
                        align: p.abi_align,
                        field_offsets: Vec::new(),
                    }
                }
                Primitive::Str => self.fat_pointer_layout(),
                Primitive::Any => {
                    // Any is dynamic box pointer
                    let p = self.data_layout.pointer;
                    TypeLayout {
                        size: p.size,
                        align: p.abi_align,
                        field_offsets: Vec::new(),
                    }
                }
            },
            ArType::IntLiteral => {
                let p = self.data_layout.pointer;
                TypeLayout {
                    size: p.size,
                    align: p.abi_align,
                    field_offsets: Vec::new(),
                }
            }
            ArType::FloatLiteral => {
                let f = self.data_layout.float;
                TypeLayout {
                    size: f.size,
                    align: f.abi_align,
                    field_offsets: Vec::new(),
                }
            }
            // `Err` is a non-null message handle (pointer to a NUL-terminated
            // UTF-8 buffer allocated by `err.new`). Not a ZST — payload of
            // `Result<T, Err>` must be distinguishable from nil.
            ArType::Err => {
                let p = self.data_layout.pointer;
                TypeLayout {
                    size: p.size,
                    align: p.abi_align,
                    field_offsets: Vec::new(),
                }
            }
            ArType::Void | ArType::Error => TypeLayout {
                size: 0,
                align: 1,
                field_offsets: Vec::new(),
            },
            ArType::Ptr(_) | ArType::Ref(_) | ArType::RefMut(_) => {
                // Safe refs and raw pointers are single machine pointers (fat types later).
                let p = self.data_layout.pointer;
                TypeLayout {
                    size: p.size,
                    align: p.abi_align,
                    field_offsets: Vec::new(),
                }
            }
            // F2.3: GenRef = {u32 index, u32 generation} — always 8 bytes.
            ArType::GenRef => TypeLayout {
                size: 8,
                align: 4,
                field_offsets: vec![0, 4],
            },
            ArType::Nullable(_) => {
                // Nullable is always a null-or-pointer handle (box for scalars;
                // heap object pointer for Named/etc.). Never stores the payload
                // inline — avoids `int? = 0` colliding with `nil`.
                let p = self.data_layout.pointer;
                TypeLayout {
                    size: p.size,
                    align: p.abi_align,
                    field_offsets: Vec::new(),
                }
            }
            ArType::Slice(_) => self.fat_pointer_layout(),
            ArType::Array(len, inner) => {
                let inner_layout = self.layout_of(*inner, interner, provider)?;
                TypeLayout {
                    size: self.checked_mul(
                        inner_layout.size,
                        *len,
                        LayoutOperation::ArrayRepeat,
                    )?,
                    align: inner_layout.align,
                    field_offsets: Vec::new(),
                }
            }
            ArType::Tuple(tys) => {
                let mut current_offset = 0;
                let mut max_align = 1;
                let mut field_offsets = Vec::with_capacity(tys.len());

                for &ty_id in tys {
                    let layout = self.layout_of(ty_id, interner, provider)?;
                    max_align = max_align.max(layout.align);
                    current_offset =
                        self.align_up(current_offset, layout.align, LayoutOperation::FieldOffset)?;
                    field_offsets.push(current_offset);
                    current_offset = self.checked_add(
                        current_offset,
                        layout.size,
                        LayoutOperation::FieldOffset,
                    )?;
                }

                let total_size =
                    self.align_up(current_offset, max_align, LayoutOperation::AggregatePadding)?;

                TypeLayout {
                    size: total_size,
                    align: max_align,
                    field_offsets,
                }
            }
            ArType::Named(symbol_id, generic_args) => {
                if let Some(fields_def) = provider.get_struct_fields(*symbol_id) {
                    if let Some(indices_def) = provider.get_struct_field_indices(*symbol_id) {
                        // Only `idx` and field TypeId are needed after the sort.
                        let mut fields_with_indices: Vec<(usize, TypeId)> = Vec::new();
                        for (name, &tid) in fields_def {
                            if let Some(&idx) = indices_def.get(name) {
                                fields_with_indices.push((idx, tid));
                            }
                        }
                        fields_with_indices.sort_by_key(|x| x.0);

                        let generic_params = provider.get_generic_params(*symbol_id).unwrap_or(&[]);
                        let subst: FxHashMap<SymbolId, TypeId> = generic_params
                            .iter()
                            .copied()
                            .zip(generic_args.iter().copied())
                            .collect();

                        let mut current_offset = 0;
                        let mut max_align = 1;
                        let mut field_offsets = Vec::with_capacity(fields_with_indices.len());

                        for (_, tid) in fields_with_indices {
                            let ty = interner.resolve(tid);
                            let substituted = substitute(&ty, &subst, interner);
                            let layout = self.layout_of_type(&substituted, interner, provider)?;
                            max_align = max_align.max(layout.align);
                            current_offset = self.align_up(
                                current_offset,
                                layout.align,
                                LayoutOperation::FieldOffset,
                            )?;
                            field_offsets.push(current_offset);
                            current_offset = self.checked_add(
                                current_offset,
                                layout.size,
                                LayoutOperation::FieldOffset,
                            )?;
                        }

                        let total_size = self.align_up(
                            current_offset,
                            max_align,
                            LayoutOperation::AggregatePadding,
                        )?;

                        TypeLayout {
                            size: total_size,
                            align: max_align,
                            field_offsets,
                        }
                    } else {
                        TypeLayout {
                            size: 0,
                            align: 1,
                            field_offsets: Vec::new(),
                        }
                    }
                } else if let Some(variants) = provider.get_enum_variants(*symbol_id) {
                    let tag_size = self.pointer_width();
                    let mut max_payload_size = 0;
                    let mut max_payload_align = 1;
                    for variant in variants {
                        if let Some(payload_ty_id) = variant.payload_ty {
                            let payload_layout =
                                self.layout_of(payload_ty_id, interner, provider)?;
                            if payload_layout.size > max_payload_size {
                                max_payload_size = payload_layout.size;
                            }
                            if payload_layout.align > max_payload_align {
                                max_payload_align = payload_layout.align;
                            }
                        }
                    }
                    let max_align = max_payload_align.max(tag_size);
                    let payload_end =
                        self.checked_add(tag_size, max_payload_size, LayoutOperation::EnumPayload)?;
                    let size =
                        self.align_up(payload_end, max_align, LayoutOperation::AggregatePadding)?;
                    TypeLayout {
                        size,
                        align: max_align,
                        field_offsets: vec![0, tag_size],
                    }
                } else {
                    TypeLayout {
                        size: 0,
                        align: 1,
                        field_offsets: Vec::new(),
                    }
                }
            }

            ArType::Func(_, _) => TypeLayout {
                size: self.pointer_width(),
                align: self.pointer_width(),
                field_offsets: Vec::new(),
            },
            ArType::Result(ok, err) => {
                let ok_layout = self.layout_of(*ok, interner, provider)?;
                let err_layout = self.layout_of(*err, interner, provider)?;
                let max_align = ok_layout
                    .align
                    .max(err_layout.align)
                    .max(self.pointer_width());
                let tag_offset = 0;
                let payload_offset = self.pointer_width();
                let max_payload_size = ok_layout.size.max(err_layout.size);
                let payload_end = self.checked_add(
                    payload_offset,
                    max_payload_size,
                    LayoutOperation::ResultPayload,
                )?;
                let total_size =
                    self.align_up(payload_end, max_align, LayoutOperation::AggregatePadding)?;

                TypeLayout {
                    size: total_size,
                    align: max_align,
                    field_offsets: vec![tag_offset, payload_offset],
                }
            }
            ArType::Option(inner) | ArType::Poll(inner) => {
                let inner_layout = self.layout_of(*inner, interner, provider)?;
                let max_align = inner_layout.align.max(self.pointer_width());
                let tag_offset = 0;
                let payload_offset = self.pointer_width();
                let payload_end = self.checked_add(
                    payload_offset,
                    inner_layout.size,
                    LayoutOperation::OptionPayload,
                )?;
                let total_size =
                    self.align_up(payload_end, max_align, LayoutOperation::AggregatePadding)?;

                TypeLayout {
                    size: total_size,
                    align: max_align,
                    field_offsets: vec![tag_offset, payload_offset],
                }
            }
            ArType::Coroutine(_) => TypeLayout {
                size: self.pointer_width(),
                align: self.pointer_width(),
                field_offsets: Vec::new(),
            },
            ArType::Range(inner) => {
                let inner_layout = self.layout_of(*inner, interner, provider)?;
                let align = inner_layout.align;
                let start_offset = 0;
                let end_offset =
                    self.align_up(inner_layout.size, align, LayoutOperation::RangePair)?;
                let end =
                    self.checked_add(end_offset, inner_layout.size, LayoutOperation::RangePair)?;
                let total_size = self.align_up(end, align, LayoutOperation::AggregatePadding)?;

                TypeLayout {
                    size: total_size,
                    align,
                    field_offsets: vec![start_offset, end_offset],
                }
            }
        })
    }

    fn checked_add(
        &self,
        lhs: u64,
        rhs: u64,
        operation: LayoutOperation,
    ) -> Result<u64, LayoutError> {
        let value = lhs.checked_add(rhs).ok_or(LayoutError::SizeOverflow {
            operation,
            limit: self.data_layout.object_size_bound(),
        })?;
        self.check_object_size(value, operation)
    }

    fn checked_mul(
        &self,
        lhs: u64,
        rhs: u64,
        operation: LayoutOperation,
    ) -> Result<u64, LayoutError> {
        let value = lhs.checked_mul(rhs).ok_or(LayoutError::SizeOverflow {
            operation,
            limit: self.data_layout.object_size_bound(),
        })?;
        self.check_object_size(value, operation)
    }

    fn align_up(
        &self,
        value: u64,
        align: u64,
        operation: LayoutOperation,
    ) -> Result<u64, LayoutError> {
        if align == 0 || !align.is_power_of_two() {
            return Err(LayoutError::InvalidAlignment { align });
        }
        let mask = align - 1;
        let padded = value.checked_add(mask).ok_or(LayoutError::SizeOverflow {
            operation,
            limit: self.data_layout.object_size_bound(),
        })? & !mask;
        self.check_object_size(padded, operation)
    }

    fn check_object_size(
        &self,
        value: u64,
        operation: LayoutOperation,
    ) -> Result<u64, LayoutError> {
        let limit = self.data_layout.object_size_bound();
        if value >= limit {
            Err(LayoutError::SizeOverflow { operation, limit })
        } else {
            Ok(value)
        }
    }
}

fn substitute(ty: &ArType, subst: &FxHashMap<SymbolId, TypeId>, interner: &TypeInterner) -> ArType {
    match ty {
        ArType::Named(id, args) => {
            if let Some(&concrete_id) = subst.get(id) {
                interner.resolve(concrete_id)
            } else {
                let new_args = args
                    .iter()
                    .map(|&arg_id| {
                        let arg_ty = interner.resolve(arg_id);
                        let substituted_arg = substitute(&arg_ty, subst, interner);
                        interner.lookup(&substituted_arg).unwrap_or(arg_id)
                    })
                    .collect();
                ArType::Named(*id, new_args)
            }
        }
        ArType::Func(params, ret) => {
            let new_params = params
                .iter()
                .map(|&param_id| {
                    let param_ty = interner.resolve(param_id);
                    let substituted_param = substitute(&param_ty, subst, interner);
                    interner.lookup(&substituted_param).unwrap_or(param_id)
                })
                .collect();
            let ret_ty = interner.resolve(*ret);
            let substituted_ret = substitute(&ret_ty, subst, interner);
            let new_ret = interner.lookup(&substituted_ret).unwrap_or(*ret);
            ArType::Func(new_params, new_ret)
        }
        ArType::Nullable(inner) => {
            let inner_ty = interner.resolve(*inner);
            let substituted_inner = substitute(&inner_ty, subst, interner);
            let new_inner = interner.lookup(&substituted_inner).unwrap_or(*inner);
            ArType::Nullable(new_inner)
        }
        ArType::Slice(inner) => {
            let inner_ty = interner.resolve(*inner);
            let substituted_inner = substitute(&inner_ty, subst, interner);
            let new_inner = interner.lookup(&substituted_inner).unwrap_or(*inner);
            ArType::Slice(new_inner)
        }
        ArType::Array(len, inner) => {
            let inner_ty = interner.resolve(*inner);
            let substituted_inner = substitute(&inner_ty, subst, interner);
            let new_inner = interner.lookup(&substituted_inner).unwrap_or(*inner);
            ArType::Array(*len, new_inner)
        }
        ArType::Ptr(inner) => {
            let inner_ty = interner.resolve(*inner);
            let substituted_inner = substitute(&inner_ty, subst, interner);
            let new_inner = interner.lookup(&substituted_inner).unwrap_or(*inner);
            ArType::Ptr(new_inner)
        }
        ArType::Ref(inner) => {
            let inner_ty = interner.resolve(*inner);
            let substituted_inner = substitute(&inner_ty, subst, interner);
            let new_inner = interner.lookup(&substituted_inner).unwrap_or(*inner);
            ArType::Ref(new_inner)
        }
        ArType::RefMut(inner) => {
            let inner_ty = interner.resolve(*inner);
            let substituted_inner = substitute(&inner_ty, subst, interner);
            let new_inner = interner.lookup(&substituted_inner).unwrap_or(*inner);
            ArType::RefMut(new_inner)
        }
        ArType::Tuple(tys) => {
            let new_tys = tys
                .iter()
                .map(|&ty_id| {
                    let item_ty = interner.resolve(ty_id);
                    let substituted_item = substitute(&item_ty, subst, interner);
                    interner.lookup(&substituted_item).unwrap_or(ty_id)
                })
                .collect();
            ArType::Tuple(new_tys)
        }
        ArType::Result(ok, err) => {
            let ok_ty = interner.resolve(*ok);
            let substituted_ok = substitute(&ok_ty, subst, interner);
            let new_ok = interner.lookup(&substituted_ok).unwrap_or(*ok);

            let err_ty = interner.resolve(*err);
            let substituted_err = substitute(&err_ty, subst, interner);
            let new_err = interner.lookup(&substituted_err).unwrap_or(*err);

            ArType::Result(new_ok, new_err)
        }
        ArType::Option(inner) => {
            let inner_ty = interner.resolve(*inner);
            let substituted_inner = substitute(&inner_ty, subst, interner);
            let new_inner = interner.lookup(&substituted_inner).unwrap_or(*inner);
            ArType::Option(new_inner)
        }
        ArType::Coroutine(inner) => {
            let inner_ty = interner.resolve(*inner);
            let substituted_inner = substitute(&inner_ty, subst, interner);
            let new_inner = interner.lookup(&substituted_inner).unwrap_or(*inner);
            ArType::Coroutine(new_inner)
        }
        ArType::Poll(inner) => {
            let inner_ty = interner.resolve(*inner);
            let substituted_inner = substitute(&inner_ty, subst, interner);
            let new_inner = interner.lookup(&substituted_inner).unwrap_or(*inner);
            ArType::Poll(new_inner)
        }
        ArType::Range(inner) => {
            let inner_ty = interner.resolve(*inner);
            let substituted_inner = substitute(&inner_ty, subst, interner);
            let new_inner = interner.lookup(&substituted_inner).unwrap_or(*inner);
            ArType::Range(new_inner)
        }
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests;
