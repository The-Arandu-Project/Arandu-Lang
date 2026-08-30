use super::*;
use crate::newtype_index;

struct LayoutEngine(super::LayoutEngine);

impl LayoutEngine {
    fn new(pointer_width: u64) -> Self {
        Self(super::LayoutEngine::new(pointer_width))
    }

    fn from_data_layout(data_layout: DataLayout) -> Self {
        Self(super::LayoutEngine::from_data_layout(data_layout))
    }

    #[allow(clippy::panic)]
    fn layout_of(
        &self,
        type_id: TypeId,
        interner: &TypeInterner,
        provider: &dyn StructLayoutProvider,
    ) -> TypeLayout {
        match self.0.layout_of(type_id, interner, provider) {
            Ok(layout) => layout,
            Err(error) => panic!("expected valid test layout, got {error}"),
        }
    }

    fn try_layout_of(
        &self,
        type_id: TypeId,
        interner: &TypeInterner,
        provider: &dyn StructLayoutProvider,
    ) -> Result<TypeLayout, LayoutError> {
        self.0.layout_of(type_id, interner, provider)
    }

    fn gen_payload_layout(
        &self,
        type_id: TypeId,
        interner: &TypeInterner,
        provider: &dyn StructLayoutProvider,
    ) -> Result<GenPayloadLayout, LayoutError> {
        self.0.gen_payload_layout(type_id, interner, provider)
    }

    fn fat_ptr_len_offset(&self) -> u64 {
        self.0.fat_ptr_len_offset()
    }
}

newtype_index!(TestId);

#[test]
fn empty_range_has_no_ids() {
    let range = DenseRange::empty();
    assert!(range.is_empty());
    assert_eq!(range.as_range(), 0..0);
    assert!(range.iter_ids::<TestId>().next().is_none());
}

#[test]
fn typed_iteration_returns_dense_ids() {
    assert_eq!(TestId::from_usize(9).as_usize(), 9);
    let ids: Vec<_> = DenseRange::new(2, 3).iter_ids::<TestId>().collect();
    assert_eq!(ids, vec![TestId(2), TestId(3), TestId(4)]);
}

struct MockProvider;
impl StructLayoutProvider for MockProvider {
    fn get_struct_fields(&self, _struct_id: SymbolId) -> Option<&FxHashMap<String, TypeId>> {
        None
    }
    fn get_struct_field_indices(&self, _struct_id: SymbolId) -> Option<&FxHashMap<String, usize>> {
        None
    }
    fn get_generic_params(&self, _struct_id: SymbolId) -> Option<&[SymbolId]> {
        None
    }
    fn get_enum_variants(&self, _enum_id: SymbolId) -> Option<Vec<EnumPayloadShape>> {
        None
    }
}

#[test]
fn test_primitive_layouts_64bit() {
    let engine = LayoutEngine::new(8);
    let interner = TypeInterner::new();
    let provider = MockProvider;

    let u8_id = interner.intern(ArType::Primitive(Primitive::U8));
    let layout_u8 = engine.layout_of(u8_id, &interner, &provider);
    assert_eq!(layout_u8.size, 1);
    assert_eq!(layout_u8.align, 1);

    let i32_id = interner.intern(ArType::Primitive(Primitive::I32));
    let layout_i32 = engine.layout_of(i32_id, &interner, &provider);
    assert_eq!(layout_i32.size, 4);
    assert_eq!(layout_i32.align, 4);

    let str_id = interner.intern(ArType::Primitive(Primitive::Str));
    let layout_str = engine.layout_of(str_id, &interner, &provider);
    assert_eq!(layout_str.size, 16);
    assert_eq!(layout_str.align, 8);
    assert_eq!(layout_str.field_offsets, vec![0, 8]);
}

#[test]
fn test_primitive_layouts_32bit() {
    let engine = LayoutEngine::new(4);
    let interner = TypeInterner::new();
    let provider = MockProvider;

    let str_id = interner.intern(ArType::Primitive(Primitive::Str));
    let layout_str = engine.layout_of(str_id, &interner, &provider);
    // Fat pointer: ptr(4) + len as usize(4) → size 8 on 32-bit.
    assert_eq!(layout_str.size, 8);
    assert_eq!(layout_str.align, 4);
    assert_eq!(layout_str.field_offsets, vec![0, 4]);
    assert_eq!(engine.fat_ptr_len_offset(), 4);

    let int_id = interner.intern(ArType::Primitive(Primitive::Int));
    let layout_int = engine.layout_of(int_id, &interner, &provider);
    assert_eq!(layout_int.size, 4);
    assert_eq!(layout_int.align, 4);

    let uint_id = interner.intern(ArType::Primitive(Primitive::Uint));
    let layout_uint = engine.layout_of(uint_id, &interner, &provider);
    assert_eq!(layout_uint.size, 4);
    assert_eq!(layout_uint.align, 4);

    let int_lit_id = interner.intern(ArType::IntLiteral);
    let layout_int_lit = engine.layout_of(int_lit_id, &interner, &provider);
    assert_eq!(layout_int_lit.size, 4);
    assert_eq!(layout_int_lit.align, 4);
}

#[test]
fn gen_payload_layout_uses_target_not_host_width() {
    let interner = TypeInterner::new();
    let provider = MockProvider;
    let string = interner.intern(ArType::Primitive(Primitive::Str));

    let layout_64 = LayoutEngine::new(8)
        .gen_payload_layout(string, &interner, &provider)
        .unwrap();
    let layout_32 = LayoutEngine::from_data_layout(DataLayout::i686_sysv())
        .gen_payload_layout(string, &interner, &provider)
        .unwrap();

    assert_eq!(layout_64, GenPayloadLayout { size: 16, align: 8 });
    assert_eq!(layout_32, GenPayloadLayout { size: 8, align: 4 });
}

#[test]
fn gen_payload_layout_preserves_zero_sized_void_contract() {
    let interner = TypeInterner::new();
    let provider = MockProvider;
    let void = interner.intern(ArType::Void);
    let layout = LayoutEngine::new(8)
        .gen_payload_layout(void, &interner, &provider)
        .unwrap();

    assert_eq!(layout, GenPayloadLayout { size: 0, align: 1 });
}

struct StructMockProvider {
    fields: FxHashMap<SymbolId, FxHashMap<String, TypeId>>,
    field_indices: FxHashMap<SymbolId, FxHashMap<String, usize>>,
    generic_params: FxHashMap<SymbolId, Vec<SymbolId>>,
    enum_variants: FxHashMap<SymbolId, Vec<EnumPayloadShape>>,
}

impl StructLayoutProvider for StructMockProvider {
    fn get_struct_fields(&self, struct_id: SymbolId) -> Option<&FxHashMap<String, TypeId>> {
        self.fields.get(&struct_id)
    }
    fn get_struct_field_indices(&self, struct_id: SymbolId) -> Option<&FxHashMap<String, usize>> {
        self.field_indices.get(&struct_id)
    }
    fn get_generic_params(&self, struct_id: SymbolId) -> Option<&[SymbolId]> {
        self.generic_params.get(&struct_id).map(|v| v.as_slice())
    }
    fn get_enum_variants(&self, enum_id: SymbolId) -> Option<Vec<EnumPayloadShape>> {
        self.enum_variants.get(&enum_id).cloned()
    }
}

#[test]
fn test_all_primitive_layouts() {
    for &ptr_width in &[4u64, 8] {
        let engine = LayoutEngine::new(ptr_width);
        let interner = TypeInterner::new();
        let provider = MockProvider;

        let cases = [
            (Primitive::I8, 1u64, 1u64),
            (Primitive::U8, 1, 1),
            (Primitive::Byte, 1, 1),
            (Primitive::Bool, 1, 1),
            (Primitive::Char, 4, 4),
            (Primitive::I16, 2, 2),
            (Primitive::U16, 2, 2),
            (Primitive::I32, 4, 4),
            (Primitive::U32, 4, 4),
            (Primitive::F32, 4, 4),
            // Language float is always IEEE f64 (DataLayout), not pointer-width.
            (Primitive::Float, 8, 8),
            (Primitive::Int, ptr_width, ptr_width),
            (Primitive::Uint, ptr_width, ptr_width),
            (Primitive::I64, 8, 8),
            (Primitive::U64, 8, 8),
            (Primitive::F64, 8, 8),
            (Primitive::Any, ptr_width, ptr_width),
        ];
        for (prim, size, align) in cases {
            let tid = interner.intern(ArType::Primitive(prim));
            let layout = engine.layout_of(tid, &interner, &provider);
            assert_eq!(layout.size, size, "{prim:?} size at ptr_width={ptr_width}");
            assert_eq!(
                layout.align, align,
                "{prim:?} align at ptr_width={ptr_width}"
            );
            assert!(layout.field_offsets.is_empty());
        }
    }
}

#[test]
fn test_i686_sysv_i64_align_4() {
    let engine = LayoutEngine::from_data_layout(DataLayout::i686_sysv());
    let interner = TypeInterner::new();
    let provider = MockProvider;
    let i64_id = interner.intern(ArType::Primitive(Primitive::I64));
    let layout = engine.layout_of(i64_id, &interner, &provider);
    assert_eq!(layout.size, 8);
    assert_eq!(layout.align, 4);
    let f64_id = interner.intern(ArType::Primitive(Primitive::F64));
    let fl = engine.layout_of(f64_id, &interner, &provider);
    assert_eq!(fl.size, 8);
    assert_eq!(fl.align, 4);
    // Fat pointer still 8 bytes on 32-bit pointer width.
    let str_id = interner.intern(ArType::Primitive(Primitive::Str));
    let sl = engine.layout_of(str_id, &interner, &provider);
    assert_eq!(sl.size, 8);
    assert_eq!(sl.field_offsets, vec![0, 4]);
}

#[test]
fn test_ptr_layout() {
    for &ptr_width in &[4u64, 8] {
        let engine = LayoutEngine::new(ptr_width);
        let interner = TypeInterner::new();
        let provider = MockProvider;
        let inner = interner.intern(ArType::Primitive(Primitive::I32));
        let ptr_ty = ArType::Ptr(inner);
        let tid = interner.intern(ptr_ty);
        let layout = engine.layout_of(tid, &interner, &provider);
        assert_eq!(layout.size, ptr_width);
        assert_eq!(layout.align, ptr_width);
    }
}

#[test]
fn test_slice_layout() {
    for &ptr_width in &[4u64, 8] {
        let engine = LayoutEngine::new(ptr_width);
        let interner = TypeInterner::new();
        let provider = MockProvider;
        let inner = interner.intern(ArType::Primitive(Primitive::I32));
        let slice_ty = ArType::Slice(inner);
        let tid = interner.intern(slice_ty);
        let layout = engine.layout_of(tid, &interner, &provider);
        // Fat pointer: ptr(W) + len as usize(W)
        assert_eq!(layout.field_offsets, vec![0, ptr_width]);
        assert_eq!(layout.size, ptr_width * 2);
        assert_eq!(layout.align, ptr_width);
    }
}

#[test]
fn test_array_layout() {
    let engine = LayoutEngine::new(8);
    let interner = TypeInterner::new();
    let provider = MockProvider;
    let elem = interner.intern(ArType::Primitive(Primitive::I32));
    let arr_ty = ArType::Array(5, elem);
    let tid = interner.intern(arr_ty);
    let layout = engine.layout_of(tid, &interner, &provider);
    assert_eq!(layout.size, 20); // 5 * 4
    assert_eq!(layout.align, 4);
}

#[test]
fn array_layout_rejects_target_size_bound_on_32_and_64_bit() {
    let interner = TypeInterner::new();
    let provider = MockProvider;
    let byte = interner.intern(ArType::Primitive(Primitive::U8));

    for (pointer_width, bound) in [(4, 1_u64 << 31), (8, 1_u64 << 63)] {
        let engine = LayoutEngine::new(pointer_width);
        let last_valid = interner.intern(ArType::Array(bound - 1, byte));
        assert_eq!(
            engine
                .try_layout_of(last_valid, &interner, &provider)
                .map(|layout| layout.size),
            Ok(bound - 1)
        );

        let too_large = interner.intern(ArType::Array(bound, byte));
        assert_eq!(
            engine.try_layout_of(too_large, &interner, &provider),
            Err(LayoutError::SizeOverflow {
                operation: LayoutOperation::ArrayRepeat,
                limit: bound,
            })
        );
    }
}

#[test]
fn aggregate_layout_rejects_offset_that_crosses_target_bound() {
    let engine = LayoutEngine::new(4);
    let interner = TypeInterner::new();
    let provider = MockProvider;
    let byte = interner.intern(ArType::Primitive(Primitive::U8));
    let almost_full = interner.intern(ArType::Array((1_u64 << 31) - 1, byte));
    let tuple = interner.intern(ArType::Tuple(vec![almost_full, byte]));

    assert_eq!(
        engine.try_layout_of(tuple, &interner, &provider),
        Err(LayoutError::SizeOverflow {
            operation: LayoutOperation::FieldOffset,
            limit: 1_u64 << 31,
        })
    );
}

#[test]
fn test_void_error_layout() {
    let engine = LayoutEngine::new(8);
    let interner = TypeInterner::new();
    let provider = MockProvider;
    for ty in [ArType::Void, ArType::Error] {
        let tid = interner.intern(ty);
        let layout = engine.layout_of(tid, &interner, &provider);
        assert_eq!(layout.size, 0);
        assert_eq!(layout.align, 1);
    }
    // `Err` is a message handle (pointer-sized), not a ZST.
    let err_tid = interner.intern(ArType::Err);
    let err_layout = engine.layout_of(err_tid, &interner, &provider);
    assert_eq!(err_layout.size, 8);
    assert_eq!(err_layout.align, 8);
}

#[test]
fn test_func_layout() {
    let engine = LayoutEngine::new(8);
    let interner = TypeInterner::new();
    let provider = MockProvider;
    let int_id = interner.intern(ArType::Primitive(Primitive::Int));
    let func_ty = ArType::Func(vec![int_id, int_id], int_id);
    let tid = interner.intern(func_ty);
    let layout = engine.layout_of(tid, &interner, &provider);
    assert_eq!(layout.size, 8);
    assert_eq!(layout.align, 8);
}

#[test]
fn test_nullable_layout_is_pointer_handle() {
    let engine = LayoutEngine::new(8);
    let interner = TypeInterner::new();
    let provider = MockProvider;
    let inner = interner.intern(ArType::Primitive(Primitive::I32));
    let nullable = ArType::Nullable(inner);
    let tid = interner.intern(nullable);
    let layout = engine.layout_of(tid, &interner, &provider);
    // Handle ABI: always pointer-sized (null vs box/object ptr).
    assert_eq!(layout.size, 8);
    assert_eq!(layout.align, 8);
}

#[test]
fn test_tuple_layout() {
    let engine = LayoutEngine::new(8);
    let interner = TypeInterner::new();
    let provider = MockProvider;
    let u8_id = interner.intern(ArType::Primitive(Primitive::U8));
    let i32_id = interner.intern(ArType::Primitive(Primitive::I32));
    let u8_2 = interner.intern(ArType::Primitive(Primitive::U8));
    let tuple_ty = ArType::Tuple(vec![u8_id, i32_id, u8_2]);
    let tid = interner.intern(tuple_ty);
    let layout = engine.layout_of(tid, &interner, &provider);
    // u8 at 0, i32 at 4 (align 4), u8 at 8, total = 12 (aligned to 4)
    assert_eq!(layout.align, 4);
    assert_eq!(layout.field_offsets, vec![0, 4, 8]);
    assert_eq!(layout.size, 12);
}

#[test]
fn test_result_layout() {
    let engine = LayoutEngine::new(8);
    let interner = TypeInterner::new();
    let provider = MockProvider;
    let ok = interner.intern(ArType::Primitive(Primitive::I32));
    let err = interner.intern(ArType::Primitive(Primitive::U8));
    let result_ty = ArType::Result(ok, err);
    let tid = interner.intern(result_ty);
    let layout = engine.layout_of(tid, &interner, &provider);
    // tag at 0, payload at 8 (ptr_width), max payload = max(4,1) = 4, total = 12 aligned to max(4,1,8)=8 => 16
    assert_eq!(layout.field_offsets, vec![0, 8]);
    assert_eq!(layout.align, 8);
    assert_eq!(layout.size, 16);
}

#[test]
fn test_option_layout() {
    let engine = LayoutEngine::new(8);
    let interner = TypeInterner::new();
    let provider = MockProvider;
    let inner = interner.intern(ArType::Primitive(Primitive::I32));
    let opt_ty = ArType::Option(inner);
    let tid = interner.intern(opt_ty);
    let layout = engine.layout_of(tid, &interner, &provider);
    // tag at 0, payload at 8 (ptr_width), payload size 4, total = 12 aligned to max(4,8)=8 => 16
    assert_eq!(layout.field_offsets, vec![0, 8]);
    assert_eq!(layout.align, 8);
    assert_eq!(layout.size, 16);
}

#[test]
fn test_range_layout() {
    let engine = LayoutEngine::new(8);
    let interner = TypeInterner::new();
    let provider = MockProvider;
    let inner = interner.intern(ArType::Primitive(Primitive::I32));
    let range_ty = ArType::Range(inner);
    let tid = interner.intern(range_ty);
    let layout = engine.layout_of(tid, &interner, &provider);
    // start at 0, end at 4 (padding to align 4), total = 8
    assert_eq!(layout.field_offsets, vec![0, 4]);
    assert_eq!(layout.align, 4);
    assert_eq!(layout.size, 8);
}

#[test]
fn test_zst_allocator_field_adds_no_size() {
    let engine = LayoutEngine::new(8);
    let interner = TypeInterner::new();

    // Setup mock provider for ZST
    let mut provider = StructMockProvider {
        fields: FxHashMap::<SymbolId, FxHashMap<String, TypeId>>::default(),
        field_indices: FxHashMap::<SymbolId, FxHashMap<String, usize>>::default(),
        generic_params: FxHashMap::<SymbolId, Vec<SymbolId>>::default(),
        enum_variants: FxHashMap::<SymbolId, Vec<EnumPayloadShape>>::default(),
    };

    // Create ZST struct "GlobalAllocator"
    let zst_id = SymbolId::new(0, 100);
    provider
        .fields
        .insert(zst_id, FxHashMap::<String, TypeId>::default()); // No fields = ZST
    provider
        .field_indices
        .insert(zst_id, FxHashMap::<String, usize>::default());

    let zst_ty = ArType::Named(zst_id, vec![]);
    let zst_tid = interner.intern(zst_ty);
    let zst_layout = engine.layout_of(zst_tid, &interner, &provider);
    assert_eq!(zst_layout.size, 0); // ZST has 0 size
    assert_eq!(zst_layout.align, 1);

    // Create Vec struct with ZST field
    let vec_id = SymbolId::new(0, 101);
    let mut vec_fields = FxHashMap::<String, TypeId>::default();
    vec_fields.insert(
        "data".to_string(),
        interner.intern(ArType::Primitive(Primitive::I64)),
    ); // simplified pointer
    vec_fields.insert(
        "len".to_string(),
        interner.intern(ArType::Primitive(Primitive::U64)),
    );
    vec_fields.insert(
        "capacity".to_string(),
        interner.intern(ArType::Primitive(Primitive::U64)),
    );
    vec_fields.insert(
        "allocator".to_string(),
        interner.intern(ArType::Named(zst_id, vec![])),
    );

    let mut vec_indices = FxHashMap::<String, usize>::default();
    vec_indices.insert("data".to_string(), 0);
    vec_indices.insert("len".to_string(), 1);
    vec_indices.insert("capacity".to_string(), 2);
    vec_indices.insert("allocator".to_string(), 3);

    provider.fields.insert(vec_id, vec_fields);
    provider.field_indices.insert(vec_id, vec_indices);

    let vec_ty = ArType::Named(vec_id, vec![]);
    let vec_tid = interner.intern(vec_ty);
    let vec_layout = engine.layout_of(vec_tid, &interner, &provider);

    // 3 u64 fields + 1 ZST field = 3 * 8 = 24 bytes
    assert_eq!(vec_layout.size, 24);
    assert_eq!(vec_layout.align, 8);
}

#[test]
fn test_int_literal_layout() {
    let engine = LayoutEngine::new(8);
    let interner = TypeInterner::new();
    let provider = MockProvider;
    let tid = interner.intern(ArType::IntLiteral);
    let layout = engine.layout_of(tid, &interner, &provider);
    assert_eq!(layout.size, 8);
    assert_eq!(layout.align, 8);
}

#[test]
fn test_float_literal_layout() {
    let engine = LayoutEngine::new(8);
    let interner = TypeInterner::new();
    let provider = MockProvider;
    let tid = interner.intern(ArType::FloatLiteral);
    let layout = engine.layout_of(tid, &interner, &provider);
    assert_eq!(layout.size, 8);
    assert_eq!(layout.align, 8);
}

#[test]
fn test_struct_layout_and_padding() {
    let engine = LayoutEngine::new(8);
    let interner = TypeInterner::new();

    let struct_sym = SymbolId::new(0, 1234);

    let mut fields = FxHashMap::<String, TypeId>::default();
    fields.insert(
        "a".to_string(),
        interner.intern(ArType::Primitive(Primitive::U8)),
    );
    fields.insert(
        "b".to_string(),
        interner.intern(ArType::Primitive(Primitive::I32)),
    );
    fields.insert(
        "c".to_string(),
        interner.intern(ArType::Primitive(Primitive::U8)),
    );

    let mut field_indices = FxHashMap::<String, usize>::default();
    field_indices.insert("a".to_string(), 0);
    field_indices.insert("b".to_string(), 1);
    field_indices.insert("c".to_string(), 2);

    let mut fields_map = FxHashMap::<SymbolId, FxHashMap<String, TypeId>>::default();
    fields_map.insert(struct_sym, fields);

    let mut indices_map = FxHashMap::<SymbolId, FxHashMap<String, usize>>::default();
    indices_map.insert(struct_sym, field_indices);

    let provider = StructMockProvider {
        fields: fields_map,
        field_indices: indices_map,
        generic_params: FxHashMap::<SymbolId, Vec<SymbolId>>::default(),
        enum_variants: FxHashMap::<SymbolId, Vec<EnumPayloadShape>>::default(),
    };

    let struct_ty = ArType::Named(struct_sym, Vec::new());
    let struct_id = interner.intern(struct_ty);

    let layout = engine.layout_of(struct_id, &interner, &provider);
    // a: offset 0
    // b: offset 4 (due to align 4 of I32)
    // c: offset 8
    // total size: 12 (aligned to max alignment 4)
    assert_eq!(layout.align, 4);
    assert_eq!(layout.field_offsets, vec![0, 4, 8]);
    assert_eq!(layout.size, 12);
}

#[test]
fn test_struct_missing_fields_fallback() {
    let engine = LayoutEngine::new(8);
    let interner = TypeInterner::new();
    let struct_sym = SymbolId::new(0, 9999);
    let struct_ty = ArType::Named(struct_sym, Vec::new());
    let struct_id = interner.intern(struct_ty);
    let provider = MockProvider;
    let layout = engine.layout_of(struct_id, &interner, &provider);
    assert_eq!(layout.size, 0);
    assert_eq!(layout.align, 1);
    assert!(layout.field_offsets.is_empty());
}

#[test]
fn test_struct_generic_substitution() {
    let engine = LayoutEngine::new(8);
    let interner = TypeInterner::new();

    let struct_sym = SymbolId::new(0, 42);
    let param_sym = SymbolId::new(0, 1);

    let param_ty = interner.intern(ArType::Named(param_sym, vec![]));

    let mut fields = FxHashMap::<String, TypeId>::default();
    fields.insert("value".to_string(), param_ty);

    let mut field_indices = FxHashMap::<String, usize>::default();
    field_indices.insert("value".to_string(), 0);

    let mut fields_map = FxHashMap::<SymbolId, FxHashMap<String, TypeId>>::default();
    fields_map.insert(struct_sym, fields);

    let mut indices_map = FxHashMap::<SymbolId, FxHashMap<String, usize>>::default();
    indices_map.insert(struct_sym, field_indices);

    let mut generic_params = FxHashMap::<SymbolId, Vec<SymbolId>>::default();
    generic_params.insert(struct_sym, vec![param_sym]);

    let provider = StructMockProvider {
        fields: fields_map,
        field_indices: indices_map,
        generic_params,
        enum_variants: FxHashMap::<SymbolId, Vec<EnumPayloadShape>>::default(),
    };

    let concrete_int = interner.intern(ArType::Primitive(Primitive::I32));
    let struct_ty = ArType::Named(struct_sym, vec![concrete_int]);
    let struct_id = interner.intern(struct_ty);

    let layout = engine.layout_of(struct_id, &interner, &provider);
    // value field substituted to I32: size 4, align 4
    assert_eq!(layout.align, 4);
    assert_eq!(layout.field_offsets, vec![0]);
    assert_eq!(layout.size, 4);
}
