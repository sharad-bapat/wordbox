"""Check the frozen source: every file in results/frozen.sha256 must hash as recorded.

Line endings are normalised to LF first, so a checkout that converts them doesn't count as a change.
Run before any held-out evaluation. Exits 1 on any mismatch.

usage: python tools/check_frozen.py            check
       python tools/check_frozen.py --write    record the current source (at the freeze only)
"""
import glob, hashlib, sys


def digest_of(name):
    return hashlib.sha256(open(name, 'rb').read().replace(b'\r\n', b'\n')).hexdigest()


if '--write' in sys.argv:
    names = ['extractor/Cargo.toml', 'extractor/Cargo.lock'] + sorted(glob.glob('extractor/src/*.rs'))
    with open('results/frozen.sha256', 'w', encoding='utf-8') as f:
        for n in names:
            f.write(digest_of(n) + '  ' + n.replace('\\', '/') + '\n')
    print(f'recorded {len(names)} files')
    sys.exit(0)

bad = 0
for line in open('results/frozen.sha256', encoding='utf-8'):
    digest, name = line.split()
    if digest_of(name.lstrip('*')) != digest:
        print(f'CHANGED {name}')
        bad += 1
print('frozen source: ' + ('ok' if not bad else f'{bad} file(s) changed'))
sys.exit(1 if bad else 0)
