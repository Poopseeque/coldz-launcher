"""Проверка подписи обновления лаунчера открытым ключом из src-tauri/tauri.conf.json (тем, что вшит в установленные
лаунчеры). Секретный ключ не нужен и не читается. Только стандартная библиотека Python.

    python scripts/verify-update.py 0.3.1     файлы release/0.3.1/ после `npm run release`
    python scripts/verify-update.py --url     то, что получают игроки: releases/latest/download/latest.json и установщик

Код возврата 0 — всё сходится, 1 — нет. Ed25519 — эталонная проверка из RFC 8032 (раздел 6); подпись minisign:
алгоритм ED (BLAKE2b-512 от файла), подпись строки trusted comment.
"""
import base64
import hashlib
import json
import sys
import tempfile
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
LATEST_URL = 'https://github.com/Poopseeque/coldz-launcher/releases/latest/download/latest.json'

p = 2**255 - 19
d = -121665 * pow(121666, p - 2, p) % p
q = 2**252 + 27742317777372353535851937790883648493


def _add(P, Q):
    A, B = (P[1] - P[0]) * (Q[1] - Q[0]) % p, (P[1] + P[0]) * (Q[1] + Q[0]) % p
    C, D = 2 * P[3] * Q[3] * d % p, 2 * P[2] * Q[2] % p
    E, F, G, H = B - A, D - C, D + C, B + A
    return (E * F, G * H, F * G, E * H)


def _mul(s, P):
    Q = (0, 1, 1, 0)
    while s > 0:
        if s & 1:
            Q = _add(Q, P)
        P = _add(P, P)
        s >>= 1
    return Q


def _equal(P, Q):
    return (P[0] * Q[2] - Q[0] * P[2]) % p == 0 and (P[1] * Q[2] - Q[1] * P[2]) % p == 0


_sqrt_m1 = pow(2, (p - 1) // 4, p)


def _recover_x(y, sign):
    if y >= p:
        return None
    x2 = (y * y - 1) * pow(d * y * y + 1, p - 2, p)
    if x2 == 0:
        return None if sign else 0
    x = pow(x2, (p + 3) // 8, p)
    if (x * x - x2) % p != 0:
        x = x * _sqrt_m1 % p
    if (x * x - x2) % p != 0:
        return None
    if (x & 1) != sign:
        x = p - x
    return x


_gy = 4 * pow(5, p - 2, p) % p
_gx = _recover_x(_gy, 0)
G = (_gx, _gy, 1, _gx * _gy % p)


def _decompress(s):
    if len(s) != 32:
        return None
    y = int.from_bytes(s, 'little')
    sign = y >> 255
    y &= (1 << 255) - 1
    x = _recover_x(y, sign)
    return None if x is None else (x, y, 1, x * y % p)


def ed25519_verify(public, msg, sig):
    if len(public) != 32 or len(sig) != 64:
        return False
    A, R = _decompress(public), _decompress(sig[:32])
    if not A or not R:
        return False
    s = int.from_bytes(sig[32:], 'little')
    if s >= q:
        return False
    h = int.from_bytes(hashlib.sha512(sig[:32] + public + msg).digest(), 'little') % q
    return _equal(_mul(s, G), _add(R, _mul(h, A)))


def public_key():
    conf = json.loads((ROOT / 'src-tauri' / 'tauri.conf.json').read_text(encoding='utf-8'))
    text = base64.b64decode(conf['plugins']['updater']['pubkey']).decode()
    raw = base64.b64decode(text.strip().splitlines()[1])
    return raw[2:10], raw[10:42]


def check(data: bytes, sig_b64: str):
    """(ключ тот же, подпись файла верна, trusted comment верен, trusted comment)"""
    key_id, pk = public_key()
    lines = base64.b64decode(sig_b64.strip()).decode().splitlines()
    raw = base64.b64decode(lines[1])
    alg, sig_key, sig = raw[:2], raw[2:10], raw[10:74]
    msg = hashlib.blake2b(data, digest_size=64).digest() if alg == b'ED' else data
    trusted = lines[2]
    ok_comment = trusted.startswith('trusted comment: ') and ed25519_verify(
        pk, sig + trusted[len('trusted comment: '):].encode(), base64.b64decode(lines[3]))
    return sig_key == key_id, ed25519_verify(pk, msg, sig), ok_comment, trusted


def fetch(url: str) -> bytes:
    req = urllib.request.Request(url, headers={'User-Agent': 'COLDZ-launcher-verify'})
    with urllib.request.urlopen(req, timeout=60) as r:
        return r.read()


def main(argv):
    # консоль Windows и перенаправление в файл бывают cp1252: кириллица иначе падает
    try:
        sys.stdout.reconfigure(encoding='utf-8', errors='replace')
    except (AttributeError, ValueError):
        pass
    if len(argv) != 2:
        print(__doc__)
        return 1
    if argv[1] == '--url':
        latest = json.loads(fetch(LATEST_URL).decode('utf-8'))
        win = latest['platforms']['windows-x86_64']
        data = fetch(win['url'])
        where = win['url']
    else:
        folder = ROOT / 'release' / argv[1]
        latest = json.loads((folder / 'latest.json').read_text(encoding='utf-8'))
        win = latest['platforms']['windows-x86_64']
        setup = folder / f'COLDZ-Launcher_{argv[1]}_x64-setup.exe'
        data = setup.read_bytes()
        sig_file = (folder / (setup.name + '.sig')).read_text(encoding='utf-8').strip()
        if sig_file != win['signature'].strip():
            print('ОШИБКА: .sig и подпись в latest.json разные')
            return 1
        where = str(setup)
    same_key, ok_file, ok_comment, trusted = check(data, win['signature'])
    print('версия в latest.json:', latest['version'])
    print('ссылка на установщик:', win['url'])
    print('файл:', where, '|', len(data), 'байт | SHA256', hashlib.sha256(data).hexdigest())
    print('ключ тот же, что в tauri.conf.json:', same_key)
    print('подпись файла верна:', ok_file, '| trusted comment верен:', ok_comment)
    print(ascii(trusted))
    good = same_key and ok_file and ok_comment and f'v{latest["version"]}/' in win['url']
    print('ИТОГ:', 'всё сходится' if good else 'НЕ СХОДИТСЯ')
    return 0 if good else 1


if __name__ == '__main__':
    sys.exit(main(sys.argv))
