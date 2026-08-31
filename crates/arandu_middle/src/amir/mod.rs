pub mod block;
pub mod dominators;
pub mod local;
pub mod pretty;
pub mod program;
pub mod reachability;
pub mod rpo;
pub mod stmt;
pub mod value;
pub mod visit;

pub use block::{AmirBasicBlock, BlockId, BlockParam};
pub use dominators::Dominators;
pub use local::{AmirLocal, AmirReceiver, AmirTemp, LocalId, TempId};
pub use program::{AmirFunc, AmirProgram};
pub use reachability::reachable_blocks_dense;
pub use rpo::reverse_post_order;
pub use stmt::{
    AmirStmt, AmirStmtKind, AmirStmtTable, AmirTerminator, CallBorrowDependency, InstrId,
};
pub use value::{AmirConstant, AmirOperand, AmirPlace, AmirProjection, AmirRvalue, GenArenaDomain};
pub use visit::{
    for_each_place_operand, for_each_rvalue_operand, for_each_rvalue_place,
    for_each_terminator_operand,
};
