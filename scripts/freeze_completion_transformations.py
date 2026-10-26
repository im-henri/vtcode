"""Freeze designed transformation sessions and independent real-source crops.

This is evaluation input construction, not predictor code. It never runs a
predictor or selects cases by its output. Expected identifiers are removed at
the caret. Source-file and vocabulary partitions are deterministic and disjoint.
"""
import argparse
import hashlib
import json
from pathlib import Path
import re


TOKEN = re.compile(r'[\w$]+|"(?:\\.|[^"\\])*"|[^\s]', re.UNICODE)
WORD = re.compile(r'^[\w$]+$', re.UNICODE)


def digest(value):
    return hashlib.sha256(value).hexdigest()


def pascal(name):
    return ''.join(piece[:1].upper() + piece[1:] for piece in name.split('_'))


def camel(name):
    result = pascal(name)
    return result[:1].lower() + result[1:]


def designed(roots, split):
    result = []
    for number, root in enumerate(roots):
        source = f'session-{number}.unmapped-syntax' if number % 2 else f'session-{number}'

        def add(group, left, prefix, expected=None, right='', allowed=(), quiet=False):
            visible = left + prefix + right
            if expected:
                assert expected not in TOKEN.findall(visible), (group, expected)
            result.append(dict(id=f'{split}:{root}:{group}', split=split, group=group,
                               workload='controlled', source=source, visible=visible,
                               prefix=prefix, caret=len(left + prefix), expected=expected,
                               novel=bool(expected), allowed=list(allowed), quiet=quiet))

        for substantial in (False, True):
            tail = '.with_border(style{n}, color{n}).with_padding(Insets::new(1, 2, 3, 4))' if substantial else ''
            template = (f'let record_{root}{{n}} = build_entry(source_{root}{{n}}, options, theme, bounds)'
                        f'.with_action(action_{root}{{n}})' + tail + '.finish();\n')
            before = ''.join(template.format(n=n) for n in range(1, 5))
            group = 'long-statements' if substantial else 'short-statements'
            add(group + '-first', before + 'let ', f'record_{root}', f'record_{root}5')
            add(group + '-middle', before + f'let record_{root}5 = build_entry(', f'source_{root}', f'source_{root}5')
            add(group + '-last', before + f'let record_{root}5 = build_entry(source_{root}5, options, theme, bounds).with_action(', f'action_{root}', f'action_{root}5')

        names = [f'{root}_{suffix}' for suffix in ('amber', 'birch', 'cedar', 'dune', 'elm')]
        transformations = [
            ('row-prefix', lambda value: value, lambda value: f'decode_{value}'),
            ('row-suffix', lambda value: value, lambda value: f'{value}_cached'),
            ('row-replacement', lambda value: f'source_{value}', lambda value: f'target_{value}'),
            ('row-wrap', lambda value: value, lambda value: f'fetch_{value}_cached'),
            ('row-pascal', lambda value: value, pascal),
            ('row-camel', lambda value: value, camel),
            ('row-snake', pascal, lambda value: value),
            ('row-upper-snake', lambda value: value, str.upper),
            ('row-lower-camel', str.upper, camel),
            ('row-case-wrap', lambda value: f'source_{value}', lambda value: f'decode{pascal(value)}Value'),
            ('row-affix-pair', lambda value: f'staged_{value}_temporary', lambda value: f'finished_{value}_ready'),
            ('row-reverse', lambda value: value, lambda value: '_'.join(reversed(value.split('_')))),
        ]
        for group, input_name, output_name in transformations:
            if split == 'development':
                before = ''.join(f'let {input_name(name)} = {output_name(name)}(payload);\n' for name in names[:4])
                left = before + f'let {input_name(names[4])} = '
            else:
                before = ''.join(f'bind({input_name(name)}, {output_name(name)}, channel);\n' for name in names[:4])
                left = before + f'bind({input_name(names[4])}, '
            expected = output_name(names[4])
            add(group, left, expected[:3], expected)

        before = ''.join(f'let {name} = decode_{name}(payload);\n' for name in names[:4])
        add('row-right-context', before + f'let {names[4]} = ', 'decode', f'decode_{names[4]}', '(payload);')
        reverse = ''.join(f'{name} = decode_{name}(payload);\n' for name in names[:4])
        add('row-reverse-direction', reverse, names[4][:3], names[4], f' = decode_{names[4]}(payload);')
        # The whole expected spelling is absent even though another token may
        # contain it as a component. That is the transformation being inferred.
        one = f'let {names[0]} = decode_{names[0]}(payload);\n'
        add('negative-single-witness', one + f'let {names[4]} = ', 'decode')
        add('negative-cloned-witness', one * 12 + f'let {names[4]} = ', 'decode')
        conflicting = before + f'let {root}_fir = encode_{root}_fir(payload);\n'
        add('negative-contradictory-row', conflicting + f'let {names[4]} = ', 'decode')
        add('negative-wrong-fixed-context', before + f'emit {names[4]} = ', 'decode')
        add('negative-wrong-suffix-context', before + f'let {names[4]} = ', 'decode', right='(other_payload);')
        add('negative-scope', before + '}\n' + f'let {names[4]} = ', 'decode')
        add('negative-indentation', before + f'    let {names[4]} = ', 'decode')
        alias = ''.join(f'let {name} = decode_{name}({name});\n' for name in names[:4])
        add('negative-inconsistent-alias', alias + f'let {names[4]} = ', 'decode', right=f'({root}_other);')
        add('negative-string', before + 'message = "', 'decode', quiet=True)
        add('negative-comment', before + '// ', 'decode', quiet=True)
        unrelated = ''.join(f'let {name} = independent_{color}(payload);\n' for color, name in zip(('red', 'cyan', 'ochre', 'blue'), names[:4]))
        add('negative-unrelated-pairs', unrelated + f'let {names[4]} = ', 'independent')
        add('negative-cold-start', f'let {names[4]} = ', 'decode')
    return result


def shape(tokens):
    return tuple('w' if WORD.fullmatch(value) else value for value in tokens)


def related(a, b):
    left = re.sub(r'[^\w]', '', a).lower()
    right = re.sub(r'[^\w]', '', b).lower()
    # A corpus-independent lexical eligibility rule, not the candidate DSL.
    return (len(left) >= 3 and len(right) >= 3
            and any(left[i:i + 3] in right for i in range(len(left) - 2)))


def source_cases(files, split, cap):
    result = []
    for name, text in files:
        file_split = 'development' if hashlib.sha256(name.encode()).digest()[0] % 2 == 0 else 'holdout'
        if file_split != split:
            continue
        lines = text.splitlines(keepends=True)
        offset = 0
        previous = []
        for line_number, line in enumerate(lines):
            matches = list(TOKEN.finditer(line))
            tokens = [match.group() for match in matches]
            indentation = line[:len(line) - len(line.lstrip())]
            if not tokens or line.lstrip().startswith(('//', '#', '/*', '*')):
                previous.clear()
                offset += len(line)
                continue
            if len(previous) >= 2:
                examples = previous[-2:]
                if all(row[0] == indentation and shape(row[1]) == shape(tokens) for row in examples):
                    for at, match in enumerate(matches):
                        word = match.group()
                        if not WORD.fullmatch(word) or len(word) < 5 or not word[0].isalpha():
                            continue
                        if any(row[1][at] == word for row in examples):
                            continue
                        anchor_columns = [i for i in range(at) if WORD.fullmatch(tokens[i])
                                          and examples[0][1][i] != examples[1][1][i]
                                          and related(tokens[i], word)
                                          and all(related(row[1][i], row[1][at]) for row in examples)]
                        if not anchor_columns:
                            continue
                        prefix = word[:3]
                        start = offset + match.start()
                        end = start + len(prefix)
                        visible = text[:end]
                        result.append(dict(id=f'{name}:{start}', split=split, group='real-source-related',
                                           workload='source', source=name, line=line_number + 1,
                                           offset=start, visible=visible, prefix=prefix, caret=len(visible),
                                           # The worker can observe words inside literals too.
                                           # Quoted spellings are therefore not novel names.
                                           expected=word, novel=word not in re.findall(r'[\w$]+', visible),
                                           anchors=anchor_columns, allowed=[], quiet=False))
            previous.append((indentation, tokens))
            previous = previous[-2:]
            offset += len(line)
    result.sort(key=lambda case: digest(('source-challenges-v1:' + case['id']).encode()))
    return result[:cap]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--corpus', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--source-cap-per-split', type=int, default=512)
    args = parser.parse_args()
    if args.output.exists():
        parser.error('inventory already exists; do not overwrite frozen evaluation data')
    raw_manifest = (args.corpus / 'manifest.json').read_bytes()
    manifest = json.loads(raw_manifest)
    files = []
    vocabulary = set()
    for entry in manifest['files']:
        data = (args.corpus / entry['path']).read_bytes()
        assert digest(data) == entry['sha256']
        text = data.decode('utf-8')
        files.append((entry['path'], text))
        vocabulary.update(word for word in TOKEN.findall(text)
                          if re.fullmatch(r'[a-z][a-z_]{4,13}[a-z]', word))
    inventory = dict(formatVersion=1, baselineRevision=manifest['revision'],
                     corpusManifestSha256=digest(raw_manifest),
                     sourceSelection='two preceding equal lexical shapes; a varying earlier related identifier; prefix=3; SHA-256 order; file-disjoint splits',
                     sourceCapPerSplit=args.source_cap_per_split, roots={}, cases=[])
    for split in ('development', 'holdout'):
        roots = [word for word in vocabulary if (hashlib.sha256(word.encode()).digest()[0] % 2 == 0) == (split == 'development')]
        roots.sort(key=lambda word: digest(('transform-roots-v1:' + split + ':' + word).encode()))
        inventory['roots'][split] = roots[:16]
        inventory['cases'].extend(designed(roots[:16], split))
        inventory['cases'].extend(source_cases(files, split, args.source_cap_per_split))
    args.output.parent.mkdir(parents=True, exist_ok=True)
    data = (json.dumps(inventory, ensure_ascii=False, indent=2) + '\n').encode()
    args.output.write_bytes(data)
    counts = {}
    for case in inventory['cases']:
        key = case['split'] + ':' + case['workload']
        counts[key] = counts.get(key, 0) + 1
    print(json.dumps(dict(inventorySha256=digest(data), bytes=len(data), counts=counts), indent=2))


if __name__ == '__main__':
    main()
