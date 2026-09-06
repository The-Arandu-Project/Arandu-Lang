use super::{AmirFunc, BlockId};

#[must_use]
pub fn reverse_post_order(func: &AmirFunc) -> Vec<BlockId> {
    let n = func.blocks.len();
    if n == 0 {
        return Vec::new();
    }

    let entry = BlockId::from_usize(0);
    let mut visited = vec![false; n];
    let mut post_order = Vec::with_capacity(n);
    post_order_walk(entry, func, &mut visited, &mut post_order);
    post_order.into_iter().rev().collect()
}

fn post_order_walk(
    block_id: BlockId,
    func: &AmirFunc,
    visited: &mut [bool],
    post_order: &mut Vec<BlockId>,
) {
    let idx = block_id.as_usize();
    if idx >= visited.len() || visited[idx] {
        return;
    }
    visited[idx] = true;

    for &succ in func.successors(block_id) {
        post_order_walk(succ, func, visited, post_order);
    }
    post_order.push(block_id);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SymbolId;
    use crate::amir::block::AmirBasicBlock;
    use crate::amir::stmt::{AmirStmtTable, AmirTerminator};
    use crate::cfg::compute_cfg_edges;
    use crate::layout::DenseRange;
    use crate::types::ArType;

    fn make_block(id: usize, successors: &[usize]) -> AmirBasicBlock {
        let term = match successors {
            [] => AmirTerminator::Return,
            &[s] => AmirTerminator::Goto {
                target: BlockId::from_usize(s),
                args: Vec::new(),
            },
            &[t, f] => AmirTerminator::Branch {
                condition: crate::amir::AmirOperand::Constant(crate::amir::AmirConstant::Bool(
                    true,
                )),
                if_true: BlockId::from_usize(t),
                true_args: Vec::new(),
                if_false: BlockId::from_usize(f),
                false_args: Vec::new(),
            },
            _ => panic!("too many successors"),
        };
        AmirBasicBlock {
            id: BlockId::from_usize(id),
            statements: DenseRange::empty(),
            params: DenseRange::empty(),
            terminator: term,
        }
    }

    fn make_func(blocks: Vec<AmirBasicBlock>) -> AmirFunc {
        let cfg = compute_cfg_edges(&blocks);
        AmirFunc {
            symbol: SymbolId::new(0, 0),
            return_type: crate::types::TypeInterner::new().intern(ArType::Void),
            receiver: None,
            params: Vec::new(),
            locals: Vec::new(),
            temps: Vec::new(),
            blocks,
            block_params: Vec::new(),
            stmts: AmirStmtTable::new(),
            cfg,
        }
    }

    #[test]
    fn rpo_empty_func() {
        let func = make_func(vec![]);
        assert!(reverse_post_order(&func).is_empty());
    }

    #[test]
    fn rpo_single_block() {
        let func = make_func(vec![make_block(0, &[])]);
        let rpo = reverse_post_order(&func);
        assert_eq!(rpo, vec![BlockId::from_usize(0)]);
    }

    #[test]
    fn rpo_linear_chain() {
        let func = make_func(vec![
            make_block(0, &[1]),
            make_block(1, &[2]),
            make_block(2, &[]),
        ]);
        let rpo = reverse_post_order(&func);
        assert_eq!(
            rpo,
            vec![
                BlockId::from_usize(0),
                BlockId::from_usize(1),
                BlockId::from_usize(2),
            ]
        );
    }

    #[test]
    fn rpo_branch() {
        let func = make_func(vec![
            make_block(0, &[1, 2]),
            make_block(1, &[]),
            make_block(2, &[]),
        ]);
        let rpo = reverse_post_order(&func);
        assert_eq!(rpo[0], BlockId::from_usize(0));
        assert_eq!(rpo.len(), 3);
        assert!(rpo.contains(&BlockId::from_usize(1)));
        assert!(rpo.contains(&BlockId::from_usize(2)));
    }

    #[test]
    fn rpo_skips_unreachable() {
        let func = make_func(vec![
            make_block(0, &[1]),
            make_block(1, &[]),
            make_block(2, &[3]),
            make_block(3, &[]),
        ]);
        let rpo = reverse_post_order(&func);
        assert_eq!(rpo, vec![BlockId::from_usize(0), BlockId::from_usize(1)]);
    }

    #[test]
    fn rpo_loop() {
        let func = make_func(vec![
            make_block(0, &[1]),
            make_block(1, &[2]),
            make_block(2, &[1]),
        ]);
        let rpo = reverse_post_order(&func);
        assert_eq!(rpo.len(), 3);
        assert!(rpo.contains(&BlockId::from_usize(2)));
    }
}
