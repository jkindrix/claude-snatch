#!/usr/bin/env python3
"""Weekly check for Claude Code log-format drift, using `snatch doctor`.

Runs `snatch doctor --since 8d -o json` and records a finding when:
  - records were kept only by dropping malformed optional fields (degraded),
  - lines failed to parse with a schema error (wrong type, missing field), as
    opposed to JSON corruption from torn writes, or
  - a record type, system subtype, content block, attachment kind or unpriced
    model appears that no earlier run has seen. The first run records the
    current set as the baseline instead of reporting all of it.

Findings are appended to $STATE/alert.md, which the SessionStart hook
(loadout hooks/snatch-recall.sh) shows until it is deleted. Every run writes
$STATE/last-run.json, so the hook can also say when the check stopped running.

Usage: drift-check.py [--state DIR] [--snatch PATH] [--since 8d]
"""
import argparse
import datetime
import json
import os
import subprocess
import sys

# serde data-error messages: the log parsed as JSON but did not fit the model.
# Everything else serde reports (expected `,`, EOF while parsing, ...) is a
# syntax error, i.e. a corrupt line rather than a format change.
SCHEMA_PREFIXES = ('invalid type', 'invalid value', 'invalid length', 'missing field',
                   'unknown variant', 'unknown field', 'duplicate field')
KIND_SECTIONS = {
    'unknown_entry_types': 'record type',
    'other_system_subtypes': 'system subtype',
    'unknown_content_blocks': 'content block',
    'attachment_kinds': 'attachment kind',
    'unpriced_models': 'unpriced model',
}


def run_doctor(snatch, since):
    try:
        p = subprocess.run([snatch, 'doctor', '--since', since, '-o', 'json'],
                           capture_output=True, text=True, timeout=3600)
    except (OSError, subprocess.TimeoutExpired) as e:
        return None, f'could not run `{snatch} doctor`: {e}'
    if p.returncode != 0:
        tail = ' '.join(p.stderr.split())[-300:]
        return None, f'`snatch doctor` exited {p.returncode}: {tail}'
    try:
        return json.loads(p.stdout), None
    except ValueError as e:
        return None, f'`snatch doctor` printed invalid JSON: {e}'


def findings_for(report, known):
    """Return (findings, updated known kinds, whether this was the baseline run)."""
    out = []
    for cause, s in (report.get('degraded_fields') or {}).items():
        out.append(f"{s['count']} record(s) kept without a malformed field — {cause} "
                   f"(e.g. {s.get('example', '?')})")
    for cause, s in (report.get('parse_errors') or {}).items():
        if cause.startswith(SCHEMA_PREFIXES):
            out.append(f"{s['count']} line(s) dropped by a schema error — {cause} "
                       f"(e.g. {s.get('example', '?')})")
    baseline = known is None
    known = dict(known or {})
    for section, label in KIND_SECTIONS.items():
        seen = set(known.get(section, []))
        current = report.get(section) or {}
        if not baseline:
            for name in sorted(set(current) - seen):
                s = current[name]
                example = s.get('example_session')
                out.append(f"new {label} `{name}`: {s.get('count')}x"
                           + (f" (e.g. session {example})" if example else ''))
        known[section] = sorted(seen | set(current))
    return out, known, baseline


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('--state', default=os.path.expanduser('~/.local/state/snatch-drift'))
    ap.add_argument('--snatch', default=os.path.expanduser('~/.cargo/bin/snatch'))
    ap.add_argument('--since', default='8d', help='doctor window (default 8d: a week plus overlap)')
    a = ap.parse_args()
    os.makedirs(a.state, exist_ok=True)
    known_path = os.path.join(a.state, 'known-kinds.json')
    now = datetime.datetime.now().astimezone()

    report, error = run_doctor(a.snatch, a.since)
    if error:
        findings, baseline = [error], False
    else:
        try:
            with open(known_path) as fh:
                known = json.load(fh)
        except (OSError, ValueError):
            known = None
        findings, known, baseline = findings_for(report, known)
        tmp = known_path + '.tmp'
        with open(tmp, 'w') as fh:
            json.dump(known, fh, indent=1)
        os.replace(tmp, known_path)

    if findings:
        with open(os.path.join(a.state, 'alert.md'), 'a') as fh:
            fh.write(f"## {now:%Y-%m-%d} — snatch doctor (last {a.since})\n\n")
            fh.writelines(f"- {f}\n" for f in findings)
            fh.write("\n")
    with open(os.path.join(a.state, 'last-run.json'), 'w') as fh:
        json.dump({'ran_at': now.isoformat(timespec='seconds'), 'findings': len(findings),
                   'baseline': baseline, 'error': error,
                   'sessions_scanned': (report or {}).get('sessions_scanned')}, fh)
    print(f"{len(findings)} finding(s){' (baseline run)' if baseline else ''}")
    return 0


if __name__ == '__main__':
    sys.exit(main())
