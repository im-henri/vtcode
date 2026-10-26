// Regenerate with Node >= 22.20 and ripgrep 15.2.0 (PCRE2 enabled):
// npm install --prefix scratch/vscode-search-reference --ignore-scripts vscode-regexpp@3.1.0
// node tests/support/generate_vscode_search_fixtures.mjs
// The downloaded Microsoft sources are cached under ignored scratch/.
import fs from 'node:fs';
import path from 'node:path';
import { createRequire, stripTypeScriptTypes } from 'node:module';
import { execFileSync } from 'node:child_process';
import vm from 'node:vm';
const revision = '959031245ebb1fe077e0d512e1397c0ae82006e4';
const cache = path.resolve('scratch/vscode-search-reference');
fs.mkdirSync(cache, { recursive: true });
const sources = {
  'textModelSearch.ts': 'src/vs/editor/common/model/textModelSearch.ts',
  'replacePattern.ts': 'src/vs/editor/contrib/find/browser/replacePattern.ts',
  'search.ts': 'src/vs/base/common/search.ts',
  'strings.ts': 'src/vs/base/common/strings.ts',
  'charCode.ts': 'src/vs/base/common/charCode.ts',
  'wordHelper.ts': 'src/vs/editor/common/core/wordHelper.ts',
  'replace.ts': 'src/vs/workbench/services/search/common/replace.ts',
  'ripgrepTextSearchEngine.ts': 'src/vs/workbench/services/search/node/ripgrepTextSearchEngine.ts',
  'glob.ts': 'src/vs/base/common/glob.ts',
  'queryBuilder.ts': 'src/vs/workbench/services/search/common/queryBuilder.ts',
};
for (const [name, source] of Object.entries(sources)) {
  const file = path.join(cache, name);
  if (!fs.existsSync(file)) {
    const response = await fetch(`https://raw.githubusercontent.com/microsoft/vscode/${revision}/${source}`);
    if (!response.ok) throw new Error(`${source}: ${response.status}`);
    fs.writeFileSync(file, await response.text());
  }
}
const read = name => fs.readFileSync(path.join(cache, name), 'utf8');
const js = source => stripTypeScriptTypes(source.replace(/^import .*;\r?\n/gm, '').replace(/^export /gm, ''), { mode: 'transform', disableExperimentalWarning: true });
const fn = (name, source) => {
  const start = source.indexOf(`function ${name}(`);
  if (start < 0) throw new Error(`Missing upstream function ${name}`);
  return source.slice(start, source.indexOf('\n}', start) + 2);
};
const CharCode = new Function(`${js(read('charCode.ts'))}; return CharCode;`)();
const stringFunctions = ['createRegExp', 'escapeRegExpCharacters', 'isHighSurrogate', 'isLowSurrogate', 'computeCodePoint', 'getNextCodePoint', 'containsUppercaseCharacter'];
const strings = new Function('CharCode', `${js(stringFunctions.map(name => fn(name, read('strings.ts'))).join('\n'))}; return {${stringFunctions.join(',')}};`)(CharCode);
const wordSeparators = new Function(`${js(read('wordHelper.ts').split('\n').find(line => line.startsWith('export const USUAL_WORD_SEPARATORS')))}; return USUAL_WORD_SEPARATORS;`)();
class Position { constructor(lineNumber, column) { Object.assign(this, { lineNumber, column }); } }
class Range {
  constructor(startLineNumber, startColumn, endLineNumber, endColumn) { Object.assign(this, { startLineNumber, startColumn, endLineNumber, endColumn }); }
  getStartPosition() { return new Position(this.startLineNumber, this.startColumn); }
  getEndPosition() { return new Position(this.endLineNumber, this.endColumn); }
}
class FindMatch { constructor(range, matches) { Object.assign(this, { range, matches }); } }
class SearchData { constructor(regex, wordSeparators, simpleSearch) { Object.assign(this, { regex, wordSeparators, simpleSearch }); } }
const getMapForWordSeparators = separators => ({ get: code => separators.includes(String.fromCharCode(code)) || code === 32 || code === 9 ? 2 : 0 });
const modelApi = new Function('CharCode', 'strings', 'Position', 'Range', 'FindMatch', 'SearchData', 'getMapForWordSeparators', 'WordCharacterClass', 'EndOfLinePreference', `${js(read('textModelSearch.ts'))}; return {TextModelSearch, SearchParams, isMultilineRegexSource};`)(CharCode, strings, Position, Range, FindMatch, SearchData, getMapForWordSeparators, { Regular: 0 }, { LF: 1 });
class Model {
  constructor(text) {
    this.original = text;
    this.lines = text.split(/\r\n|\r|\n/);
    this.eol = text.includes('\r\n') ? '\r\n' : '\n';
    this.text = this.lines.join(this.eol);
    this.offsets = [0];
    this.originalOffsets = [0];
    const matcher = /\r\n|\r|\n/g;
    let match;
    while ((match = matcher.exec(this.text))) this.offsets.push(match.index + match[0].length);
    matcher.lastIndex = 0;
    while ((match = matcher.exec(text))) this.originalOffsets.push(match.index + match[0].length);
  }
  getEOL() { return this.eol; }
  getLineCount() { return this.lines.length; }
  getLineContent(line) { return this.lines[line - 1]; }
  getLineMaxColumn(line) { return this.lines[line - 1].length + 1; }
  getFullModelRange() { return new Range(1, 1, this.lines.length, this.lines.at(-1).length + 1); }
  getOffsetAt(position) { return this.offsets[position.lineNumber - 1] + position.column - 1; }
  getPositionAt(offset) {
    let row = this.offsets.findLastIndex(start => start <= offset);
    return new Position(row + 1, Math.min(offset - this.offsets[row], this.lines[row].length) + 1);
  }
  getValueInRange(range) { return this.text.slice(this.getOffsetAt(range.getStartPosition()), this.getOffsetAt(range.getEndPosition())).replace(/\r\n|\r/g, '\n'); }
  charOffset(position) { return [...this.original.slice(0, this.originalOffsets[position.lineNumber - 1] + position.column - 1)].length; }
}
const preserveCase = new Function('strings', `${js(read('search.ts'))}; return buildReplaceStringWithCasePreserved;`)(strings);
const { ReplacePattern, parseReplaceString } = new Function('CharCode', 'buildReplaceStringWithCasePreserved', `${js(read('replacePattern.ts'))}; return {ReplacePattern, parseReplaceString};`)(CharCode, preserveCase);
const WorkspaceReplacement = new Function('CharCode', 'strings', 'buildReplaceStringWithCasePreserved', `${js(read('replace.ts'))}; return ReplacePattern;`)(CharCode, strings, preserveCase);
const texts = ['', 'ababa', 'aa b\n', 'foo\nbar\n', 'foo\r\nbar\r\n', 'FOO foo food\r\n\r\nbar', 'é É 猫\né猫', 'a\tb 12_34', '\r\n', 'éfoo fooé foo_foo .foo foo.', 'Σ σ ς S s ſ K k K İ i ı', '😀foo 🐱x', 'a\u0085b a\u00a0b a\uFEFFb a\u2028b a\u2029b', 'foo\r\n\r\nbar\n\nend', '\\n\nn\n\n\r\nfoo\r\nbar'];
const regexes = ['.', '.*', '.+?', 'a*', 'a?', 'a|', '^', '$', '^$', '^.*$', '\\b', '\\B', '\\w+', '\\W', '\\s+', '[^a]+', '\\d+', '\\r?\\n', '\\n', '\\r', 'foo\\nbar', 'foo\\s+bar', '[\\s\\S]+', '[^\\n]', '[^\\n]+', '[a\\n]+', 'foo|food', '(foo)?', '\\p{L}+', '(?:)', '(?=foo)', '(?<=f)oo', '(?<!f)oo', '(a)\\1', '(?<name>a)\\k<name>', '\\u00E9', '\\u{1F431}', '\\A', '\\z', '(?i)foo', '(?s).*', '(?P<name>a)', '*', '[', '(', '\\', 'a{3,2}'];
const literals = ['', 'a', 'aba', 'foo', 'é', 'σ', 's', 'k', 'i', '.', '-', '#', '[', 'foo\nbar'];
regexes.push('\\n+', '[\\n]+', '[\\n\\n]', '[^a\\n]', '[^a\\n]+', '[^\\n]?', '[^\\n]*', '[\\\\n]\\n', '[a-z\\n]+', '(?<=foo\\n)bar', '(?<!foo\\n)bar', '(?=(\\n))', '(?<=foo)\\nbar', '\\n{2}', '[\\n]{2}');
const editor = [];
for (const text of texts) for (const [regex, queries] of [[true, regexes], [false, literals]]) for (const query of queries) for (const caseSensitive of [false, true]) for (const wholeWord of [false, true]) {
  const model = new Model(text);
  const params = new modelApi.SearchParams(query, regex, caseSensitive, wholeWord ? wordSeparators : null);
  const matches = modelApi.TextModelSearch.findMatches(model, params, model.getFullModelRange(), false, 19999);
  editor.push({ text, query, regex, caseSensitive, wholeWord, invalid: query !== '' && !params.parseSearchRequest(), matches: matches.map(match => [model.charOffset(match.range.getStartPosition()), model.charOffset(match.range.getEndPosition())]) });
}
for (const query of ['foo', '^foo', 'foo$', '(?<=x)foo', '\\bfoo', 'aba', 'foo\\nbar']) for (const wholeWord of [false, true]) {
  const text = query === 'aba' ? 'xababa' : query.includes('\\n') ? 'xfoo\r\nbarz' : 'xfooz foo';
  const model = new Model(text);
  const range = query.includes('\\n') ? new Range(1, 2, 2, 4) : new Range(1, 2, 1, 5);
  const params = new modelApi.SearchParams(query, true, true, wholeWord ? wordSeparators : null);
  const matches = modelApi.TextModelSearch.findMatches(model, params, range, false, 19999);
  editor.push({ text, query, regex: true, caseSensitive: true, wholeWord, invalid: false, selection: [range.startLineNumber - 1, range.startColumn - 1, range.endLineNumber - 1, range.endColumn - 1], matches: matches.map(match => [model.charOffset(match.range.getStartPosition()), model.charOffset(match.range.getEndPosition())]) });
}
const require = createRequire(path.join(cache, 'package.json'));
const { RegExpParser, RegExpVisitor } = require('vscode-regexpp');
const rgSource = read('ripgrepTextSearchEngine.ts');
// Execute VS Code's byte-to-UTF-16 range mapper too. Prefix decoding alone is
// different for zero-width ripgrep matches inside a UTF-8 sequence.
const mapperStart = rgSource.indexOf('private createTextSearchMatch(');
const mapperEnd = rgSource.indexOf('\n\t\tconst searchRange =', mapperStart);
const mapperSource = rgSource.slice(mapperStart, mapperEnd).replace('private createTextSearchMatch', 'function createTextSearchMatch') + '\nreturn ranges;\n}';
const workspaceRanges = new Function('Range', 'coalesce', 'Buffer', `${js(fn('bytesOrTextToString', rgSource) + '\n' + fn('getNumLinesAndLastNewlineLength', rgSource) + '\n' + mapperSource)}; return createTextSearchMatch;`)(Range, values => values.filter(Boolean), Buffer);
const rgFunctions = ['getRgArgs', 'unicodeEscapesToPCRE2', 'fixRegexNewline', 'fixNewline'];
const getRgArgs = new Function('RegExpParser', 'RegExpVisitor', 'createRegExp', 'escapeRegExpCharacters', 'groupBy', `${js("const anchorGlob = (value: string) => value;\nconst isLookBehind = (node: any) => node.type === 'Assertion' && node.kind === 'lookbehind';\n" + rgFunctions.map(name => fn(name, rgSource)).join('\n'))}; return getRgArgs;`)(RegExpParser, RegExpVisitor, strings.createRegExp, strings.escapeRegExpCharacters, () => ({}));
const inputs = path.join(cache, 'oracle-inputs');
fs.mkdirSync(inputs, { recursive: true });
texts.forEach((text, i) => fs.writeFileSync(path.join(inputs, `${i}.txt`), text));
const workspace = [];
for (const [regex, queries] of [[true, regexes], [false, literals]]) for (const query of queries) for (const caseSensitive of [false, true]) for (const wholeWord of [false, true]) {
  let invalid = false;
  if (query) { try { strings.createRegExp(query, regex, { unicode: true, matchCase: caseSensitive }); } catch { invalid = true; } }
  const results = new Map();
  if (query && !invalid) {
    const args = getRgArgs({ pattern: query, isRegExp: regex, isCaseSensitive: caseSensitive, isWordMatch: wholeWord, isMultiline: regex ? modelApi.isMultilineRegexSource(query) : query.includes('\n') }, { folderOptions: { includes: [], excludes: [], useIgnoreFiles: { local: true, parent: false, global: false }, followSymlinks: true } });
    let output;
    try { output = execFileSync('rg', args, { cwd: inputs, encoding: 'utf8', maxBuffer: 16 * 1024 * 1024, stdio: ['ignore', 'pipe', 'pipe'] }); }
    catch (error) { if (error.status !== 1) throw error; output = error.stdout; }
    for (const line of output.trim().split('\n').filter(Boolean)) {
      const event = JSON.parse(line);
      if (event.type !== 'match') continue;
      const data = event.data;
      const index = Number(path.basename(data.path.text, '.txt'));
      const model = new Model(texts[index]);
      const ranges = results.get(index) || [];
      const scalarOffset = (row, col) => [...texts[index].slice(0, model.originalOffsets[row])].length + [...model.lines[row].slice(0, col)].length + Math.max(0, col - model.lines[row].length);
      for (const range of workspaceRanges.call({ hitLimit: false, numResults: 0, maxResults: Number.MAX_SAFE_INTEGER }, data, null)) {
        ranges.push([scalarOffset(range.startLineNumber, range.startColumn), scalarOffset(range.endLineNumber, range.endColumn)]);
      }
      results.set(index, ranges);
    }
  }
  texts.forEach((text, i) => workspace.push({ text, query, regex, caseSensitive, wholeWord, invalid, matches: results.get(i) || [] }));
}
const replacements = [];
for (const [text, query] of [['foo', '(foo)'], ['foo', '(?<name>foo)'], ['foo', '(f)(o)(o)'], ['foO', '(foO)'], ['FOO', '(FOO)'], ['foo', '(foo)(bar)?'], ['😀foo', '(😀)(foo)']]) for (const replacement of ['world', 'MiXeD', '$0', '$&', '$1', '$2', '$10', '$20', '$100', '$01', '$99', '$1-$2', '${name}', '$<name>', '$$1', '$$', '$`', "$'", '\\n', '\\t', '\\\\n', '\\r', '\\U$1', '\\l$1', '\\u\\u\\l\\l\\U$1', '\\Uhello$1', 'x\\L$1$2', '$1$<name>']) for (const preserve of [false, true]) {
  const match = new RegExp(query, 'u').exec(text);
  const editorResult = parseReplaceString(replacement).buildReplaceString(match, preserve);
  let workspaceResult;
  try {
    const pattern = new WorkspaceReplacement(replacement, { pattern: query, isRegExp: true, isCaseSensitive: true, isMultiline: false });
    workspaceResult = vm.runInNewContext('pattern.getReplaceString(text, preserve)', { pattern, text, preserve }, { timeout: 100 });
  }
  catch { workspaceResult = null; } // Do not reproduce upstream exceptions.
  replacements.push({ text, query, replacement, preserve, editorResult, workspaceResult });
}
const preserved = [];
for (const original of ['HELLO', 'hello', 'Hello', 'hElLo', 'HeLLo', '123', 'éFoo', 'Foo-Bar', 'foo_BAR', 'FOO_bar', 'Foo_Bar-Baz', '😀foo']) for (const replacement of ['world', 'MiXeD', 'new-value', 'NEW_value', 'a_b-c', '']) preserved.push({ original, replacement, expected: preserveCase([original], replacement) });
const globApi = new Function('equals', 'isThenable', 'CharCode', 'isEqualOrParent', 'LRUCache', 'basename', 'extname', 'posix', 'sep', 'isLinux', 'endsWithIgnoreCase', 'equalsIgnoreCase', 'escapeRegExpCharacters', 'ltrim', `${js(read('glob.ts'))}; return {match, splitGlobAware};`)(
  (a, b) => a.length === b.length && a.every((value, i) => value === b[i]), value => !!value?.then, CharCode,
  (child, parent) => child === parent || child.startsWith(parent + '/'), class LRUCache extends Map { constructor() { super(); } }, path.posix.basename, path.posix.extname, path.posix, '/', false,
  (text, suffix) => text.toLowerCase().endsWith(suffix.toLowerCase()), (a, b) => a.toLowerCase() === b.toLowerCase(), strings.escapeRegExpCharacters,
  (text, prefix) => { while (text.startsWith(prefix)) text = text.slice(prefix.length); return text; },
);
const querySource = read('queryBuilder.ts');
const method = name => {
  const start = querySource.indexOf(`\t${name}(`) >= 0 ? querySource.indexOf(`\t${name}(`) : querySource.indexOf(`\tprivate ${name}(`);
  if (start < 0) throw new Error(`Missing upstream method ${name}`);
  return querySource.slice(start, querySource.indexOf('\n\t}', start) + 3);
};
const rootUri = { path: '/workspace', fsPath: '/workspace', scheme: 'file' };
const QueryBuilder = new Function('path', 'collections', 'glob', 'strings', 'WorkbenchState', 'Schemas', `${js(['splitGlobPattern', 'expandGlobalGlob', 'normalizeGlobPattern', 'normalizeSlashes', 'splitGlobFromPath', 'patternListToIExpression'].map(name => fn(name, querySource)).join('\n') + '\nclass QueryBuilder {\n' + ['parseSearchPaths', 'expandSearchPathPatterns', 'expandOneSearchPath', 'resolveOneSearchPathPattern'].map(method).join('\n') + '\n}')}; return QueryBuilder;`)(
  path.posix, { groupBy: (values, key) => values.reduce((groups, value) => ((groups[key(value)] ??= []).push(value), groups), {}) }, globApi,
  { rtrim: (text, suffix) => { while (text.endsWith(suffix)) text = text.slice(0, -suffix.length); return text; } }, { FOLDER: 1 }, { file: 'file' },
);
const queryBuilder = new QueryBuilder();
queryBuilder.pathService = { resolvedUserHome: null };
queryBuilder.workspaceContextService = { getWorkbenchState: () => 1, getWorkspace: () => ({ folders: [{ uri: rootUri }] }) };
const globs = [];
for (const pattern of ['', '*.rs', '.rs', '*.rs, *.toml', '{*.rs,*.ts}', '**/*.rs', 'src', 'src/', './src', './src/**', './src/*.rs', './src, *.rs', '*', '**', '?.rs', '[ab].rs', '[!a].rs', 'src/**/main.*', 'generated', '**/node_modules', 'a{b,c}.rs', '[[]a].rs', './', './src/{a,b}.rs']) {
  const query = queryBuilder.parseSearchPaths(pattern);
  for (const file of ['main.rs', 'MAIN.RS', 'Cargo.toml', 'src/main.rs', 'src/deep/main.rs', 'src/main.ts', 'other/src/main.rs', 'src_extra/main.rs', 'generated/file.rs', 'src/generated/file.rs', 'node_modules/pkg/x.rs', 'ab.rs', 'ac.rs', 'a.rs', 'b.rs', '[a].rs']) {
    const expression = query.searchPaths?.length ? (query.pattern || query.searchPaths[0].pattern ? { ...query.pattern, ...query.searchPaths[0].pattern } : undefined) : query.pattern;
    globs.push({ pattern, file, expected: expression ? !!globApi.match(expression, file) : pattern.trim() !== '' });
  }
}
const target = path.resolve('tests/fixtures/vscode-search.json');
fs.writeFileSync(target, JSON.stringify({ revision, editor, workspace, replacements, preserved, globs }) + '\n');
console.log(`Generated ${editor.length} editor, ${workspace.length} workspace, ${replacements.length} replacement, ${preserved.length} case-preservation and ${globs.length} glob cases at ${target}`);
