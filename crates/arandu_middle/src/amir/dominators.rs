use super::block::BlockId;
use super::program::AmirFunc;
use super::rpo::reverse_post_order;
use crate::BitMatrix;

pub struct Dominators {
    // Maps each BlockId (by its index) to its immediate dominator (idom).
    // The entry block dominates itself, so idoms[entry] = Some(entry).
    // Unreachable blocks will have None as their idom.
    idoms: Vec<Option<BlockId>>,
    pre_order: Vec<u32>,
    post_order: Vec<u32>,
}

impl Dominators {
    #[must_use]
    pub fn new(func: &AmirFunc) -> Self {
        let n = func.blocks.len();
        let mut idoms = vec![None; n];

        if n == 0 {
            return Self {
                idoms,
                pre_order: Vec::new(),
                post_order: Vec::new(),
            };
        }

        let entry = BlockId::from_usize(0);
        idoms[entry.as_usize()] = Some(entry);

        // Step 1: Compute reverse post-order of reachable blocks.
        let full_rpo = reverse_post_order(func);
        let post_order_vec: Vec<BlockId> = full_rpo.iter().rev().copied().collect();

        // Map BlockId to its index in the post_order vector
        let mut post_order_indices = vec![0; n];
        for (idx, &block_id) in post_order_vec.iter().enumerate() {
            post_order_indices[block_id.as_usize()] = idx;
        }

        let mut rpo = full_rpo;
        if let Some(pos) = rpo.iter().position(|&b| b == entry) {
            rpo.remove(pos);
        }

        // Step 2: Iterate until convergence (Cooper-Harvey-Kennedy)
        let mut changed = true;
        while changed {
            changed = false;
            for &b in &rpo {
                // Find the first predecessor that has its dominator already set
                let mut processed_pred = None;
                for &p in func.predecessors(b) {
                    if idoms[p.as_usize()].is_some() {
                        processed_pred = Some(p);
                        break;
                    }
                }

                if let Some(mut new_idom) = processed_pred {
                    for &p in func.predecessors(b) {
                        if p != new_idom && idoms[p.as_usize()].is_some() {
                            new_idom = intersect(p, new_idom, &post_order_indices, &idoms);
                        }
                    }

                    let b_idx = b.as_usize();
                    if idoms[b_idx] != Some(new_idom) {
                        idoms[b_idx] = Some(new_idom);
                        changed = true;
                    }
                }
            }
        }

        // Step 3: Compute pre-order and post-order traversals on the dominator tree
        let mut children = vec![Vec::new(); n];
        for (v, &idom) in idoms.iter().enumerate() {
            if let Some(u) = idom
                && u.as_usize() != v
            {
                children[u.as_usize()].push(BlockId::from_usize(v));
            }
        }

        let mut pre_order = vec![0; n];
        let mut post_order = vec![0; n];
        let mut pre_counter = 0u32;
        let mut post_counter = 0u32;

        if n > 0 && idoms[0].is_some() {
            let mut stack = vec![(entry, 0)];
            pre_counter += 1;
            pre_order[entry.as_usize()] = pre_counter;

            while let Some((curr, child_idx)) = stack.pop() {
                let curr_idx = curr.as_usize();
                if child_idx < children[curr_idx].len() {
                    let next_child = children[curr_idx][child_idx];
                    stack.push((curr, child_idx + 1));

                    pre_counter += 1;
                    pre_order[next_child.as_usize()] = pre_counter;
                    stack.push((next_child, 0));
                } else {
                    post_counter += 1;
                    post_order[curr_idx] = post_counter;
                }
            }
        }

        Self {
            idoms,
            pre_order,
            post_order,
        }
    }

    /// Returns the immediate dominator of the given block, if it is reachable.
    #[must_use]
    pub fn immediate_dominator(&self, block: BlockId) -> Option<BlockId> {
        self.idoms.get(block.as_usize()).copied().flatten()
    }

    /// Returns true if block `a` dominates block `b`.
    #[must_use]
    pub fn dominates(&self, a: BlockId, b: BlockId) -> bool {
        if a == b {
            return true;
        }
        let a_idx = a.as_usize();
        let b_idx = b.as_usize();
        if a_idx >= self.idoms.len() || b_idx >= self.idoms.len() {
            return false;
        }
        if self.idoms[a_idx].is_none() || self.idoms[b_idx].is_none() {
            return false;
        }
        self.pre_order[a_idx] <= self.pre_order[b_idx]
            && self.post_order[b_idx] <= self.post_order[a_idx]
    }

    #[must_use]
    pub fn frontiers(&self, func: &AmirFunc) -> BitMatrix<BlockId, BlockId> {
        let mut frontiers =
            BitMatrix::<BlockId, BlockId>::new(func.blocks.len(), func.blocks.len());

        for block in &func.blocks {
            if func.predecessors(block.id).len() < 2 {
                continue;
            }

            let Some(block_idom) = self.immediate_dominator(block.id) else {
                continue;
            };

            for &pred in func.predecessors(block.id) {
                let mut runner = pred;
                while runner != block_idom {
                    frontiers.insert(runner, block.id);
                    let Some(next) = self.immediate_dominator(runner) else {
                        break;
                    };
                    if next == runner {
                        break;
                    }
                    runner = next;
                }
            }
        }

        frontiers
    }
}

fn intersect(
    mut b1: BlockId,
    mut b2: BlockId,
    post_order_indices: &[usize],
    idoms: &[Option<BlockId>],
) -> BlockId {
    while b1 != b2 {
        while post_order_indices[b1.as_usize()] < post_order_indices[b2.as_usize()] {
            match idoms[b1.as_usize()] {
                Some(p) => b1 = p,
                None => crate::ice::bug("dominator not set in intersection"),
            }
        }
        while post_order_indices[b2.as_usize()] < post_order_indices[b1.as_usize()] {
            match idoms[b2.as_usize()] {
                Some(p) => b2 = p,
                None => crate::ice::bug("dominator not set in intersection"),
            }
        }
    }
    b1
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SymbolId;
    use crate::amir::block::AmirBasicBlock;
    use crate::amir::stmt::{AmirStmtTable, AmirTerminator};
    use crate::layout::DenseRange;
    use crate::types::ArType;

    fn make_block(id: usize, _predecessors: &[usize], successors: &[usize]) -> AmirBasicBlock {
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
            _ => panic!("too many successors in test"),
        };
        AmirBasicBlock {
            id: BlockId::from_usize(id),
            statements: DenseRange::empty(),
            params: DenseRange::empty(),
            terminator: term,
        }
    }

    fn make_func(blocks: Vec<AmirBasicBlock>) -> AmirFunc {
        let cfg = crate::cfg::compute_cfg_edges(&blocks);
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
    fn test_simple_diamond() {
        // 0 -> 1, 2
        // 1 -> 3
        // 2 -> 3
        // 3 -> 4
        let blocks = vec![
            make_block(0, &[], &[1, 2]),
            make_block(1, &[0], &[3]),
            make_block(2, &[0], &[3]),
            make_block(3, &[1, 2], &[4]),
            make_block(4, &[3], &[]),
        ];
        let func = make_func(blocks);
        let doms = Dominators::new(&func);

        assert_eq!(
            doms.immediate_dominator(BlockId::from_usize(0)),
            Some(BlockId::from_usize(0))
        );
        assert_eq!(
            doms.immediate_dominator(BlockId::from_usize(1)),
            Some(BlockId::from_usize(0))
        );
        assert_eq!(
            doms.immediate_dominator(BlockId::from_usize(2)),
            Some(BlockId::from_usize(0))
        );
        assert_eq!(
            doms.immediate_dominator(BlockId::from_usize(3)),
            Some(BlockId::from_usize(0))
        );
        assert_eq!(
            doms.immediate_dominator(BlockId::from_usize(4)),
            Some(BlockId::from_usize(3))
        );

        assert!(doms.dominates(BlockId::from_usize(0), BlockId::from_usize(3)));
        assert!(!doms.dominates(BlockId::from_usize(1), BlockId::from_usize(3)));
        assert!(!doms.dominates(BlockId::from_usize(2), BlockId::from_usize(3)));
        assert!(doms.dominates(BlockId::from_usize(3), BlockId::from_usize(4)));
        assert!(doms.dominates(BlockId::from_usize(0), BlockId::from_usize(4)));
    }

    #[test]
    fn rpo_skips_unreachable_blocks_and_starts_at_entry() {
        let blocks = vec![
            make_block(0, &[], &[1, 2]),
            make_block(1, &[0], &[3]),
            make_block(2, &[0], &[3]),
            make_block(3, &[1, 2], &[]),
            make_block(4, &[], &[]),
        ];
        let func = make_func(blocks);
        let rpo = crate::amir::reverse_post_order(&func);

        assert_eq!(rpo.first().copied(), Some(BlockId::from_usize(0)));
        assert_eq!(rpo.len(), 4);
        assert!(!rpo.contains(&BlockId::from_usize(4)));
        assert!(rpo.contains(&BlockId::from_usize(1)));
        assert!(rpo.contains(&BlockId::from_usize(2)));
        assert_eq!(rpo.last().copied(), Some(BlockId::from_usize(3)));
    }

    #[test]
    fn test_loop_cfg() {
        // 0 -> 1
        // 1 -> 2, 5
        // 2 -> 3
        // 3 -> 4
        // 4 -> 1
        // 5 -> 6
        let blocks = vec![
            make_block(0, &[], &[1]),
            make_block(1, &[0, 4], &[2, 5]),
            make_block(2, &[1], &[3]),
            make_block(3, &[2], &[4]),
            make_block(4, &[3], &[1]),
            make_block(5, &[1], &[6]),
            make_block(6, &[5], &[]),
        ];
        let func = make_func(blocks);
        let doms = Dominators::new(&func);

        assert_eq!(
            doms.immediate_dominator(BlockId::from_usize(0)),
            Some(BlockId::from_usize(0))
        );
        assert_eq!(
            doms.immediate_dominator(BlockId::from_usize(1)),
            Some(BlockId::from_usize(0))
        );
        assert_eq!(
            doms.immediate_dominator(BlockId::from_usize(2)),
            Some(BlockId::from_usize(1))
        );
        assert_eq!(
            doms.immediate_dominator(BlockId::from_usize(3)),
            Some(BlockId::from_usize(2))
        );
        assert_eq!(
            doms.immediate_dominator(BlockId::from_usize(4)),
            Some(BlockId::from_usize(3))
        );
        assert_eq!(
            doms.immediate_dominator(BlockId::from_usize(5)),
            Some(BlockId::from_usize(1))
        );
        assert_eq!(
            doms.immediate_dominator(BlockId::from_usize(6)),
            Some(BlockId::from_usize(5))
        );
    }

    #[test]
    fn test_unreachable_block() {
        // 0 -> 1
        // 2 -> 3 (unreachable)
        let blocks = vec![
            make_block(0, &[], &[1]),
            make_block(1, &[0], &[]),
            make_block(2, &[], &[3]),
            make_block(3, &[2], &[]),
        ];
        let func = make_func(blocks);
        let doms = Dominators::new(&func);

        assert_eq!(
            doms.immediate_dominator(BlockId::from_usize(0)),
            Some(BlockId::from_usize(0))
        );
        assert_eq!(
            doms.immediate_dominator(BlockId::from_usize(1)),
            Some(BlockId::from_usize(0))
        );
        assert_eq!(doms.immediate_dominator(BlockId::from_usize(2)), None);
        assert_eq!(doms.immediate_dominator(BlockId::from_usize(3)), None);
    }
}
