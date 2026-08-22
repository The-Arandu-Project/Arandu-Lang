# Arandu Language Support

This extension adds support for the **Arandu** programming language to VS Code.

## Features

- **Syntax Highlighting**: Basic offline highlighting via TextMate grammar and full, precise syntax coloring via LSP Semantic Tokens.
- **Auto-completion**: Smart suggestions for language keywords, module paths, and symbols.
- **Signature Help**: Inline parameter help for functions and methods.
- **Go to Definition**: Quickly navigate to the definition of types, functions, and variables.
- **Diagnostics**: Real-time error and warning reporting directly in the editor.
- **Document Formatting**: Canonical formatting with minimal edits. Manual
  formatting is available immediately; format-on-save is opt-in.

## Requirements

This extension requires the `arandu-lsp` language server binary to be compiled on your system.
To compile it from the root of the repository:
```bash
cargo build -p arandu_lsp
```

## Running & Developing Locally

To load and run this extension locally for testing or development:
1. Open the `editors/vscode` directory in VS Code.
2. Run `npm install` and `npm run compile` to build the TypeScript code (or use the helper script `./build.sh`).
3. Press `F5` (or go to **Run and Debug** -> select **Launch Extension**). This opens a new "Extension Development Host" VS Code window.
4. In the new window, open any folder containing Arandu files (e.g., `examples/stable/syntax`).
5. Open any `.aru` file. The extension will automatically locate the compiled `arandu-lsp` binary from your `target/debug/` directory and activate.

## Configuration

You can customize the extension via your VS Code Settings:

* `arandu.server.path`: Absolute path to the `arandu-lsp` executable. If null, the extension will automatically look up the binary under your workspace's `target/debug/arandu-lsp` or under the global `PATH`.
* `arandu.trace.server`: Log detail level for tracing communication between VS Code and the server (`off`, `messages`, or `verbose`).

Formatting on save is intentionally disabled by default. Enable it only for
Arandu files with:

```json
"[arandu]": {
    "editor.defaultFormatter": "arandu.arandu-lang",
    "editor.formatOnSave": true
}
```

## Troubleshooting

The Arandu status item reports whether the language server is starting,
indexing, ready, restarting, missing, or stopped. Workspace indexing is also
reported through VS Code's native progress UI. Select the status item, or run
**Arandu: Show Language Server Logs**, to open the server log.

Transient crashes are restarted automatically up to three times in a rolling
three-minute window. After a repeated crash loop, automatic recovery stops so
it cannot consume resources indefinitely. Inspect the log and choose **Restart
Server**, or run **Arandu: Restart Language Server**, when the underlying issue
has been corrected.
