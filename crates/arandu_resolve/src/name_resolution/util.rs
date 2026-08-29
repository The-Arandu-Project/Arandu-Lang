use arandu_parser::{BinaryOp, UnaryOp};

pub(crate) fn is_type_case(name: &str) -> bool {
    name.chars().next().is_some_and(char::is_uppercase)
}

fn _keep_ops_exhaustive(_: UnaryOp, _: BinaryOp) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keep_ops_used() {
        // call the helper to avoid dead_code lint while keeping it available
        _keep_ops_exhaustive(UnaryOp::Neg, BinaryOp::Add);
    }
}
