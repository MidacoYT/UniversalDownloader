#!/usr/bin/env python3
"""Resolve Epic CDN chunk URLs for local BuildPatch manifests.

The script asks for an Epic authorizationCode, exchanges it for a short-lived
access token, then reads BuildPatch manifests itself. It never invokes Legendary
or writes the authorization code/access token to disk.
"""

from __future__ import annotations

import argparse
import base64
import csv
import difflib
import hashlib
import json
import os
import re
import getpass
import struct
import sys
import urllib.error
import urllib.parse
import urllib.request
import unicodedata
import zlib
from pathlib import Path
from typing import Any

ASSETS_URL = "https://launcher-public-service-prod06.ol.epicgames.com/launcher/api/public/assets/Windows?label=Live"
LIBRARY_URL = "https://library-service.live.use1a.on.epicgames.com/library/api/public/items"
MANIFEST_URL = (
    "https://launcher-public-service-prod06.ol.epicgames.com/launcher/api/public/assets/v2/"
    "platform/Windows/namespace/{namespace}/catalogItem/{catalog_item_id}/app/{app_name}/label/Live"
)
USER_AGENT = "UniversalDownloader/0.1 (Epic BuildPatch URL resolver)"
OAUTH_URL = "https://account-public-service-prod03.ol.epicgames.com/account/api/oauth/token"
OAUTH_CLIENT_ID = "34a02cf8f4414e29b15921876da36f9a"
OAUTH_CLIENT_SECRET = "daafbccc737745039dffe53d94fc76cf"
AUTHORIZATION_URL = "https://www.epicgames.com/id/api/redirect"


def exchange_authorization_code(code: str, grant_type: str) -> str:
    credentials = base64.b64encode(f"{OAUTH_CLIENT_ID}:{OAUTH_CLIENT_SECRET}".encode("ascii")).decode("ascii")
    code_field = "exchange_code" if grant_type == "exchange_code" else "code"
    payload = urllib.parse.urlencode({
        "grant_type": grant_type,
        code_field: code,
        "token_type": "eg1",
    }).encode("ascii")
    request = urllib.request.Request(
        OAUTH_URL,
        data=payload,
        headers={
            "Authorization": f"Basic {credentials}",
            "Content-Type": "application/x-www-form-urlencoded",
            "Accept": "application/json",
            "User-Agent": USER_AGENT,
        },
        method="POST",
    )
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            result = json.load(response)
    except urllib.error.HTTPError as exc:
        if exc.code in (400, 401):
            try:
                error = json.loads(exc.read(4096).decode("utf-8", "replace"))
                error_code = error.get("errorCode") or error.get("error")
            except (ValueError, AttributeError):
                error_code = None
            suffix = f" Epic code: {error_code}." if error_code else ""
            raise RuntimeError(
                f"Epic rejected the authorization code (HTTP {exc.code}).{suffix} Codes expire quickly and work once; get a fresh one."
            ) from exc
        raise RuntimeError(f"Epic token exchange failed (HTTP {exc.code}).") from exc
    token = result.get("access_token") if isinstance(result, dict) else None
    if not token:
        raise RuntimeError("Epic's token response did not contain an access_token.")
    return str(token)


def normalize_authorization_input(value: str) -> tuple[str, str]:
    value = value.strip()
    grant_type = "authorization_code"
    if value.startswith("{"):
        try:
            parsed = json.loads(value)
            if parsed.get("exchangeCode"):
                value = str(parsed["exchangeCode"])
                grant_type = "exchange_code"
            else:
                value = str(parsed.get("authorizationCode", value))
        except json.JSONDecodeError:
            pass
    elif value.startswith("http://") or value.startswith("https://"):
        query = urllib.parse.parse_qs(urllib.parse.urlparse(value).query)
        if query.get("exchangeCode"):
            value = query["exchangeCode"][0]
            grant_type = "exchange_code"
        else:
            value = query.get("authorizationCode", query.get("code", [value]))[0]
    return value.strip().strip('"'), grant_type


def request_json(url: str, token: str) -> Any:
    request = urllib.request.Request(
        url,
        headers={"Authorization": f"Bearer {token}", "Accept": "application/json", "User-Agent": USER_AGENT},
    )
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            return json.load(response)
    except urllib.error.HTTPError as exc:
        detail = exc.read(500).decode("utf-8", "replace")
        if exc.code == 401:
            raise RuntimeError("Epic refused the access token (401). Refresh it and retry.") from exc
        if exc.code == 403:
            raise RuntimeError("Epic denied access to this build (403); it may not belong to this account.") from exc
        raise RuntimeError(f"Epic API returned HTTP {exc.code}: {detail}") from exc


def walk_dicts(value: Any):
    if isinstance(value, dict):
        yield value
        for child in value.values():
            yield from walk_dicts(child)
    elif isinstance(value, list):
        for child in value:
            yield from walk_dicts(child)


def normalize_title(value: str) -> str:
    # Titles in Epic's local cache and our manifest index can contain different
    # trademark symbols or a damaged accent; normalize them before matching.
    value = str(value).replace("™", " ").replace("®", " ").replace("\ufffd", " ").replace("?", " ")
    value = unicodedata.normalize("NFKD", value).encode("ascii", "ignore").decode("ascii").casefold()
    return re.sub(r"[^a-z0-9]+", "", value)


def load_catalog_cache(path: Path) -> list[dict[str, Any]]:
    if not path.is_file():
        return []
    try:
        contents = base64.b64decode(path.read_bytes().strip())
        data = json.loads(contents)
    except (OSError, ValueError, base64.binascii.Error) as exc:
        print(f"Epic catalog cache could not be read: {exc}", file=sys.stderr)
        return []
    return data if isinstance(data, list) else []


def title_from_item(item: dict[str, Any]) -> str:
    catalog_item = item.get("catalogItem") or item.get("catalog_item") or {}
    if not isinstance(catalog_item, dict):
        catalog_item = {}
    return str(item.get("title") or catalog_item.get("title") or "")


def match_title_asset(title: str, candidates: list[dict[str, str]]) -> dict[str, str] | None:
    normalized = normalize_title(title)
    exact = {}
    scored: dict[tuple[str, str, str], tuple[float, dict[str, str]]] = {}
    for item in candidates:
        candidate_title = item.get("catalog_title", "")
        candidate_normalized = normalize_title(candidate_title)
        score = difflib.SequenceMatcher(None, normalized, candidate_normalized).ratio()
        key = (item.get("app_name", ""), item.get("namespace", ""), item.get("catalog_item_id", ""))
        if key not in scored or score > scored[key][0]:
            scored[key] = (score, item)
        if normalized and candidate_normalized == normalized:
            exact[key] = item
    if len(exact) == 1:
        return next(iter(exact.values()))
    if len(exact) > 1:
        return None
    ranked = sorted(scored.values(), key=lambda pair: pair[0], reverse=True)
    if not ranked or ranked[0][0] < 0.94:
        return None
    if len(ranked) > 1 and ranked[0][0] - ranked[1][0] < 0.05:
        return None
    return ranked[0][1]


def catalog_cache_match(
    app_name: str, title: str, cache: list[dict[str, Any]]
) -> dict[str, str] | None:
    candidates = []
    for item in cache:
        if not isinstance(item, dict):
            continue
        namespace = item.get("namespace")
        catalog_item_id = item.get("id") or item.get("catalogItemId")
        catalog_title = item.get("title")
        if not namespace or not catalog_item_id or not catalog_title:
            continue
        release_app_names = []
        for release in item.get("releaseInfo", []) or []:
            if isinstance(release, dict) and release.get("appId"):
                release_app_names.append(str(release["appId"]))
        candidates.append({
            "app_name": app_name,
            "namespace": str(namespace),
            "catalog_item_id": str(catalog_item_id),
            "source": "local-catalog-cache",
            "catalog_title": str(catalog_title),
            "release_app_names": "|".join(dict.fromkeys(release_app_names)),
        })
    return match_title_asset(title, candidates)


def indexed_asset(metadata: dict[str, str], app_name: str) -> dict[str, str] | None:
    """Build an Epic asset from identifiers saved by the Legendary exporter."""
    namespace = str(metadata.get("namespace", "")).strip()
    catalog_item_id = str(metadata.get("catalog_item_id", "")).strip()
    if not namespace or not catalog_item_id:
        return None
    return {
        "app_name": app_name,
        "namespace": namespace,
        "catalog_item_id": catalog_item_id,
        "source": "manifest-index",
        "catalog_title": str(metadata.get("game", "")).strip(),
    }


def load_asset_map(token: str) -> tuple[dict[str, dict[str, str]], list[dict[str, str]]]:
    assets: dict[str, dict[str, str]] = {}
    title_assets: list[dict[str, str]] = []

    # Assets is not a complete account-library index for every older/retired title.
    # Legendary resolves these app identifiers from Epic's paginated Library API.
    try:
        cursor = None
        seen_cursors = set()
        while True:
            params = {"includeMetadata": "true"}
            if cursor:
                params["cursor"] = cursor
            url = LIBRARY_URL + "?" + urllib.parse.urlencode(params)
            response = request_json(url, token)
            records = response.get("records", []) if isinstance(response, dict) else []
            for item in records:
                if not isinstance(item, dict):
                    continue
                app_name = item.get("appName") or item.get("app_name")
                namespace = item.get("namespace") or item.get("catalogNamespace")
                catalog_item_id = item.get("catalogItemId") or item.get("catalog_item_id")
                if app_name and namespace and catalog_item_id:
                    asset = {
                        "app_name": str(app_name),
                        "namespace": str(namespace),
                        "catalog_item_id": str(catalog_item_id),
                        "source": "library",
                        "catalog_title": title_from_item(item),
                    }
                    assets[str(app_name).casefold()] = asset
                    if asset["catalog_title"]:
                        title_assets.append(asset)
            metadata = response.get("responseMetadata", {}) if isinstance(response, dict) else {}
            next_cursor = metadata.get("nextCursor") if isinstance(metadata, dict) else None
            if not next_cursor or next_cursor in seen_cursors:
                break
            seen_cursors.add(next_cursor)
            cursor = str(next_cursor)
    except (RuntimeError, urllib.error.URLError) as exc:
        print(f"Epic Library lookup unavailable; trying Assets endpoint: {exc}", file=sys.stderr)

    try:
        response = request_json(ASSETS_URL, token)
        for item in walk_dicts(response):
            app_name = item.get("appName") or item.get("app_name")
            namespace = item.get("namespace") or item.get("catalogNamespace")
            catalog_item_id = item.get("catalogItemId") or item.get("catalog_item_id")
            if app_name and namespace and catalog_item_id:
                asset = {
                    "app_name": str(app_name),
                    "namespace": str(namespace),
                    "catalog_item_id": str(catalog_item_id),
                    "source": "assets",
                    "catalog_title": title_from_item(item),
                }
                assets.setdefault(str(app_name).casefold(), asset)
                if asset["catalog_title"]:
                    title_assets.append(asset)
    except (RuntimeError, urllib.error.URLError) as exc:
        print(f"Epic Assets lookup unavailable; using Library results: {exc}", file=sys.stderr)

    if not assets:
        raise RuntimeError("Epic Library and Assets returned no appName/namespace/catalogItemId entries.")
    return assets, title_assets


def epic_guid(raw: bytes) -> tuple[int, int, int, int]:
    if len(raw) != 16:
        raise ValueError("Invalid chunk GUID bytes")
    return struct.unpack("<IIII", raw)


def b64url(data: bytes) -> str:
    return base64.urlsafe_b64encode(data).decode("ascii").rstrip("=")


def parse_manifest_chunks(data: bytes, source_name: str) -> tuple[int, str, list[dict[str, Any]]]:
    if len(data) < 41 or struct.unpack_from("<I", data, 0)[0] != 0x44BEC00C:
        raise ValueError(f"{source_name}: not an Epic BuildPatch manifest")

    header_size, uncompressed_size, compressed_size = struct.unpack_from("<III", data, 4)
    expected_sha1 = data[16:36]
    stored_as = data[36]
    manifest_version = struct.unpack_from("<I", data, 37)[0]
    if header_size < 41 or header_size > len(data):
        raise ValueError(f"{source_name}: invalid manifest header size")
    body = data[header_size:]
    if len(body) != compressed_size:
        raise ValueError(f"{source_name}: compressed body length does not match header")
    if stored_as & 1:
        body = zlib.decompress(body)
    if len(body) != uncompressed_size or hashlib.sha1(body).digest() != expected_sha1:
        raise ValueError(f"{source_name}: manifest body size or SHA-1 check failed")
    if len(body) < 9:
        raise ValueError(f"{source_name}: truncated manifest metadata")

    meta_size = struct.unpack_from("<I", body, 0)[0]
    feature_level = struct.unpack_from("<I", body, 5)[0]
    if meta_size < 9 or meta_size + 9 > len(body):
        raise ValueError(f"{source_name}: invalid metadata section size")
    cursor = meta_size
    cdl_size, _cdl_version, count = struct.unpack_from("<IBI", body, cursor)
    cdl_start = cursor
    cursor += 9
    if count > 5_000_000:
        raise ValueError(f"{source_name}: unreasonable chunk count")

    def take(size: int) -> bytes:
        nonlocal cursor
        end = cursor + size
        if size < 0 or end > len(body) or end > cdl_start + cdl_size:
            raise ValueError(f"{source_name}: truncated chunk data list")
        result = body[cursor:end]
        cursor = end
        return result

    guids = [epic_guid(take(16)) for _ in range(count)]
    hashes = [struct.unpack("<Q", take(8))[0] for _ in range(count)]
    _sha1s = [take(20) for _ in range(count)]
    _stored_groups = take(count)
    _window_sizes = [struct.unpack("<I", take(4))[0] for _ in range(count)]
    sizes = [struct.unpack("<q", take(8))[0] for _ in range(count)]
    secrets = [(0, 0, 0, 0)] * count
    if feature_level >= 22:
        secrets = [epic_guid(take(16)) for _ in range(count)]
        _compressed_window_sizes = [struct.unpack("<I", take(4))[0] for _ in range(count)]
        _encryption_tags = [take(16) for _ in range(count)]
    if cursor > cdl_start + cdl_size:
        raise ValueError(f"{source_name}: chunk data list exceeded its declared size")

    if feature_level >= 22:
        chunk_dir = "ChunksV5"
    elif feature_level >= 15:
        chunk_dir = "ChunksV4"
    elif feature_level >= 6:
        chunk_dir = "ChunksV3"
    elif feature_level >= 3:
        chunk_dir = "ChunksV2"
    else:
        chunk_dir = "Chunks"

    chunks = []
    for guid, chunk_hash, size, secret in zip(guids, hashes, sizes, secrets):
        guid_bytes = struct.pack("<IIII", *guid)
        group = zlib.crc32(guid_bytes) % 100
        if feature_level >= 22:
            secret_path = b64url(struct.pack("<IIII", *secret)) if any(secret) else "plain"
            hash_part = b64url(struct.pack("<Q", chunk_hash))
            guid_part = b64url(guid_bytes)
            relative = f"{chunk_dir}/{secret_path}/{group:02d}/{hash_part}_{guid_part}.chunk"
        else:
            hash_part = f"{chunk_hash:016X}"
            guid_part = "".join(f"{part:08X}" for part in guid)
            relative = f"{chunk_dir}/{group:02d}/{hash_part}_{guid_part}.chunk"
        chunks.append({
            "group": group,
            "hash": hash_part,
            "guid": guid_part,
            "path": relative,
            "size": size,
        })
    return feature_level, chunk_dir, chunks


def read_manifest_chunks(path: Path) -> tuple[int, str, list[dict[str, Any]]]:
    return parse_manifest_chunks(path.read_bytes(), path.name)


def cdn_bases(token: str, app_name: str, asset: dict[str, str]) -> list[str]:
    url = MANIFEST_URL.format(
        namespace=urllib.parse.quote(asset["namespace"], safe=""),
        catalog_item_id=urllib.parse.quote(asset["catalog_item_id"], safe=""),
        app_name=urllib.parse.quote(app_name, safe=""),
    )
    response = request_json(url, token)
    elements = response.get("elements", []) if isinstance(response, dict) else []
    if not elements:
        raise RuntimeError(f"No manifest returned by Epic for {app_name}.")
    bases = []
    for element in elements:
        for manifest in element.get("manifests", []):
            uri = manifest.get("uri")
            if uri:
                base = uri.rpartition("/")[0].rstrip("/")
                if base and base not in bases:
                    bases.append(base)
    if not bases:
        raise RuntimeError(f"Epic returned no CDN base URL for {app_name}.")
    return bases


def split_cloud_dir(base_url: str) -> dict[str, str | None]:
    parts = [urllib.parse.unquote(part) for part in urllib.parse.urlsplit(base_url).path.split("/") if part]
    org_index = next((index for index, part in enumerate(parts) if part.lower() == "org"), None)
    if org_index is None or len(parts) < org_index + 3:
        return {"organization": None, "build": None, "cloudDir": None}
    tail = parts[org_index + 1:]
    return {
        "organization": tail[0],
        "build": tail[1],
        "cloudDir": "/".join(tail[2:]),
    }


def load_manifest_index(path: Path) -> dict[str, dict[str, str]]:
    """Read the optional CSV emitted by export_epic_manifests.py."""
    if not path.is_file():
        return {}
    result = {}
    with path.open("r", encoding="utf-8-sig", newline="") as source:
        for row in csv.DictReader(source):
            filename = Path(row.get("file", "")).name
            if filename:
                result[filename.casefold()] = row
    return result


def write_text_atomic(path: Path, content: str) -> None:
    temporary = path.with_suffix(path.suffix + ".part")
    temporary.write_text(content, encoding="utf-8")
    temporary.replace(path)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest-dir", type=Path, default=Path("Epic_Manifests"))
    parser.add_argument(
        "--manifest-index",
        type=Path,
        help="Optional _resultats.csv emitted by export_epic_manifests.py (defaults to <manifest-dir>/_resultats.csv).",
    )
    parser.add_argument("--output", type=Path, default=Path("epic_chunk_urls"))
    program_data = Path(os.environ.get("PROGRAMDATA", r"C:\ProgramData"))
    parser.add_argument(
        "--catalog-cache",
        type=Path,
        default=program_data / "Epic" / "EpicGamesLauncher" / "Data" / "Catalog" / "catcache.bin",
    )
    args = parser.parse_args()
    if not args.manifest_dir.is_dir():
        print(f"Missing manifest directory: {args.manifest_dir}", file=sys.stderr)
        return 2
    manifests = sorted(args.manifest_dir.glob("*.manifest"), key=lambda path: path.name.casefold())
    if not manifests:
        print(f"No .manifest files found in {args.manifest_dir}", file=sys.stderr)
        return 2

    try:
        authorization_link = AUTHORIZATION_URL + "?" + urllib.parse.urlencode({
            "clientId": OAUTH_CLIENT_ID,
            "responseType": "code",
        })
        print("\nOpen this link in a browser signed in to Epic Games:")
        print(authorization_link)
        print("Copy the authorizationCode field from Epic's response.\n")
        code, grant_type = normalize_authorization_input(getpass.getpass(
            "Epic authorizationCode, JSON, or response URL: "
        ))
        if not code:
            print("No authorization code was entered.", file=sys.stderr)
            return 2
        token = exchange_authorization_code(code, grant_type)
        index_path = args.manifest_index or (args.manifest_dir / "_resultats.csv")
        manifest_index = load_manifest_index(index_path)
        asset_map, title_assets = load_asset_map(token)
        local_catalog = load_catalog_cache(args.catalog_cache)
        args.output.mkdir(parents=True, exist_ok=True)
        chunk_data_dir = args.output / "chunks"
        chunk_data_dir.mkdir(parents=True, exist_ok=True)
        summary_path = args.output / "summary.json"
        try:
            old_summary = json.loads(summary_path.read_text(encoding="utf-8"))
        except (OSError, ValueError):
            old_summary = []
        summary_by_app = {
            str(item["appName"]): item
            for item in old_summary
            if isinstance(item, dict) and item.get("appName")
        }
        summary_order = []

        def save_summary() -> None:
            ordered = [summary_by_app[key] for key in summary_order if key in summary_by_app]
            ordered_keys = set(summary_order)
            ordered.extend(value for key, value in summary_by_app.items() if key not in ordered_keys)
            write_text_atomic(
                summary_path,
                json.dumps(ordered, indent=2, ensure_ascii=False) + "\n",
            )

        for manifest_path in manifests:
            metadata = manifest_index.get(manifest_path.name.casefold(), {})
            app_name = str(metadata.get("app_name", "")).strip()
            title = str(metadata.get("game", "")).strip() or manifest_path.stem
            try:
                candidates = []
                # The exporter obtains these exact IDs from Legendary's Epic
                # asset data; prefer them to fuzzy title matching.
                direct_indexed_asset = indexed_asset(metadata, app_name)
                if direct_indexed_asset:
                    candidates.append(direct_indexed_asset)
                if app_name:
                    direct_asset = asset_map.get(app_name.casefold())
                    if direct_asset:
                        candidates.append(direct_asset)
                title_asset = match_title_asset(title, title_assets)
                if title_asset:
                    candidates.append(title_asset)
                cache_asset = catalog_cache_match(app_name, title, local_catalog) if app_name else None
                if cache_asset:
                    for release_app_name in cache_asset.get("release_app_names", "").split("|"):
                        if release_app_name:
                            candidates.append({
                                **cache_asset,
                                "app_name": release_app_name,
                                "source": "local-catalog-releaseInfo",
                            })
                    candidates.append(cache_asset)
                unique_candidates = []
                seen = set()
                for candidate in candidates:
                    key = (
                        candidate.get("app_name", ""),
                        candidate.get("namespace", ""),
                        candidate.get("catalog_item_id", ""),
                    )
                    if key not in seen:
                        seen.add(key)
                        unique_candidates.append(candidate)
                if not unique_candidates:
                    raise RuntimeError(
                        "No appName/namespace/catalogItemId match in the manifest index, "
                        "Epic account data, or local catalog cache"
                    )
                feature_level, chunk_dir, chunks = read_manifest_chunks(manifest_path)
                asset = None
                bases = None
                resolver_errors = []
                for candidate in unique_candidates:
                    try:
                        resolved = cdn_bases(token, candidate["app_name"], candidate)
                        asset = candidate
                        bases = resolved
                        break
                    except RuntimeError as exc:
                        resolver_errors.append(f"{candidate.get('source')} / {candidate.get('app_name')}: {exc}")
                if asset is None or bases is None:
                    details = " | ".join(resolver_errors)
                    raise RuntimeError(details or "Epic returned no CDN metadata")
                resolved_app_name = asset.get("app_name", app_name or manifest_path.stem)
                output_app_name = app_name or resolved_app_name
                urls = [f"{base}/{chunk['path']}" for chunk in chunks for base in bases]
                filename = f"{manifest_path.stem}.txt"
                write_text_atomic(args.output / filename, "\n".join(urls) + "\n")
                cloud = split_cloud_dir(bases[0])
                chunks_filename = f"{manifest_path.stem}.json"
                chunk_data = {
                    "name": title,
                    "appName": output_app_name,
                    "epicAppName": resolved_app_name,
                    "namespace": asset["namespace"],
                    "catalogItemId": asset["catalog_item_id"],
                    "metadataSource": asset.get("source"),
                    "catalogTitle": asset.get("catalog_title"),
                    "manifestVersion": feature_level,
                    "chunkDir": chunk_dir,
                    "chunkCount": len(chunks),
                    **cloud,
                    "cdnBases": bases,
                    "chunks": chunks,
                }
                write_text_atomic(
                    chunk_data_dir / chunks_filename,
                    json.dumps(chunk_data, separators=(",", ":"), ensure_ascii=False) + "\n",
                )
                summary_by_app[output_app_name] = {
                    "name": title,
                    "appName": output_app_name,
                    "metadataSource": asset.get("source"),
                    "catalogTitle": asset.get("catalog_title"),
                    "manifestVersion": feature_level,
                    "chunkCount": len(chunks),
                    "chunkDir": chunk_dir,
                    **cloud,
                    "cdnBases": bases,
                    "urlFile": filename,
                    "chunkDataFile": f"chunks/{chunks_filename}",
                }
                summary_order.append(output_app_name)
                save_summary()
                print(
                    f"{title}: {len(chunks)} chunks, {len(bases)} CDN(s) "
                    f"(metadata source={asset.get('source')}, Epic appName={resolved_app_name})"
                )
            except KeyboardInterrupt:
                save_summary()
                print("\nInterrupted; completed game results are saved in summary.json.", file=sys.stderr)
                break
            except Exception as exc:
                print(f"Skip {title}: {exc}", file=sys.stderr)
        save_summary()
        print(f"Wrote results to {args.output.resolve()}")
        return 0
    except (OSError, ValueError, RuntimeError, urllib.error.URLError) as exc:
        print(f"Could not resolve Epic chunk URLs: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
