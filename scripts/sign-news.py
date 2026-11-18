"""Подпись news/news.json ключом новостей (0.5.0). Запускать после КАЖДОЙ правки news.json, до коммита:

    python scripts/sign-news.py

Что делает:
  1. пишет в news.json поле "signedAt" (время подписи, UTC): из двух ответивших адресов лаунчер берёт более новую копию;
  2. сохраняет файл в обычном виде (json.dumps indent=2, ensure_ascii=False, строки LF, перевод строки в конце);
  3. подписывает его секретным ключом новостей `%USERPROFILE%\\.coldz-launcher-keys\\news.key` (ключ НЕ тот, что у
     обновлений; в git, чат и вывод не попадает) через `tauri signer sign` -> news/news.json.sig;
  4. кладёт копии для сайта: release/site/launcher/news.json и news.json.sig (выкладывает их на coldz.vip главная, README 7е).

Без подписи лаунчер 0.5.0+ скачанный news.json не берёт (остаётся на последней хорошей копии или вшитой).
Проверка: `npm test` (тест news_signature) или `python scripts/sign-news.py --check`.
Ключ задаётся и переменной COLDZ_NEWS_KEY (путь к файлу).
"""
import datetime
import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
NEWS = ROOT / 'news' / 'news.json'
SIG = ROOT / 'news' / 'news.json.sig'
SITE = ROOT / 'release' / 'site' / 'launcher'


def tauri_cli():
    """node_modules/.bin/tauri.cmd here, or in the main checkout of this repository (worktrees share it)."""
    cands = [ROOT]
    try:
        common = subprocess.run(['git', '-C', str(ROOT), 'rev-parse', '--path-format=absolute', '--git-common-dir'],
                                capture_output=True, text=True, check=True).stdout.strip()
        cands.append(Path(common).parent)
    except Exception:
        pass
    for c in cands:
        for name in ('tauri.cmd', 'tauri'):
            p = c / 'node_modules' / '.bin' / name
            if p.exists():
                return str(p)
    sys.exit('tauri CLI не найден: npm install в папке лаунчера')


def check():
    """Подпись news.json.sig подходит к news.json открытым ключом из launcher.config.json (через cargo test)."""
    r = subprocess.run(['cargo', 'test', '--manifest-path', str(ROOT / 'src-tauri' / 'Cargo.toml'), 'news_signature'],
                       cwd=ROOT)
    sys.exit(r.returncode)


def main():
    try:
        sys.stdout.reconfigure(encoding='utf-8', errors='replace')
    except Exception:
        pass
    if '--check' in sys.argv:
        check()
    key = Path(os.environ.get('COLDZ_NEWS_KEY') or Path.home() / '.coldz-launcher-keys' / 'news.key')
    if not key.exists():
        sys.exit(f'нет ключа новостей: {key}')
    data = json.loads(NEWS.read_text(encoding='utf-8'))
    data['signedAt'] = datetime.datetime.now(datetime.timezone.utc).strftime('%Y-%m-%dT%H:%M:%SZ')
    NEWS.write_bytes((json.dumps(data, indent=2, ensure_ascii=False) + '\n').encode('utf-8'))
    env = dict(os.environ)
    env.setdefault('TAURI_SIGNING_PRIVATE_KEY_PASSWORD', '')
    env.pop('TAURI_SIGNING_PRIVATE_KEY', None)
    r = subprocess.run([tauri_cli(), 'signer', 'sign', '-f', str(key), str(NEWS)], env=env,
                       stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True)
    if r.returncode != 0 or not SIG.exists():
        sys.exit('подпись не удалась: ' + (r.stderr or '')[-400:])
    # tauri writes the .sig without a line end; keep it so (the launcher trims anyway)
    SITE.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(NEWS, SITE / 'news.json')
    shutil.copyfile(SIG, SITE / 'news.json.sig')
    print(f'подписано: {NEWS.relative_to(ROOT)} ({data["signedAt"]}), копии в {SITE.relative_to(ROOT)}')


if __name__ == '__main__':
    main()
