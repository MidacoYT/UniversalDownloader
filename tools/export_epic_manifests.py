#!/usr/bin/env python3
"""Export BuildPatch manifests for Epic games available through Legendary.

Requirements: pip install legendary-gl
Authentication: legendary auth
"""

import argparse
import csv
import logging
import os
import re
import sys
import time
import unicodedata
from pathlib import Path

try:
    from legendary.core import LegendaryCore
except ImportError:
    sys.exit("Legendary is missing. Install it with: python -m pip install legendary-gl")


def safe_filename(value: str) -> str:
    # Keep filenames Windows-safe and remove trailing dots/spaces.
    value = re.sub(r'[<>:"/\\|?*\x00-\x1f]', '_', value).strip().rstrip('. ')
    if not value:
        value = 'Jeu_sans_nom'
    if value.split('.')[0].upper() in {
        'CON', 'PRN', 'AUX', 'NUL', *{f'COM{i}' for i in range(1, 10)},
        *{f'LPT{i}' for i in range(1, 10)}
    }:
        value = '_' + value
    return value[:150].rstrip('. ')


def normalized_name(value: str) -> str:
    value = unicodedata.normalize('NFKD', value).encode('ascii', 'ignore').decode('ascii')
    return re.sub(r'[^a-z0-9]+', '', value.casefold())


REPORT_FIELDS = [
    'game', 'app_name', 'namespace', 'catalog_item_id', 'build_version',
    'file', 'status', 'details',
]


def save_report(path: Path, rows: dict[str, dict[str, str]]) -> None:
    """Atomically persist progress so Ctrl+C never loses completed lookups."""
    temporary = path.with_suffix(path.suffix + '.part')
    with temporary.open('w', encoding='utf-8-sig', newline='') as f:
        writer = csv.DictWriter(f, fieldnames=REPORT_FIELDS, extrasaction='ignore')
        writer.writeheader()
        writer.writerows(rows.values())
        f.flush()
        os.fsync(f.fileno())
    temporary.replace(path)


def main():
    parser = argparse.ArgumentParser(description='Save Epic game .manifest files without downloading the games.')
    parser.add_argument('--output', default='Epic_Manifests', help='Output directory')
    parser.add_argument('--platform', default='Windows', help='Epic platform (default: Windows)')
    parser.add_argument('--delay', type=float, default=0.4, help='Delay between games, in seconds')
    parser.add_argument('--overwrite', action='store_true', help='Replace manifests that have already been downloaded')
    parser.add_argument('--include-dlc', action='store_true', help='Include available DLC')
    parser.add_argument(
        '--index-only', action='store_true',
        help='Save Epic identifiers for existing manifests without downloading them.',
    )
    args = parser.parse_args()

    logging.basicConfig(level=logging.WARNING, format='[Legendary] %(levelname)s: %(message)s')
    out = Path(args.output)
    out.mkdir(parents=True, exist_ok=True)
    core = LegendaryCore(timeout=30.0)
    if not core.login():
        sys.exit('Could not log in. Run this first: legendary auth')

    print('[+] Fetching Epic library...')
    games, dlcs = core.get_game_and_dlc_list(update_assets=True, platform=args.platform)
    targets = list(games)
    if args.include_dlc:
        for dlc_list in dlcs.values():
            targets.extend(dlc_list)

    # Keep only applications that have an asset for the requested platform.
    targets = [g for g in targets if args.platform in g.asset_infos]
    print(f'[+] Found {len(targets)} applications with a {args.platform} asset.')

    # Prevent filename collisions when multiple apps share the same title.
    title_counts = {}
    for game in targets:
        title = safe_filename(game.app_title)
        title_counts[title.casefold()] = title_counts.get(title.casefold(), 0) + 1

    report = out / '_resultats.csv'
    # Keep earlier progress if a later run is stopped or only indexes a subset.
    report_rows = {}
    if report.is_file():
        try:
            with report.open('r', encoding='utf-8-sig', newline='') as f:
                for row in csv.DictReader(f):
                    key = Path(row.get('file', '')).name.casefold()
                    if key:
                        report_rows[key] = row
        except (OSError, csv.Error) as exc:
            print(f'[!] Could not read the previous report; creating a new one: {exc}')

    # Create a valid report before starting the work, then update it after each
    # game. A hard stop can only lose the game currently being processed.
    save_report(report, report_rows)
    for n, game in enumerate(targets, 1):
        title = safe_filename(game.app_title)
        name = title if title_counts[title.casefold()] == 1 else f'{title} [{safe_filename(game.app_name)}]'
        destination = out / f'{name}.manifest'
        if args.index_only and not destination.is_file():
            normalized_title = normalized_name(title)
            matches = [
                path for path in out.glob('*.manifest')
                if normalized_name(path.stem) == normalized_name(name)
                or normalized_name(path.stem) == normalized_title
            ]
            if len(matches) == 1:
                destination = matches[0]
        print(f'[{n}/{len(targets)}] {game.app_title}', flush=True)

        record = {
            'game': game.app_title,
            'app_name': game.app_name,
            # Legendary already resolved these identifiers from Epic's assets.
            # Save them beside the manifest to avoid title-based guessing.
            'namespace': game.asset_infos[args.platform].namespace,
            'catalog_item_id': game.asset_infos[args.platform].catalog_item_id,
            'build_version': game.app_version(args.platform) or '',
            'file': str(destination),
            'status': '',
            'details': '',
        }
        try:
            if args.index_only:
                if not destination.is_file():
                    continue
                record['status'] = 'INDEXED'
                record['details'] = 'Existing manifest indexed; file left unchanged'
                print(f'    -> identifiers saved; existing manifest kept ({destination.name})')
                report_rows[destination.name.casefold()] = record
                save_report(report, report_rows)
                continue
            if destination.exists() and not args.overwrite:
                record['status'] = 'SKIPPED_EXISTS'
                print('    -> already exists')
            else:
                # This makes an authenticated Epic request, downloads only the
                # .manifest file, and verifies its SHA-1 hash.
                manifest_bytes, _base_urls, _preloaded, _secrets = core.get_cdn_manifest(
                    game, platform=args.platform
                )
                # Write atomically to avoid leaving an incomplete manifest.
                tmp = destination.with_suffix(destination.suffix + '.part')
                tmp.write_bytes(manifest_bytes)
                tmp.replace(destination)
                record['status'] = 'OK'
                record['details'] = f'{len(manifest_bytes)} bytes'
                print(f'    -> {destination.name} ({len(manifest_bytes)} bytes)')
        except KeyboardInterrupt:
            record['status'] = 'INTERRUPTED'
            record['details'] = 'Interrupted by user; metadata saved'
            report_rows[destination.name.casefold()] = record
            save_report(report, report_rows)
            print('\n[!] Interrupted; progress saved to _resultats.csv.')
            break
        except Exception as exc:
            record['status'] = 'ERROR'
            # Do not write signed URLs or tokens to the report.
            record['details'] = type(exc).__name__
            print(f'    -> failed ({type(exc).__name__})')
        report_rows[destination.name.casefold()] = record
        save_report(report, report_rows)
        if args.delay > 0 and n < len(targets):
            time.sleep(args.delay)

    rows = list(report_rows.values())
    ok = sum(x.get('status') == 'OK' for x in rows)
    skipped = sum(x.get('status') in ('SKIPPED_EXISTS', 'INDEXED') for x in rows)
    failed = sum(x.get('status') == 'ERROR' for x in rows)
    print(f'\nDone: {ok} downloaded, {skipped} already present/indexed, {failed} failed.')
    print(f'Output directory: {out.resolve()}')
    print(f'Report: {report}')


if __name__ == '__main__':
    main()
