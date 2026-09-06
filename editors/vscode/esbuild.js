const esbuild = require('esbuild');

const production = process.argv.includes('--production');
const watch = process.argv.includes('--watch');

async function main() {
    const context = await esbuild.context({
        entryPoints: ['src/extension.ts'],
        bundle: true,
        external: ['vscode'],
        format: 'cjs',
        logLevel: 'info',
        minify: production,
        outfile: 'out/extension.js',
        platform: 'node',
        sourcemap: !production,
        sourcesContent: false,
        target: 'node20'
    });

    if (watch) {
        await context.watch();
        return;
    }

    await context.rebuild();
    await context.dispose();
}

void main().catch((error) => {
    console.error(error);
    process.exitCode = 1;
});
