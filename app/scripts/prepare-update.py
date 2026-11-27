#!/usr/bin/env python3
"""Finalize the release's updater packages, signatures, manifest and checksums.

macOS: the notarized app is stapled, archived and signed. Windows: the NSIS installer
(`pnpm release:windows`, or the CI artifact via --windows-installer) is signed as is.
Both platforms go into one latest.json, because every app reads the latest release's manifest.
"""
import argparse
import datetime
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tarfile

RELEASES = 'https://github.com/tancheng33/tingo/releases/download'
WINDOWS_TARGET = 'x86_64-pc-windows-msvc'


def manifest(version, platforms, notes):
    return {
        'version': version,
        'notes': notes,
        'pub_date': datetime.datetime.now(datetime.timezone.utc).isoformat().replace('+00:00', 'Z'),
        'platforms': {name: {'signature': sig.strip(), 'url': f'{RELEASES}/v{version}/{file.name}'}
                      for name, (file, sig) in platforms.items()},
    }


def sign(app_dir, key, version, path):
    """Sign with the version bound into the signature (the app sets requireSignedVersion)."""
    subprocess.run(['npm', 'run', 'tauri', '--', 'signer', 'sign', '-f', str(key),
                    '--app-version', version, str(path)], cwd=app_dir, check=True,
                   env={**os.environ, 'TAURI_SIGNING_PRIVATE_KEY_PASSWORD': os.environ.get('TAURI_SIGNING_PRIVATE_KEY_PASSWORD', '')},
                   stdout=subprocess.DEVNULL)
    return Path(str(path) + '.sig').read_text()


def windows_installer(app_dir, version, given):
    name = f'Tingo_{version}_x64-setup.exe'
    path = given or app_dir / f'src-tauri/target/{WINDOWS_TARGET}/release/bundle/nsis' / name
    if not path.is_file():
        raise SystemExit(f'Windows installer not found: {path}\n'
                         'Build it with `pnpm release:windows` or pass --windows-installer; '
                         'use --skip-windows only for a macOS-only release.')
    if path.name != name:
        raise SystemExit(f'Windows installer must be {name} (built from this version), got {path.name}')
    with path.open('rb') as f:
        if f.read(2) != b'MZ':
            raise SystemExit(f'Not a Windows executable: {path}')
    return path


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--notes', required=True, type=Path)
    parser.add_argument('--windows-installer', type=Path, help='NSIS installer to publish (default: the release:windows output)')
    parser.add_argument('--skip-windows', action='store_true', help='publish without a Windows update')
    args = parser.parse_args()
    app_dir = Path(__file__).resolve().parents[1]
    cfg = json.loads((app_dir / 'src-tauri/tauri.conf.json').read_text())
    version = cfg['version']
    bundle = app_dir / 'src-tauri/target/release/bundle'
    app = bundle / 'macos/Tingo.app'
    dmg = bundle / f'dmg/Tingo_{version}_aarch64.dmg'
    key = Path(os.environ.get('TAURI_SIGNING_PRIVATE_KEY', str(Path.home() / '.config/tingo-signing/updater.key')))
    if not key.is_file():
        raise SystemExit('Updater signing key is missing')
    setup = None if args.skip_windows else windows_installer(app_dir, version, args.windows_installer)
    requirement = '=identifier "io.github.tancheng33.tingo" and anchor apple generic and certificate leaf[subject.OU] = "9YJL5M4BY8"'
    subprocess.run(['codesign', '--verify', '--deep', '--strict', '-R', requirement, str(app)], check=True)
    subprocess.run(['spctl', '--assess', '--type', 'execute', str(app)], check=True)
    subprocess.run(['xcrun', 'stapler', 'staple', str(app)], check=True)
    subprocess.run(['xcrun', 'stapler', 'validate', str(app)], check=True)
    subprocess.run(['xcrun', 'stapler', 'validate', str(dmg)], check=True)
    archive = bundle / 'macos/Tingo.app.tar.gz'
    # The archive must contain the stapled app, not the pre-notarization copy from tauri build.
    with tarfile.open(archive, 'w:gz') as tar:
        tar.add(app, arcname='Tingo.app')
    platforms = {'darwin-aarch64': (archive, sign(app_dir, key, version, archive))}
    assets = [dmg, archive, Path(str(archive) + '.sig')]
    if setup:
        platforms['windows-x86_64'] = (setup, sign(app_dir, key, version, setup))
        assets += [setup, Path(str(setup) + '.sig')]
    latest = bundle / 'macos/latest.json'
    latest.write_text(json.dumps(manifest(version, platforms, args.notes.read_text()), ensure_ascii=False, indent=2) + '\n')
    assets.append(latest)
    checksums = bundle / 'dmg/checksums.txt'
    checksums.write_text(''.join(f'{hashlib.sha256(p.read_bytes()).hexdigest()}  {p.name}\n' for p in assets))
    print('Ready to upload:')
    for p in [*assets, checksums]:
        print(p)
    if not setup:
        print('WARNING: no Windows update in latest.json; Windows users will not be offered this release')


if __name__ == '__main__':
    main()
