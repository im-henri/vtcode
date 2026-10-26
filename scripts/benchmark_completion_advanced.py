"""Compare advanced completion changes against the recorded pre-change binary.

Runs established frozen regression workloads, not a new population accuracy
study. Keep raw reports/logs in the requested output directory. Run without a
concurrent build when interpreting latency.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import statistics
import subprocess

from compare_completion_replays import compare
from benchmark_completion_transformations import quality

ROOT = Path(__file__).resolve().parents[1]


def digest(data):
    return hashlib.sha256(data).hexdigest()


def verify_baseline(binary, revision, record_path):
    """Verify every fingerprint supplied by either recorded report format."""
    record = json.loads(record_path.read_text(encoding='utf-8-sig'))
    if 'provenance' in record:
        # Preserve the original transformation experiment's verification path.
        provenance = record['provenance']
        binary_hash = provenance['candidateBinarySha256']
        production = provenance['productionFiles']
    else:
        binary_hash = record['binaries']['candidate']['sha256']
        production = record['productionFiles']
    assert digest(binary.read_bytes()) == binary_hash, 'Baseline executable differs from provenance'
    assert production, 'Baseline provenance contains no source fingerprints'
    verified = []
    for recorded_path, expected in production.items():
        path = Path(recorded_path)
        if path.is_absolute():
            # New reports record absolute local paths. They must still name
            # files in this repository before they become git revision paths.
            path = path.resolve().relative_to(ROOT.resolve())
        assert '..' not in path.parts, f'Baseline source escapes repository: {recorded_path}'
        path = path.as_posix()
        original = subprocess.check_output(['git', 'show', f'{revision}:{path}'], cwd=ROOT)
        normalized = original.replace(b'\r\n', b'\n')
        possibilities = {digest(original), digest(normalized), digest(normalized.replace(b'\n', b'\r\n'))}
        assert expected in possibilities, f'Baseline source fingerprint differs: {path}'
        verified.append(path)
    return verified


def decision_rows(report):
    # Publication timing/revisions can vary; the visible-input digest is checked
    # by compare(). Suggestions, ranks, recall and negative outcomes must not.
    return [{key: value for key, value in row.items() if key != 'indexPublication'}
            for row in report['rows']]


def assert_repeat_stable(name, reference, repeated):
    for field in ('corpusSha256', 'inventorySha256', 'fixtureSha256', 'includeHoldout',
                  'warmIndex', 'mode', 'indexScope', 'seedFilesPerSession',
                  'negativeCases', 'unwanted', 'limited'):
        assert reference.get(field) == repeated.get(field), f'Nondeterministic {name}/{field}'
    assert decision_rows(reference) == decision_rows(repeated), f'Nondeterministic {name}/decisions'
    if name.rsplit('/', 1)[-1] not in ('transformations', 'mechanisms'):
        compare(reference, repeated)


def mechanism_quality(report):
    # Numeric rows enter after the old lexical-recall instrumentation. Report
    # actual popup outcomes consistently for all mechanism groups instead.
    groups = {}
    for group in ['all', *sorted({row['group'] for row in report['rows']})]:
        rows = [row for row in report['rows'] if group == 'all' or row['group'] == group]
        positive = [row for row in rows if row['positive']]
        groups[group] = dict(cases=len(rows), positiveCases=len(positive),
                             negativeCases=len(rows) - len(positive),
                             top1=sum(row['rank'] == 0 for row in positive),
                             top5=sum(row['rank'] is not None and row['rank'] < 5 for row in positive),
                             unexpectedNovelCases=sum(bool(row['unwantedNovel']) for row in rows),
                             quietViolations=sum(row['quietViolation'] for row in rows),
                             limited=sum(row['limited'] for row in rows))
    return groups


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--baseline', type=Path, required=True)
    parser.add_argument('--baseline-revision', default='cc0cab7c98db14ddafbb317dbca2c354d1db63c5')
    parser.add_argument('--baseline-provenance', type=Path, required=True,
                        help='Recorded report whose candidate executable becomes this baseline')
    parser.add_argument('--candidate', type=Path, required=True)
    parser.add_argument('--source', type=Path, required=True)
    parser.add_argument('--inventory', type=Path, required=True)
    parser.add_argument('--mechanism-inventory', type=Path)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--report', type=Path, required=True)
    parser.add_argument('--repeats', type=int, default=3)
    args = parser.parse_args()
    assert args.repeats > 0
    args.output.mkdir(parents=True, exist_ok=True)
    temp_root = (args.output / 'temp').resolve()
    temp_root.mkdir(parents=True, exist_ok=True)
    binaries = {'baseline': args.baseline.resolve(), 'candidate': args.candidate.resolve()}
    provenance_path = args.baseline_provenance.resolve()
    assert args.report.resolve() != provenance_path, 'Do not overwrite the baseline provenance report'
    verified = verify_baseline(binaries['baseline'], args.baseline_revision, provenance_path)
    try:
        provenance_label = provenance_path.relative_to(ROOT.resolve()).as_posix()
    except ValueError:
        provenance_label = str(provenance_path)
    report = {
        'formatVersion': 1,
        'description': 'Established regression replays; new features are evaluated by separate mechanism tests.',
        'baselineRevision': subprocess.check_output(['git', 'rev-parse', args.baseline_revision], text=True, cwd=ROOT).strip(),
        'candidateBaseRevision': subprocess.check_output(['git', 'rev-parse', 'HEAD'], text=True, cwd=ROOT).strip(),
        'baselineProvenance': {'record': provenance_label,
                               'recordSha256': digest(provenance_path.read_bytes()),
                               'verifiedFiles': verified, 'lineEndingNormalizationOnly': True},
        'binaries': {key: {'path': str(path), 'sha256': digest(path.read_bytes())} for key, path in binaries.items()},
        'inputs': {'source': str(args.source.resolve()),
                   'temporaryDirectory': str(temp_root),
                   'inventorySha256': digest(args.inventory.read_bytes())},
        'productionFiles': {str(path).replace('\\', '/'): digest(path.read_bytes()) for path in [
            ROOT / 'src' / name for name in ('app_vtcomplete.rs', 'vtcomplete_worker.rs',
            'vtcomplete_scoring.rs', 'completion_examples.rs', 'completion_edit_history.rs',
            'completion_numbers.rs', 'completion_replacement.rs', 'completion_patterns.rs',
            'completion_transformations.rs', 'completion_index.rs', 'completion_context.rs',
            'completion_source.rs', 'editor.rs')]},
        'regressions': {},
    }
    runs = {engine: {} for engine in binaries}
    workloads = [
        ('native', 'benchmark_vtcode_editing_replay', {
            'VTCODE_CAPACITY_SOURCE': str(args.source.resolve()), 'VTCODE_VTCODE_REPLAY_REPORT': None}),
        ('learning', 'learning_replay_quality', {'VTCODE_LEARNING_REPORT': None}),
        ('transformations', 'benchmark_transformations_replay', {
            'VTCODE_TRANSFORM_INPUT': str(args.inventory.resolve()),
            'VTCODE_TRANSFORM_SPLIT': 'holdout', 'VTCODE_TRANSFORM_REPORT': None}),
    ]
    if args.mechanism_inventory:
        report['inputs']['mechanismInventorySha256'] = digest(args.mechanism_inventory.read_bytes())
        workloads.append(('mechanisms', 'benchmark_transformations_replay', {
            'VTCODE_TRANSFORM_INPUT': str(args.mechanism_inventory.resolve()),
            # The existing harness names this dispatch selector "holdout";
            # this designed fixture is explicitly mechanism/development data.
            'VTCODE_TRANSFORM_SPLIT': 'holdout', 'VTCODE_TRANSFORM_REPORT': None}))
    for repetition in range(args.repeats):
        order = list(binaries) if repetition % 2 == 0 else list(reversed(binaries))
        for engine in order:
            for name, test, settings in workloads:
                stem = f'{repetition + 1}-{engine}-{name}'
                path = args.output.resolve() / f'{stem}.json'
                path.unlink(missing_ok=True)  # A zero-test run must not reuse an old report.
                environment = {k: v for k, v in os.environ.items() if not k.startswith('VTCODE_')}
                environment.update(TEMP=str(temp_root), TMP=str(temp_root), TMPDIR=str(temp_root))
                environment.update({key: str(path) if value is None else value for key, value in settings.items()})
                with (args.output / f'{stem}.log').open('w', encoding='utf-8') as log:
                    subprocess.run([str(binaries[engine]), test, '--include-ignored', '--nocapture',
                                    '--test-threads=1'], env=environment, stdout=log,
                                   stderr=subprocess.STDOUT, check=True, timeout=240, cwd=ROOT)
                value = json.loads(path.read_text(encoding='utf-8-sig'))
                if name in ('transformations', 'mechanisms'):
                    input_key = 'mechanismInventorySha256' if name == 'mechanisms' else 'inventorySha256'
                    assert value['inventorySha256'] == report['inputs'][input_key]
                runs[engine].setdefault(name, []).append(value)
                print(stem, 'complete', flush=True)
    for name, _, _ in workloads:
        before, after = (runs[engine][name][0] for engine in binaries)
        if name in ('transformations', 'mechanisms'):
            assert before['fixtureSha256'] == after['fixtureSha256']
            scorer = mechanism_quality if name == 'mechanisms' else quality
            result = {'baseline': scorer(before), 'candidate': scorer(after)}
            if name == 'mechanisms':
                result['scope'] = 'Fixed designed mechanism probes; not held-out or population accuracy evidence.'
                result['candidateRecallOmitted'] = 'Numeric predictions enter after lexical admission tracing.'
        else:
            result = compare(before, after)
        latency = {}
        for engine in binaries:
            fields = ('workerWarmP95Us', 'coldP95Us') if name in ('transformations', 'mechanisms') else (
                ('workerWarmP95Us', 'coldP95Us') if name == 'native' else ())
            latency[engine] = {field: {'runs': [v[field] for v in runs[engine][name]],
                                     'median': statistics.median(v[field] for v in runs[engine][name])}
                               for field in fields}
            for repeated in runs[engine][name][1:]:
                assert_repeat_stable(f'{engine}/{name}', runs[engine][name][0], repeated)
        report['regressions'][name] = {'quality': result, 'latencyUs': latency}
        if name == 'native':
            report['inputs']['nativeCorpusSha256'] = before['corpusSha256']
    for engine, path in binaries.items():
        assert digest(path.read_bytes()) == report['binaries'][engine]['sha256'], f'Binary changed during replay: {engine}'
    assert digest(provenance_path.read_bytes()) == report['baselineProvenance']['recordSha256']
    for path, expected in report['productionFiles'].items():
        assert digest(Path(path).read_bytes()) == expected, f'Production source changed during replay: {path}'
    assert digest(args.inventory.read_bytes()) == report['inputs']['inventorySha256']
    if args.mechanism_inventory:
        assert digest(args.mechanism_inventory.read_bytes()) == report['inputs']['mechanismInventorySha256']
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, indent=2) + '\n', encoding='utf-8')
    print(args.report)


if __name__ == '__main__':
    main()
