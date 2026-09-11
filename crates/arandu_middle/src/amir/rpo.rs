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
    let mut stack = vec![(entry, 0usize)];
    visited[0] = true;
    // Keep each suspended DFS frame explicit: CFG depth must not consume the
    // host thread's call stack. Advance successors in their original order.
    while let Some((block, next_successor)) = stack.last_mut() {
        if let Some(&successor) = func.successors(*block).get(*next_successor) {
            *next_successor += 1;
            let index = successor.as_usize();
            if index < n && !visited[index] {
                visited[index] = true;
                stack.push((successor, 0));
            }
        } else {
            post_order.push(*block);
            stack.pop();
        }
    }
    post_order.reverse();
    post_order
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

    #[test]
    fn rpo_preserves_dfs_order_with_cross_edges_and_backedges() {
        let func = make_func(vec![
            make_block(0, &[1, 2]),
            make_block(1, &[2, 3]),
            make_block(2, &[1, 3]),
            make_block(3, &[]),
            make_block(4, &[]),
        ]);
        assert_eq!(
            reverse_post_order(&func),
            (0..4).map(BlockId::from_usize).collect::<Vec<_>>()
        );
    }

    #[test]
    fn rpo_deep_chain_does_not_depend_on_thread_stack_size() {
        let count = 16_384;
        let func = make_func(
            (0..count)
                .map(|id| {
                    if id + 1 < count {
                        make_block(id, &[id + 1])
                    } else {
                        make_block(id, &[])
                    }
                })
                .collect(),
        );
        std::thread::Builder::new()
            .stack_size(128 * 1024)
            .spawn(move || {
                let order = reverse_post_order(&func);
                assert_eq!(order.len(), count);
                for (id, block) in order.into_iter().enumerate() {
                    assert_eq!(block, BlockId::from_usize(id));
                }
            })
            .unwrap()
            .join()
            .unwrap();
    }

    /// Manual pass benchmark; assertions above own correctness, not wall time.
    #[test]
    #[ignore = "informational CFG traversal benchmark"]
    fn rpo_cfg_workload_measurement() {
        use std::hint::black_box;
        use std::time::Instant;

        for branching in [false, true] {
            let count = 256;
            let func = make_func(
                (0..count)
                    .map(|id| {
                        if branching && id + 2 < count {
                            make_block(id, &[id + 1, id + 2])
                        } else if id + 1 < count {
                            make_block(id, &[id + 1])
                        } else {
                            make_block(id, &[])
                        }
                    })
                    .collect(),
            );
            for _ in 0..100 {
                black_box(reverse_post_order(black_box(&func)));
            }
            let mut samples = Vec::new();
            for _ in 0..7 {
                let start = Instant::now();
                for _ in 0..2_000 {
                    black_box(reverse_post_order(black_box(&func)));
                }
                samples.push(start.elapsed());
            }
            samples.sort();
            println!(
                "RPO: blocks={count} branching={branching} rounds=2000 median={:?}",
                samples[3]
            );
        }
    }
}
