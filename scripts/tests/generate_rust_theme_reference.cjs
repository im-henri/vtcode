// Regenerate with the tokenizer versions pinned by VS Code:
// npm install --prefix scratch/theme-parity --ignore-scripts --no-audit --no-fund vscode-textmate@9.3.2 vscode-oniguruma@1.7.0
// node scripts/tests/generate_rust_theme_reference.cjs scratch/theme-parity/node_modules
const fs = require('node:fs');
const path = require('node:path');
const root = path.resolve(__dirname, '../..');
const modules = path.resolve(process.argv[2] || path.join(root, 'scratch/theme-parity/node_modules'));
const tm = require(path.join(modules, 'vscode-textmate'));
const onig = require(path.join(modules, 'vscode-oniguruma'));

// Theme files are JSONC. Ignore comments and trailing commas outside strings.
function jsonc(text) {
  let result = '', string = false;
  for (let i = 0; i < text.length; i++) {
    const c = text[i];
    if (string) {
      result += c;
      if (c === '\\') result += text[++i];
      else if (c === '"') string = false;
    } else if (c === '"') { string = true; result += c; }
    else if (c === '/' && text[i + 1] === '/') {
      while (i < text.length && text[i] !== '\n') i++;
      result += '\n';
    } else if (c === '/' && text[i + 1] === '*') {
      i += 2;
      while (i < text.length && !(text[i] === '*' && text[i + 1] === '/')) i++;
      i++;
    } else result += c;
  }
  let clean = '';
  string = false;
  for (let i = 0; i < result.length; i++) {
    const c = result[i];
    if (string) {
      clean += c;
      if (c === '\\') clean += result[++i];
      else if (c === '"') string = false;
    } else if (c === '"') { string = true; clean += c; }
    else if (c !== ',' || !/^\s*[}\]]/.test(result.slice(i + 1))) clean += c;
  }
  return JSON.parse(clean);
}

function theme(file) {
  const current = jsonc(fs.readFileSync(path.join(root, 'themes', file), 'utf8'));
  const base = current.include ? theme(current.include) : { colors: {}, tokenColors: [] };
  return { colors: { ...base.colors, ...current.colors }, tokenColors: [...base.tokenColors, ...current.tokenColors] };
}

if (require.main === module) (async () => {
  const wasm = fs.readFileSync(path.join(modules, 'vscode-oniguruma/release/onig.wasm'));
  await onig.loadWASM(wasm.buffer.slice(wasm.byteOffset, wasm.byteOffset + wasm.byteLength));
  const source = fs.readFileSync(path.join(root, 'src/testdata/rust_highlighting.rs'), 'utf8');
  const lines = source.trimEnd().split(/\r?\n/);
  const grammarPath = path.join(root, 'grammars/Rust/syntaxes/rust.tmLanguage.json');
  const themes = [];
  for (const [key, file] of [['monokai', 'monokai-color-theme.json'], ['dark', 'dark_plus.json'], ['light', 'light_plus.json'], ['high-contrast', 'hc_black.json']]) {
    const resolved = theme(file);
    const registry = new tm.Registry({
      theme: { name: key, settings: [{ settings: { foreground: resolved.colors['editor.foreground'], background: resolved.colors['editor.background'] } }, ...resolved.tokenColors] },
      onigLib: Promise.resolve({ createOnigScanner: patterns => new onig.OnigScanner(patterns), createOnigString: text => new onig.OnigString(text) }),
      loadGrammar: async scope => scope === 'source.rust' ? tm.parseRawGrammar(fs.readFileSync(grammarPath, 'utf8'), grammarPath) : null,
    });
    const grammar = await registry.loadGrammar('source.rust');
    let state = tm.INITIAL;
    const result = [];
    for (const line of lines) {
      const tokens = grammar.tokenizeLine2(line, state);
      state = tokens.ruleStack;
      const colors = registry.getColorMap();
      const runs = [];
      for (let i = 0; i < tokens.tokens.length; i += 2) {
        const start = tokens.tokens[i], metadata = tokens.tokens[i + 1];
        if (start >= line.length) continue;
        runs.push([Buffer.byteLength(line.slice(0, start)), colors[(metadata >>> 15) & 0x1ff], (metadata >>> 11) & 0xf]);
      }
      result.push(runs);
    }
    themes.push({ key, lines: result });
    registry.dispose();
  }
  const target = path.join(root, 'src/testdata/rust_theme_reference.json');
  const output = '{\n  "generator": "VS Code vscode-textmate 9.3.2 / vscode-oniguruma 1.7.0",\n  "themes": [\n' + themes.map(t => '    {"key":' + JSON.stringify(t.key) + ',"lines":[\n' + t.lines.map(l => '      ' + JSON.stringify(l)).join(',\n') + '\n    ]}').join(',\n') + '\n  ]\n}\n';
  fs.writeFileSync(target, output);
  console.log(`${themes.length} themes, ${lines.length} Rust lines, ${themes.reduce((n, t) => n + t.lines.reduce((n, l) => n + l.length, 0), 0)} reference runs`);
})().catch(error => { console.error(error); process.exitCode = 1; });

module.exports = { jsonc, theme };
