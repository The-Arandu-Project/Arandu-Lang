use super::build::SyntaxTree;
use super::kind::SyntaxToken;
use rowan::NodeOrToken;

/// Highlight spans for LSP semantic tokens: `(start, end, class)`.
#[must_use]
pub fn highlight_spans(tree: &SyntaxTree) -> Vec<(u32, u32, &'static str)> {
    let mut out = Vec::new();
    for_each_highlight_token(tree, |tok, class| {
        let r = tok.text_range();
        out.push((u32::from(r.start()), u32::from(r.end()), class));
    });
    out
}

/// Iterate tokens for semantic highlighting.
pub fn for_each_highlight_token(tree: &SyntaxTree, mut f: impl FnMut(SyntaxToken, &'static str)) {
    let root = tree.root();
    for event in root.preorder_with_tokens() {
        let rowan::WalkEvent::Enter(el) = event else {
            continue;
        };
        let NodeOrToken::Token(tok) = el else {
            continue;
        };
        if let Some(class) = tok.kind().highlight_class() {
            f(tok, class);
        }
    }
}
