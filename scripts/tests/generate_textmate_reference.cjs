// Usage: node scripts/tests/generate_textmate_reference.cjs NODE_MODULES [GRAMMARS]
// Reference dependencies: vscode-textmate@9.3.2 and vscode-oniguruma@1.7.0.
// GRAMMARS defaults to the repository grammars/bundled catalog.
const fs = require('node:fs');
const path = require('node:path');
const zlib = require('node:zlib');
const crypto = require('node:crypto');
const {theme} = require('./generate_rust_theme_reference.cjs');
const root = path.resolve(__dirname, '../..');
const modules = path.resolve(process.argv[2]);
const assets = path.resolve(process.argv[3] || path.join(root, 'grammars/bundled'));
const tm = require(path.join(modules, 'vscode-textmate'));
const onig = require(path.join(modules, 'vscode-oniguruma'));
const sha = bytes => crypto.createHash('sha256').update(bytes).digest('hex');

(async () => {
  const wasm = fs.readFileSync(path.join(modules, 'vscode-oniguruma/release/onig.wasm'));
  await onig.loadWASM(wasm.buffer.slice(wasm.byteOffset, wasm.byteOffset + wasm.byteLength));
  const index = JSON.parse(fs.readFileSync(path.join(assets, 'index.json'), 'utf8'));
  const grammars = new Map();
  for (const id of Object.keys(index.grammars).sort()) {
    grammars.set(id, JSON.parse(fs.readFileSync(path.join(assets, `${id}.json`), 'utf8')));
  }
  grammars.set('rust', JSON.parse(fs.readFileSync(path.join(root, 'grammars/Rust/syntaxes/rust.tmLanguage.json'), 'utf8')));
  grammars.set('python', JSON.parse(fs.readFileSync(path.join(root, 'grammars/Python/MagicPython.tmLanguage.json'), 'utf8')));
  // The VS Code Python package registers this helper scope as a separate grammar.
  const pythonRegex = JSON.parse(fs.readFileSync(path.join(root, 'grammars/Python/MagicRegExp.tmLanguage.json'), 'utf8'));
  const pythonRegexId = [...grammars].find(([,g]) => g.scopeName === pythonRegex.scopeName)?.[0] || pythonRegex.scopeName;
  grammars.set(pythonRegexId, pythonRegex);
  for (const id of ['git-commit','git-rebase','ignore']) {
    grammars.set(id,JSON.parse(fs.readFileSync(path.join(root,`grammars/Git/${id}.tmLanguage.json`),'utf8')));
  }
  const byScope = new Map();
  const injections = new Map();
  for (const [id, grammar] of grammars) {
    if (!byScope.has(grammar.scopeName)) byScope.set(grammar.scopeName, grammar);
    for (const scope of index.grammars[id]?.injectTo || grammar.injectTo || []) {
      if (!injections.has(scope)) injections.set(scope, []);
      injections.get(scope).push(grammar.scopeName);
    }
  }
  const source = fs.readFileSync(path.join(root, 'src/testdata/theme_scope_corpus.txt'), 'utf8');
  const lines = source.trimEnd().split(/\r?\n/);
  const output = {generator:'vscode-textmate 9.3.2 / vscode-oniguruma 1.7.0', corpusSha256:sha(source), languages:[]};
  const files = [['monokai','monokai-color-theme.json'],['dark','dark_plus.json'],['light','light_plus.json'],['high-contrast','hc_black.json']];
  for (const [id, raw] of grammars) {
    const entry = {id,scope:raw.scopeName,themes:[]};
    for (const [key,file] of files) {
      const resolved = theme(file);
      const registry = new tm.Registry({
        theme:{settings:[{settings:{foreground:resolved.colors['editor.foreground'],background:resolved.colors['editor.background']}},...resolved.tokenColors.filter(r=>r.scope)]},
        onigLib:Promise.resolve({createOnigScanner:patterns=>new onig.OnigScanner(patterns),createOnigString:text=>new onig.OnigString(text)}),
        loadGrammar:async scope=>{
          const grammar=scope===raw.scopeName ? raw : byScope.get(scope);
          return grammar ? JSON.parse(JSON.stringify(grammar)) : null;
        },
        getInjections:scope=>injections.get(scope) || [],
      });
      // Capture dependencies are not discovered by vscode-textmate's automatic
      // dependency walker. Register the complete catalog, as the native resolver
      // does, so embedded scopes are independent of which files were opened first.
      for (const grammar of byScope.values()) {
        registry._syncRegistry.addGrammar(JSON.parse(JSON.stringify(grammar)), injections.get(grammar.scopeName));
      }
      registry._syncRegistry.addGrammar(JSON.parse(JSON.stringify(raw)), injections.get(raw.scopeName));
      const grammar = await registry.loadGrammar(raw.scopeName);
      let state=tm.INITIAL;
      const result=[];
      for (const line of lines) {
        const tokenized=grammar.tokenizeLine2(line,state);
        state=tokenized.ruleStack;
        const colors=registry.getColorMap();
        const runs=[];
        for(let i=0;i<tokenized.tokens.length;i+=2) {
          const start=tokenized.tokens[i], metadata=tokenized.tokens[i+1];
          if(start>=line.length)continue;
          runs.push([Buffer.byteLength(line.slice(0,start)),colors[(metadata>>>15)&0x1ff],(metadata>>>11)&0xf]);
        }
        result.push(runs);
      }
      entry.themes.push({key,lines:result});
      registry.dispose();
    }
    output.languages.push(entry);
    if(output.languages.length%30===0)console.log(`Compared ${output.languages.length} grammars`);
  }
  const target=path.join(root,'src/testdata/textmate_reference.json.gz');
  fs.writeFileSync(target,zlib.gzipSync(JSON.stringify(output),{level:9}));
  console.log(`${output.languages.length} grammars × ${files.length} themes × ${lines.length} lines; ${fs.statSync(target).size} bytes`);
})().catch(error=>{console.error(error);process.exitCode=1;});
