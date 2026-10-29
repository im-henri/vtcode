// Node >= 22.20. Runs pinned MIT-licensed Microsoft VS Code comparers.
// Upstream source downloads remain in ignored scratch/.
import fs from 'node:fs';
import path from 'node:path';
import { stripTypeScriptTypes } from 'node:module';

const revision = 'eaa5f41042f412308642339e8874c6bf13b3f01b';
const cache = path.resolve('scratch/vscode-ordering-20261029/upstream');
fs.mkdirSync(cache, { recursive: true });
const sources = {
  'comparers.ts': 'src/vs/base/common/comparers.ts',
  'explorerViewer.ts': 'src/vs/workbench/contrib/files/browser/views/explorerViewer.ts',
  'searchCompare.ts': 'src/vs/workbench/contrib/search/browser/searchCompare.ts',
};
for (const [name, source] of Object.entries(sources)) {
  if (!fs.existsSync(path.join(cache, name))) {
    const response = await fetch('https://raw.githubusercontent.com/microsoft/vscode/' + revision + '/' + source);
    if (!response.ok) throw new Error(source + ': ' + response.status);
    fs.writeFileSync(path.join(cache, name), await response.text());
  }
}
const read = name => fs.readFileSync(path.join(cache, name), 'utf8');
const js = source => stripTypeScriptTypes(source.replace(/^import .*;\r?\n/gm, '')
  .replace(/^export /gm, ''), { mode: 'transform', disableExperimentalWarning: true });
class Lazy {
  constructor(make) { this.make = make; }
  get value() { return this.cached ??= this.make(); }
}
const names = ['system.h', 'system_defines.h', 'system', 'system10.h', 'system2.h',
  'file1', 'file01', 'file001', 'file2', 'file10', 'file02', 'file1a',
  'a.ts', 'A.ts', 'á.ts', 'ä.ts', 'Å.ts', 'z.ts', 'Z.ts', 'õ.ts', 'ö.ts', 'ü.ts',
  'é.ts', 'e.ts', 'É.ts', 'aa.ts', '😀.ts', '𐐀.ts', '\uE000.ts',
  'x2', 'x١', 'x01', 'x1', 'x０１', 'x٠٢', 'x10', 'x١٠',
  '_file', '.file', '-file', 'file-a', 'file_a', 'file.a', 'file a',
  'file#a', 'file$a', 'file+a', 'file@a', 'file!a', 'file%a',
  'file(a)', 'file[a]', 'file~a', '~file', '.env', 'a.d.ts', '.hidden.json',
  'file2.rs', 'file10.RS', 'file1.ts', 'file01.TS', 'file.', 'file',
  'I', 'i', 'İ', 'ı', '字2.rs', '字10.rs', '章.rs', '中文.rs'];
const paths = ['z.h', 'system.h', 'system_defines.h', 'a/z.h', 'a/system.h',
  'a/system_defines.h', 'a/deep/a.h', 'a10/x.h', 'a2/x.h', 'b/x.h',
  'a/file2.h', 'a/file10.h', 'a/file01.h', 'a/file1.h', 'a/deep/z.h',
  'a-dir/z.h', 'a_dir/z.h', 'a.dir/z.h', 'á/x.h', 'z/x.h', 'same/Z.ts', 'same/a.ts'];
const entries = ['system.h', 'system_defines.h', 'file10.TS', 'file2.ts', '.a.rs',
  '.env', 'a.d.ts', 'a.js', 'A.TS', 'a.ts', 'folder10', 'folder2', 'folder.h', 'folder_defines.h']
  .map(name => ({ name, isDirectory: name.startsWith('folder'), mtime: 1, isRoot: false }));
const exported = ['compareFileNames', 'compareFileNamesDefault', 'compareFileNamesUpper',
  'compareFileNamesLower', 'compareFileNamesUnicode', 'compareFileExtensions',
  'compareFileExtensionsDefault', 'compareFileExtensionsUpper', 'compareFileExtensionsLower',
  'compareFileExtensionsUnicode', 'comparePaths'];
const sorterSource = read('explorerViewer.ts').split('export class FileSorter')[1].split('export class FileDragAndDrop')[0];
const compareMethod = sorterSource.slice(sorterSource.indexOf('\n\tcompare('), sorterSource.lastIndexOf('\n}'));
const data = [];
for (const locale of ['en-US', 'et-EE', 'sv-SE', 'de-DE', 'tr-TR', 'da-DK', 'th-TH', 'zh-CN']) {
  const safeIntl = { Collator: (_, options) => ({ value: new Intl.Collator(locale, options) }) };
  const comparers = new Function('Lazy', 'safeIntl', 'sep', js(read('comparers.ts'))
    + '; return {' + exported.join(',') + '};')(Lazy, safeIntl, '/');
  const Sorter = new Function(...exported, js('class FileSorter {'
    + 'constructor(explorerService, contextService) { this.explorerService = explorerService; this.contextService = contextService; }'
    + compareMethod + '\n}') + ';return FileSorter;')(...exported.map(name => comparers[name]));
  const sorts = {};
  for (const sortOrder of ['default', 'type']) {
    const sorter = new Sorter({ sortOrderConfiguration: { sortOrder, lexicographicOptions: 'default', reverse: false } }, {});
    sorts[sortOrder] = entries.slice().sort((a, b) => sorter.compare(a, b)).map(entry => entry.name);
  }
  const SortOrder = { Default: 'default', Type: 'type', FileNames: 'fileNames',
    CountAscending: 'countAscending', CountDescending: 'countDescending', Modified: 'modified' };
  const Range = { compareRangesUsingStarts: (a, b) => a.startLineNumber - b.startLineNumber
    || a.startColumn - b.startColumn || a.endLineNumber - b.endLineNumber || a.endColumn - b.endColumn };
  const comparer = new Function('compareFileExtensions', 'compareFileNames', 'comparePaths',
    'SearchSortOrder', 'Range', 'isSearchTreeFileMatch', 'isSearchTreeFolderMatch',
    'isSearchTreeMatch', 'isIMatchInNotebook', 'isSearchTreeAIFileMatch', 'createParentList',
    js(read('searchCompare.ts')) + ';return searchMatchComparer;')(
    comparers.compareFileExtensions, comparers.compareFileNames, comparers.comparePaths,
    SortOrder, Range, value => value.kind === 'file', () => false, value => value.kind === 'match',
    () => false, () => false, () => []);
  const files = paths.map(value => ({ kind: 'file', resource: { fsPath: value }, name: () => value.split('/').at(-1) }));
  data.push({ locale, names: names.slice().sort(comparers.compareFileNamesDefault),
    searchNames: names.slice().sort(comparers.compareFileNames), explorer: sorts,
    paths: files.slice().sort((a, b) => comparer(a, b)).map(file => file.resource.fsPath) });
}
fs.writeFileSync('tests/fixtures/vscode-ordering.json',
  JSON.stringify({ revision, sources, inputs: { names, paths, entries }, locales: data }, null, 2) + '\n');
console.log('Generated upstream ordering fixtures for ' + data.length + ' locales.');
