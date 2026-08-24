//! Structural hashing for Salsa memo equality (RC-HASHEQ).
//!
//! Never uses full `Debug` of IR graphs — only deterministic fields (IDs,
//! spans, diagnostic codes, counts, ordered maps).

use arandu_middle::{Diagnostic, ResolutionResult, SymbolId, SymbolTable};
use arandu_parser::{ParseError, Program};
use arandu_semantics::amir::AmirProgram;
use arandu_semantics::TypeCheckResult;
use blake3::Hasher;
use std::sync::Arc;

/// Types that can be content-addressed for [`crate::db::HashEq`].
pub trait StableHash {
    fn stable_hash(&self) -> blake3::Hash;
}

fn finish(hasher: Hasher) -> blake3::Hash {
    hasher.finalize()
}

fn u32_le(n: u32) -> [u8; 4] {
    n.to_le_bytes()
}

fn u64_le(n: u64) -> [u8; 8] {
    n.to_le_bytes()
}

fn hash_str(hasher: &mut Hasher, value: &str) {
    hasher.update(&u64_le(value.len() as u64));
    hasher.update(value.as_bytes());
}

fn hash_diag(hasher: &mut Hasher, d: &Diagnostic) {
    // Discriminant name is stable across builds for the same DiagCode variant.
    hasher.update(format!("{:?}", d.code).as_bytes());
    hasher.update(&[d.severity as u8]);
    hasher.update(&u32_le(d.span.file_id));
    hasher.update(&u32_le(d.span.start));
    hasher.update(&u32_le(d.span.end));
    hasher.update(d.message.as_bytes());
}

fn hash_symbol_id(hasher: &mut Hasher, id: SymbolId) {
    hasher.update(&u32_le(id.file_id));
    hasher.update(&u32_le(id.local_id.0));
}

fn hash_symbol_table(hasher: &mut Hasher, table: &SymbolTable, include_spans: bool) {
    let mut symbols: Vec<_> = table.iter().collect();
    symbols.sort_by_key(|symbol| (symbol.id.file_id, symbol.id.local_id.0));
    hasher.update(&u64_le(symbols.len() as u64));
    for symbol in symbols {
        hash_symbol_id(hasher, symbol.id);
        hash_str(hasher, symbol.name.as_str());
        hash_str(hasher, &format!("{:?}", symbol.kind));
        if include_spans {
            hasher.update(&u32_le(symbol.span.file_id));
            hasher.update(&u32_le(symbol.span.start));
            hasher.update(&u32_le(symbol.span.end));
        }
        hasher.update(&u32_le(symbol.scope.0));
        hasher.update(&[u8::from(symbol.is_public)]);
    }
}

impl StableHash for ResolutionResult {
    fn stable_hash(&self) -> blake3::Hash {
        let mut h = Hasher::new();
        h.update(&[u8::from(self.is_cycle_fallback)]);
        hash_symbol_table(&mut h, &self.symbols, true);
        h.update(&u64_le(self.diagnostics.len() as u64));
        for d in &self.diagnostics {
            hash_diag(&mut h, d);
        }
        let mut defs: Vec<_> = self.resolved.definitions.iter().collect();
        defs.sort_by_key(|(k, _)| (k.start, k.end));
        for (k, id) in defs {
            h.update(&u32_le(k.start));
            h.update(&u32_le(k.end));
            hash_symbol_id(&mut h, *id);
        }
        let mut value_refs: Vec<_> = self.resolved.value_refs.iter().collect();
        value_refs.sort_by_key(|(key, _)| (key.start, key.end));
        for (key, id) in value_refs {
            h.update(&u32_le(key.start));
            h.update(&u32_le(key.end));
            hash_symbol_id(&mut h, *id);
        }
        let mut type_refs: Vec<_> = self.resolved.type_refs.iter().collect();
        type_refs.sort_by_key(|(key, _)| (key.start, key.end));
        for (key, id) in type_refs {
            h.update(&u32_le(key.start));
            h.update(&u32_le(key.end));
            hash_symbol_id(&mut h, *id);
        }
        h.update(&u64_le(self.resolved.expr_symbols.len() as u64));
        for slot in &self.resolved.expr_symbols {
            match slot {
                Some(id) => {
                    h.update(&[1]);
                    hash_symbol_id(&mut h, *id);
                }
                None => {
                    h.update(&[0]);
                }
            }
        }
        let mut mutable: Vec<_> = self.resolved.mutable_symbols.iter().copied().collect();
        mutable.sort_by_key(|id| (id.file_id, id.local_id.0));
        for id in mutable {
            hash_symbol_id(&mut h, id);
        }
        let mut docs: Vec<_> = self.docs.iter().collect();
        docs.sort_by_key(|(key, _)| (key.start, key.end));
        for (key, lines) in docs {
            h.update(&u32_le(key.start));
            h.update(&u32_le(key.end));
            for line in lines {
                hash_str(&mut h, line);
            }
        }
        finish(h)
    }
}

impl StableHash for TypeCheckResult {
    fn stable_hash(&self) -> blake3::Hash {
        hash_type_check_result(self, true)
    }
}

pub(crate) fn type_signature_hash(result: &TypeCheckResult) -> blake3::Hash {
    hash_type_check_result(result, false)
}

fn hash_type_check_result(result: &TypeCheckResult, include_spans: bool) -> blake3::Hash {
    let mut h = Hasher::new();
    hash_symbol_table(&mut h, &result.symbols, include_spans);
    h.update(&u64_le(result.diagnostics.len() as u64));
    for d in &result.diagnostics {
        hash_diag(&mut h, d);
    }
    if include_spans {
        h.update(&u64_le(result.type_info.expr_types.len() as u64));
        for slot in &result.type_info.expr_types {
            match slot {
                Some(tid) => {
                    h.update(&[1]);
                    let ty = result.type_info.type_interner.resolve(*tid);
                    hash_str(
                        &mut h,
                        &ty.display(&result.symbols, &result.type_info.type_interner),
                    );
                }
                None => {
                    h.update(&[0]);
                }
            }
        }
    }
    h.update(&u64_le(result.type_info.decl_types.len() as u64));
    let mut decls: Vec<_> = result.type_info.decl_types.iter().collect();
    decls.sort_by_key(|(id, _)| (id.file_id, id.local_id.0));
    for (sid, tid) in decls {
        hash_symbol_id(&mut h, *sid);
        let ty = result.type_info.type_interner.resolve(*tid);
        hash_str(
            &mut h,
            &ty.display(&result.symbols, &result.type_info.type_interner),
        );
    }
    finish(h)
}

impl StableHash for Result<Program, ParseError> {
    fn stable_hash(&self) -> blake3::Hash {
        match self {
            Ok(program) => hash_program(program),
            Err(err) => hash_parse_err(err),
        }
    }
}

impl StableHash for Result<std::sync::Arc<Program>, ParseError> {
    fn stable_hash(&self) -> blake3::Hash {
        match self {
            Ok(program) => hash_program(program),
            Err(err) => hash_parse_err(err),
        }
    }
}

fn hash_program(program: &Program) -> blake3::Hash {
    let mut h = Hasher::new();
    h.update(b"Program/v2");
    h.update(&u32_le(program.span.file_id));
    // The old count-only hash treated equal-shaped programs as equal even when
    // names, literals or operators changed. The canonical AST dump covers every
    // semantic node without relying on unstable `Debug` output.
    h.update(program.dump("").as_bytes());
    // `Program::dump` deliberately presents executable syntax and omits doc
    // attachments, but docs are observable by resolve/IDE queries.
    for doc in &program.docs {
        h.update(&u32_le(doc.span.start));
        h.update(&u32_le(doc.span.end));
        hash_str(&mut h, doc.text.as_str());
        h.update(&u32_le(doc.target_span.start));
        h.update(&u32_le(doc.target_span.end));
    }
    finish(h)
}

fn hash_parse_err(err: &ParseError) -> blake3::Hash {
    let mut h = Hasher::new();
    h.update(&[0]);
    h.update(format!("{:?}", err.code).as_bytes());
    h.update(&u32_le(err.span.start));
    h.update(&u32_le(err.span.end));
    h.update(err.message.as_bytes());
    finish(h)
}

impl StableHash for AmirProgram {
    fn stable_hash(&self) -> blake3::Hash {
        let mut h = Hasher::new();
        h.update(&u64_le(self.funcs.len() as u64));
        for f in &self.funcs {
            hash_symbol_id(&mut h, f.symbol);
            h.update(&u64_le(f.blocks.len() as u64));
            h.update(&u64_le(f.locals.len() as u64));
            h.update(&u64_le(f.temps.len() as u64));
        }
        h.update(&u64_le(self.literal_pool.entries.len() as u64));
        finish(h)
    }
}

impl StableHash for crate::passes::LowerAmirArtifacts {
    fn stable_hash(&self) -> blake3::Hash {
        let mut h = Hasher::new();
        h.update(self.amir.stable_hash().as_bytes());
        h.update(self.type_check.stable_hash().as_bytes());
        finish(h)
    }
}

impl StableHash for petgraph::Graph<u32, ()> {
    fn stable_hash(&self) -> blake3::Hash {
        let mut h = Hasher::new();
        h.update(&u64_le(self.node_count() as u64));
        h.update(&u64_le(self.edge_count() as u64));
        let mut nodes: Vec<u32> = self.node_weights().copied().collect();
        nodes.sort_unstable();
        for n in nodes {
            h.update(&u32_le(n));
        }
        finish(h)
    }
}

impl StableHash for arandu_middle::amir::AmirFunc {
    fn stable_hash(&self) -> blake3::Hash {
        let mut h = Hasher::new();
        hash_symbol_id(&mut h, self.symbol);
        h.update(&u64_le(self.blocks.len() as u64));
        h.update(&u64_le(self.locals.len() as u64));
        h.update(&u64_le(self.temps.len() as u64));
        h.update(&u64_le(self.stmts.payloads.len() as u64));
        for b in &self.blocks {
            h.update(&u32_le(b.id.as_usize() as u32));
            h.update(&u32_le(b.statements.start));
            h.update(&u32_le(b.statements.len));
            // Terminator discriminant for structural early cutoff.
            h.update(&[match &b.terminator {
                arandu_middle::amir::AmirTerminator::Return => 0u8,
                arandu_middle::amir::AmirTerminator::Unreachable => 1,
                arandu_middle::amir::AmirTerminator::Goto { .. } => 2,
                arandu_middle::amir::AmirTerminator::Branch { .. } => 3,
                arandu_middle::amir::AmirTerminator::SwitchInt { .. } => 4,
                arandu_middle::amir::AmirTerminator::Suspend { .. } => 5,
            }]);
        }
        // Hash stmt kinds in order (cheap structural body fingerprint).
        for kind in self.stmts.kinds.raw.iter() {
            h.update(&[(*kind) as u8]);
        }
        finish(h)
    }
}

impl StableHash for crate::dataflow::DataflowFacts {
    fn stable_hash(&self) -> blake3::Hash {
        let mut h = Hasher::new();
        h.update(b"DataflowFacts/v1");
        h.update(&u32_le(self.block.as_usize() as u32));
        h.update(&u32_le(self.live_in_count));
        h.update(&u32_le(self.live_out_count));
        h.update(&u32_le(self.init_in_count));
        h.update(&u32_le(self.moved_in_count));
        h.update(&u32_le(self.stmt_count));
        finish(h)
    }
}

impl StableHash for crate::dataflow::BorrowFacts {
    fn stable_hash(&self) -> blake3::Hash {
        let mut h = Hasher::new();
        h.update(b"BorrowFacts/v2");
        h.update(&u32_le(self.block.as_usize() as u32));
        h.update(&u32_le(self.shared_in_count));
        h.update(&u32_le(self.exclusive_in_count));
        h.update(&u32_le(self.borrow_sites));
        h.update(&u32_le(self.shared_out_count));
        h.update(&u32_le(self.exclusive_out_count));
        finish(h)
    }
}

impl StableHash for arandu_mir::borrow_facts::BlockBorrowSummary {
    fn stable_hash(&self) -> blake3::Hash {
        let mut h = Hasher::new();
        h.update(b"BlockBorrowSummary/v2");
        h.update(&u32_le(self.shared_in));
        h.update(&u32_le(self.exclusive_in));
        h.update(&u32_le(self.borrow_sites));
        h.update(&u32_le(self.shared_out));
        h.update(&u32_le(self.exclusive_out));
        finish(h)
    }
}

impl StableHash for Vec<arandu_mir::borrow_facts::BlockBorrowSummary> {
    fn stable_hash(&self) -> blake3::Hash {
        let mut h = Hasher::new();
        h.update(b"Vec<BlockBorrowSummary>/v1");
        h.update(&u64_le(self.len() as u64));
        for s in self {
            h.update(s.stable_hash().as_bytes());
        }
        finish(h)
    }
}

impl StableHash for crate::dataflow::LivenessMap {
    fn stable_hash(&self) -> blake3::Hash {
        let mut h = Hasher::new();
        h.update(b"LivenessMap/v1");
        h.update(&u64_le(self.live_in_counts.len() as u64));
        for &c in &self.live_in_counts {
            h.update(&u32_le(c));
        }
        for &c in &self.live_out_counts {
            h.update(&u32_le(c));
        }
        finish(h)
    }
}

impl StableHash for crate::dataflow::IdeDiagnostic {
    fn stable_hash(&self) -> blake3::Hash {
        let mut h = Hasher::new();
        h.update(self.code.as_bytes());
        h.update(&[self.severity]);
        h.update(self.message.as_bytes());
        h.update(&u32_le(self.file_id));
        h.update(&u32_le(self.start));
        h.update(&u32_le(self.end));
        h.update(&u64_le(self.labels.len() as u64));
        for label in &self.labels {
            h.update(&u32_le(label.file_id));
            h.update(&u32_le(label.start));
            h.update(&u32_le(label.end));
            hash_str(&mut h, &label.message);
        }
        h.update(&u64_le(self.notes.len() as u64));
        for note in &self.notes {
            hash_str(&mut h, note);
        }
        h.update(&u64_le(self.hints.len() as u64));
        for hint in &self.hints {
            hash_str(&mut h, &hint.message);
            if let Some(replacement) = &hint.replacement {
                h.update(&[1]);
                h.update(&u32_le(replacement.file_id));
                h.update(&u32_le(replacement.start));
                h.update(&u32_le(replacement.end));
                hash_str(&mut h, &replacement.new_text);
            } else {
                h.update(&[0]);
            }
        }
        if let Some(f) = self.func {
            h.update(&[1]);
            hash_symbol_id(&mut h, f);
        } else {
            h.update(&[0]);
        }
        if let Some(b) = self.block {
            h.update(&[1]);
            h.update(&u32_le(b.as_usize() as u32));
        } else {
            h.update(&[0]);
        }
        finish(h)
    }
}

impl StableHash for Vec<crate::dataflow::IdeDiagnostic> {
    fn stable_hash(&self) -> blake3::Hash {
        let mut h = Hasher::new();
        h.update(&u64_le(self.len() as u64));
        for d in self {
            h.update(d.stable_hash().as_bytes());
        }
        finish(h)
    }
}

impl StableHash for Vec<arandu_middle::SymbolId> {
    fn stable_hash(&self) -> blake3::Hash {
        let mut h = Hasher::new();
        h.update(&u64_le(self.len() as u64));
        for id in self {
            hash_symbol_id(&mut h, *id);
        }
        finish(h)
    }
}

impl StableHash for crate::passes::ItemSourceInput {
    fn stable_hash(&self) -> blake3::Hash {
        // Content-address only this item's source fingerprint — not the whole Program.
        let mut h = Hasher::new();
        h.update(b"ItemSourceInput/v2");
        hash_symbol_id(&mut h, self.item_sym);
        h.update(self.body_fp.as_bytes());
        finish(h)
    }
}

impl StableHash for Arc<[crate::highlight::HlToken]> {
    fn stable_hash(&self) -> blake3::Hash {
        let mut h = Hasher::new();
        h.update(b"HlTokenSlice/v1");
        h.update(&u64_le(self.len() as u64));
        for t in self.iter() {
            h.update(&u32_le(t.start));
            h.update(&u32_le(t.end));
            h.update(&[t.kind as u8]);
            h.update(&u32_le(u32::from(t.mods)));
        }
        finish(h)
    }
}

impl StableHash for arandu_parser::SyntaxTree {
    fn stable_hash(&self) -> blake3::Hash {
        let mut h = Hasher::new();
        h.update(b"SyntaxTree/v2");
        h.update(self.text().as_bytes());
        // Hash ranges (no per-item String alloc).
        let ranges = self.item_ranges();
        h.update(&u64_le(ranges.len() as u64));
        let text = self.text();
        let bytes = text.as_bytes();
        for (s, e) in ranges {
            h.update(&u32_le(s));
            h.update(&u32_le(e));
            let s = (s as usize).min(bytes.len());
            let e = (e as usize).min(bytes.len()).max(s);
            h.update(&bytes[s..e]);
        }
        finish(h)
    }
}

#[cfg(test)]
mod tests {
    use super::StableHash;

    #[test]
    fn program_hash_changes_when_only_a_literal_changes() {
        let first = arandu_parser::parse("func answer(): int { return 1 }").unwrap();
        let second = arandu_parser::parse("func answer(): int { return 2 }").unwrap();
        assert_ne!(Ok(first).stable_hash(), Ok(second).stable_hash());
    }
}
