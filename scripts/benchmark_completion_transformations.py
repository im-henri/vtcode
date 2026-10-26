"""Alternate two unchanged executables on one frozen transformation inventory.

Development runs select components. Holdout runs require a freeze record naming
the exact candidate binary, inventory, mode, and production-source hashes.
Raw reports and logs remain in the output directory, including every miss.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import statistics
import subprocess


def digest(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def quality(report):
    result = {}
    for workload in ('controlled', 'source'):
        rows = [row for row in report['rows'] if row['workload'] == workload]
        positives = [row for row in rows if row['positive']]
        negatives = [row for row in rows if not row['positive']]
        result[workload] = dict(cases=len(rows), positiveCases=len(positives),
                               negativeCases=len(negatives), top1=sum(row['rank'] == 0 for row in positives),
                               top5=sum(row['rank'] is not None and row['rank'] < 5 for row in positives),
                               candidateRecall=sum(row['candidateRecall'] for row in positives),
                               novelPositiveCases=sum(row['novel'] for row in positives),
                               novelTop1=sum(row['novel'] and row['rank'] == 0 for row in positives),
                               negativeUnwantedCases=sum(bool(row['unwantedNovel']) for row in negatives),
                               positiveUnwantedCases=sum(bool(row['unwantedNovel']) for row in positives),
                               quietViolations=sum(row['quietViolation'] for row in rows),
                               limited=sum(row['limited'] for row in rows))
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--baseline', type=Path, required=True)
    parser.add_argument('--candidate', type=Path, required=True)
    parser.add_argument('--inventory', type=Path, required=True)
    parser.add_argument('--split', choices=('development', 'holdout'), default='development')
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--candidate-mode', default='full')
    parser.add_argument('--disable-live-file-prior', action='store_true')
    parser.add_argument('--repeats', type=int, default=3)
    parser.add_argument('--freeze', type=Path)
    args = parser.parse_args()
    assert args.repeats >= 1
    inventory_hash = digest(args.inventory)
    binaries = {'before': args.baseline.resolve(), 'after': args.candidate.resolve()}
    hashes = {name: digest(path) for name, path in binaries.items()}
    if args.split == 'holdout':
        if not args.freeze:
            parser.error('freeze the selected candidate before inspecting holdout results; --freeze is required')
        frozen = json.loads(args.freeze.read_text(encoding='utf-8-sig'))
        assert frozen['candidateSha256'] == hashes['after']
        assert frozen['inventorySha256'] == inventory_hash
        assert frozen['mode'] == args.candidate_mode
        assert frozen['liveFilePrior'] == (not args.disable_live_file_prior)
        for path, expected in frozen['productionFiles'].items():
            assert digest(Path(path)) == expected, f'production source changed after selection: {path}'
    args.output.mkdir(parents=True, exist_ok=True)
    reports = {name: [] for name in binaries}
    fixture = None
    for repeat in range(args.repeats):
        for name in (('before', 'after') if repeat % 2 == 0 else ('after', 'before')):
            stem = f'{args.split}-{repeat + 1}-{name}'
            output = (args.output / f'{stem}.json').resolve()
            environment = os.environ.copy()
            environment.update(VTCODE_TRANSFORM_INPUT=str(args.inventory.resolve()),
                               VTCODE_TRANSFORM_REPORT=str(output), VTCODE_TRANSFORM_SPLIT=args.split,
                               VTCODE_GENERATION_MODE='full' if name == 'before' else args.candidate_mode)
            environment.pop('VTCODE_DISABLE_LIVE_FILE_PRIOR', None)
            if name == 'after' and args.disable_live_file_prior:
                environment['VTCODE_DISABLE_LIVE_FILE_PRIOR'] = '1'
            with (args.output / f'{stem}.log').open('w', encoding='utf-8') as log:
                subprocess.run([str(binaries[name]), 'benchmark_transformations_replay', '--ignored',
                                '--nocapture', '--test-threads=1'], env=environment,
                               stdout=log, stderr=subprocess.STDOUT, check=True, timeout=600)
            report = json.loads(output.read_text(encoding='utf-8'))
            assert report['inventorySha256'] == inventory_hash
            if fixture is None:
                fixture = report['fixtureSha256']
            assert report['fixtureSha256'] == fixture, 'baseline/candidate received different inputs'
            if reports[name]:
                assert quality(report) == quality(reports[name][0]), 'quality is not deterministic'
                assert [row['rank'] for row in report['rows']] == [row['rank'] for row in reports[name][0]['rows']]
            reports[name].append(report)
            print(stem, quality(report), 'warm p95 us:', report['workerWarmP95Us'], flush=True)
    result = dict(split=args.split, mode=args.candidate_mode, liveFilePrior=not args.disable_live_file_prior, inventorySha256=inventory_hash,
                  fixtureSha256=fixture, binaries={name: dict(path=str(path), sha256=hashes[name])
                                                  for name, path in binaries.items()}, runs={})
    for name, runs in reports.items():
        result['runs'][name] = dict(quality=quality(runs[0]), groups=runs[0]['groups'],
                                  latencyUs={key: dict(runs=[run[key] for run in runs],
                                                      median=statistics.median(run[key] for run in runs))
                                             for key in ('coldP95Us', 'workerWarmP95Us')},
                                  completionEvidenceBytes=runs[0]['completionEvidenceBytes'])
    result['comparison'] = {}
    for workload in ('controlled', 'source'):
        before, after = (result['runs'][name]['quality'][workload] for name in ('before', 'after'))
        old_errors = before['positiveCases'] - before['top1']
        new_errors = after['positiveCases'] - after['top1']
        result['comparison'][workload] = dict(topChoiceErrorsBefore=old_errors, topChoiceErrorsAfter=new_errors,
                                             topChoiceErrorReduction=(old_errors - new_errors) / old_errors if old_errors else None,
                                             extraUnwantedNegativeCases=after['negativeUnwantedCases'] - before['negativeUnwantedCases'])
    pairs = zip(reports['before'][0]['rows'], reports['after'][0]['rows'])
    result['changedCases'] = [dict(id=a['id'], group=a['group'], before=a['rank'], after=b['rank'])
                              for a, b in pairs if a['rank'] != b['rank']]
    result['acceptance'] = dict(
        controlledTopChoiceErrorsReducedAtLeast20Percent=(result['comparison']['controlled']['topChoiceErrorReduction'] or 0) >= .2,
        noUnwantedNovelNegatives=result['runs']['after']['quality']['controlled']['negativeUnwantedCases'] == 0,
        noQuietContextPopups=result['runs']['after']['quality']['controlled']['quietViolations'] == 0,
        realSourceTop1NotWorse=result['runs']['after']['quality']['source']['top1'] >= result['runs']['before']['quality']['source']['top1'],
        everyMeasuredWarmP95Below5Ms=all(run['workerWarmP95Us'] < 5000 for runs in reports.values() for run in runs),
    )
    path = args.output / 'summary.json'
    path.write_text(json.dumps(result, indent=2) + '\n', encoding='utf-8')
    print(json.dumps(dict(comparison=result['comparison'], acceptance=result['acceptance']), indent=2))
    print(path)


if __name__ == '__main__':
    main()
