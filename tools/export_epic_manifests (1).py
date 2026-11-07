#!/usr/bin/env python3
"""Exporte les manifestes des jeux accessibles dans la bibliotheque Epic via Legendary.

Prerequis : pip install legendary-gl
Authentification : legendary auth
"""

import argparse
import csv
import logging
import re
import sys
import time
from pathlib import Path

try:
    from legendary.core import LegendaryCore
except ImportError:
    sys.exit("Legendary absent. Installe-le avec : python -m pip install legendary-gl")


def safe_filename(value: str) -> str:
    # Compatible Windows; evite les noms reserves et les points/espaces terminaux.
    value = re.sub(r'[<>:"/\\|?*\x00-\x1f]', '_', value).strip().rstrip('. ')
    if not value:
        value = 'Jeu_sans_nom'
    if value.split('.')[0].upper() in {
        'CON', 'PRN', 'AUX', 'NUL', *{f'COM{i}' for i in range(1, 10)},
        *{f'LPT{i}' for i in range(1, 10)}
    }:
        value = '_' + value
    return value[:150].rstrip('. ')


def main():
    parser = argparse.ArgumentParser(description='Sauvegarder les fichiers .manifest des jeux Epic possedes, sans telecharger les jeux.')
    parser.add_argument('--output', default='Epic_Manifests', help='Dossier de sortie')
    parser.add_argument('--platform', default='Windows', help='Plateforme Epic (defaut : Windows)')
    parser.add_argument('--delay', type=float, default=0.4, help='Delai entre jeux, en secondes')
    parser.add_argument('--overwrite', action='store_true', help='Remplacer les manifestes deja telecharges')
    parser.add_argument('--include-dlc', action='store_true', help='Inclure les DLC disponibles')
    args = parser.parse_args()

    logging.basicConfig(level=logging.WARNING, format='[Legendary] %(levelname)s: %(message)s')
    out = Path(args.output)
    out.mkdir(parents=True, exist_ok=True)
    core = LegendaryCore(timeout=30.0)
    if not core.login():
        sys.exit('Connexion impossible. Lance d\'abord : legendary auth')

    print('[+] Recuperation de la bibliotheque Epic...')
    games, dlcs = core.get_game_and_dlc_list(update_assets=True, platform=args.platform)
    targets = list(games)
    if args.include_dlc:
        for dlc_list in dlcs.values():
            targets.extend(dlc_list)

    # Ne garde que les applications avec un asset pour la plateforme demandee.
    targets = [g for g in targets if args.platform in g.asset_infos]
    print(f'[+] {len(targets)} applications avec asset {args.platform}.')

    # Prevenir les collisions lorsque plusieurs apps ont le meme titre.
    title_counts = {}
    for game in targets:
        title = safe_filename(game.app_title)
        title_counts[title.casefold()] = title_counts.get(title.casefold(), 0) + 1

    results = []
    for n, game in enumerate(targets, 1):
        title = safe_filename(game.app_title)
        name = title if title_counts[title.casefold()] == 1 else f'{title} [{safe_filename(game.app_name)}]'
        destination = out / f'{name}.manifest'
        print(f'[{n}/{len(targets)}] {game.app_title}', flush=True)

        record = {
            'game': game.app_title,
            'app_name': game.app_name,
            'build_version': game.app_version(args.platform) or '',
            'file': str(destination),
            'status': '',
            'details': '',
        }
        try:
            if destination.exists() and not args.overwrite:
                record['status'] = 'SKIPPED_EXISTS'
                print('    -> existe deja')
            else:
                # Cette fonction effectue la requete authentifiee vers Epic,
                # telecharge seulement le .manifest, et verifie son hash SHA-1.
                manifest_bytes, _base_urls, _preloaded, _secrets = core.get_cdn_manifest(
                    game, platform=args.platform
                )
                # Ecriture atomique pour eviter les fichiers incomplets.
                tmp = destination.with_suffix(destination.suffix + '.part')
                tmp.write_bytes(manifest_bytes)
                tmp.replace(destination)
                record['status'] = 'OK'
                record['details'] = f'{len(manifest_bytes)} bytes'
                print(f'    -> {destination.name} ({len(manifest_bytes)} octets)')
        except Exception as exc:
            record['status'] = 'ERROR'
            # Ne pas enregistrer d'URL signee ou de token dans le journal.
            record['details'] = type(exc).__name__
            print(f'    -> echec ({type(exc).__name__})')
        results.append(record)
        if args.delay > 0 and n < len(targets):
            time.sleep(args.delay)

    report = out / '_resultats.csv'
    with report.open('w', encoding='utf-8-sig', newline='') as f:
        writer = csv.DictWriter(f, fieldnames=list(results[0].keys()) if results else
                                ['game', 'app_name', 'build_version', 'file', 'status', 'details'])
        writer.writeheader()
        writer.writerows(results)

    ok = sum(x['status'] == 'OK' for x in results)
    skipped = sum(x['status'] == 'SKIPPED_EXISTS' for x in results)
    failed = sum(x['status'] == 'ERROR' for x in results)
    print(f'\nTermine : {ok} telecharges, {skipped} ignores, {failed} echecs.')
    print(f'Dossier : {out.resolve()}')
    print(f'Rapport : {report}')


if __name__ == '__main__':
    main()
