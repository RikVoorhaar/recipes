#!/usr/bin/env python3
"""Opt-in Stage 4 seeder. Loopback by default; exact preview requires --allow-preview."""
import argparse
import ipaddress
import json
import pathlib
import re
import sys
import urllib.error
import urllib.parse
import urllib.request

REPO_ROOT = pathlib.Path(__file__).resolve().parents[1]
ASSETS = REPO_ROOT / "recipe-app" / "assets" / "stage4"
SOUP_TITLE = "Chicken Soup"
PREVIEW_BASE = "http://100.81.11.110:38080"


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        raise urllib.error.HTTPError(req.full_url, code, "redirect refused", headers, fp)


OPENER = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect)


def validate_base(value, allow_preview=False):
    parsed = urllib.parse.urlsplit(value)
    if parsed.scheme not in ("http", "https") or not parsed.hostname:
        raise ValueError("--base-url must be an absolute HTTP(S) URL")
    if parsed.username or parsed.password or parsed.query or parsed.fragment:
        raise ValueError("--base-url cannot contain credentials, query, or fragment")
    if parsed.path not in ("", "/"):
        raise ValueError("--base-url cannot contain an application path prefix")
    is_preview = (
        parsed.scheme == "http"
        and parsed.hostname == "100.81.11.110"
        and parsed.port == 38080
    )
    if is_preview:
        if not allow_preview:
            raise ValueError("the Tailscale preview requires explicit --allow-preview")
        return PREVIEW_BASE
    if allow_preview:
        raise ValueError("--allow-preview is only valid for the exact pinned Tailscale preview URL")
    try:
        loopback = ipaddress.ip_address(parsed.hostname).is_loopback
    except ValueError:
        loopback = parsed.hostname.lower() == "localhost"
    if not loopback:
        raise ValueError("credentialed seeding is restricted to localhost/loopback targets")
    return value.rstrip("/")


def request(base, token, path, method="GET", body=None, content_type="application/json", auth=True):
    if not path.startswith("/") or path.startswith("//"):
        raise ValueError("invalid API path")
    data = None if body is None else (body if isinstance(body, bytes) else json.dumps(body).encode())
    headers = {"Content-Type": content_type}
    if auth:
        headers["Authorization"] = "Bearer " + token
    req = urllib.request.Request(base + path, data=data, method=method, headers=headers)
    with OPENER.open(req, timeout=30) as response:
        return response.status, response.read()


def fail_http(error):
    if error.code in (301, 302, 303, 307, 308):
        return "redirect refused; credentials were not forwarded"
    return f"HTTP {error.code}; response body withheld"


def slug(title):
    return re.sub(r"-+", "-", "".join(c.lower() if c.isalnum() else "-" for c in title)).strip("-")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base-url", required=True, help="isolated localhost app URL; exact preview URL requires --allow-preview")
    parser.add_argument("--token-file", required=True, type=pathlib.Path)
    parser.add_argument("--apply", action="store_true", help="perform writes (default is dry-run)")
    parser.add_argument("--allow-preview", action="store_true", help="allow the exact pinned Tailscale preview URL; dry-run remains read-only")
    parser.add_argument("--existing-soup-id", help="explicitly opt in to cover-only attach to this existing Chicken Soup ID")
    args = parser.parse_args()
    try:
        base = validate_base(args.base_url, args.allow_preview)
    except ValueError as exc:
        parser.error(str(exc))
    if not args.token_file.is_file():
        parser.error("token file is not readable")
    token = args.token_file.read_text().strip()
    if not token:
        parser.error("token file is empty")

    recipes = json.loads((ASSETS / "manifest.json").read_text())
    try:
        _, list_bytes = request(base, token, "/api/v1/recipes")
        current = json.loads(list_bytes)
    except urllib.error.HTTPError as exc:
        sys.exit(f"Preflight failed: {fail_http(exc)}")
    except (OSError, ValueError, json.JSONDecodeError) as exc:
        sys.exit(f"Preflight failed ({type(exc).__name__})")

    by_title = {}
    for item in current:
        by_title.setdefault(item["title"].casefold(), []).append(item)
    if any(len(items) > 1 for items in by_title.values()):
        sys.exit("Refusing ambiguous target: duplicate current recipe titles exist")

    soup_match = by_title.get(SOUP_TITLE.casefold(), [])
    soup_target = None
    if args.existing_soup_id:
        if len(soup_match) != 1 or soup_match[0].get("id") != args.existing_soup_id:
            sys.exit("Refusing soup opt-in: ID is not the unique current Chicken Soup title match")
        try:
            _, raw = request(base, token, f"/api/v1/recipes/{urllib.parse.quote(args.existing_soup_id, safe='')}")
            soup_target = json.loads(raw)
        except urllib.error.HTTPError as exc:
            sys.exit(f"Soup preflight failed: {fail_http(exc)}")
        expected = recipes[0]["body"]
        if soup_target.get("deleted") or soup_target.get("title") != SOUP_TITLE:
            sys.exit("Refusing soup opt-in: recipe is deleted or title differs")
        if soup_target.get("body") != expected:
            print("SKIP Chicken Soup: body has been edited; no recipe update")
            soup_target = None
        elif soup_target.get("image") is not None:
            print("SKIP Chicken Soup: an image already exists; refusing replacement")
            soup_target = None
        elif not soup_target.get("revision"):
            sys.exit("Refusing soup opt-in: latest revision is missing")
    # Resolve every title before any image upload. Existing exact-title rows are never modified.
    actions = []
    for index, recipe in enumerate(recipes):
        title = recipe["title"]
        if title == SOUP_TITLE and soup_target is not None:
            actions.append((index, "soup-cover", recipe, soup_target))
        elif title.casefold() in by_title:
            if title != SOUP_TITLE or args.existing_soup_id is None:
                if title == SOUP_TITLE and args.existing_soup_id and soup_target is None:
                    continue
                print(f"SKIP {title}: exact-title recipe already exists; no overwrite")
        else:
            actions.append((index, "create", recipe, None))

    for index, kind, recipe, existing in actions:
        image = ASSETS / "covers" / f"{index + 1:02d}-{slug(recipe['title'])}.jpg"
        if not image.is_file() or image.stat().st_size >= 10 * 1024 * 1024:
            sys.exit(f"Cover missing or exceeds API upload limit: {image.name}")
        if not args.apply:
            print(f"PLAN {kind}: {recipe['title']}")
            continue
        try:
            # Recheck the title immediately before each possible upload to avoid
            # creating over a recipe that appeared after the initial preflight.
            _, fresh_list_bytes = request(base, token, "/api/v1/recipes")
            fresh_list = json.loads(fresh_list_bytes)
            exact = [item for item in fresh_list if item["title"].casefold() == recipe["title"].casefold()]
            if len(exact) > 1:
                sys.exit(f"Refusing ambiguous title before upload: {recipe['title']}")
            if kind == "create" and exact:
                print(f"SKIP {recipe['title']}: exact title appeared after preflight")
                continue
            if kind == "soup-cover":
                if len(exact) != 1 or exact[0].get("id") != existing["id"]:
                    sys.exit("Soup title/ID changed before upload; refusing")
                _, fresh_soup_bytes = request(base, token, f"/api/v1/recipes/{urllib.parse.quote(existing['id'], safe='')}")
                fresh_soup = json.loads(fresh_soup_bytes)
                if fresh_soup.get("title") != SOUP_TITLE or fresh_soup.get("body") != recipe["body"]:
                    print("SKIP Chicken Soup: body changed before upload")
                    continue
                if fresh_soup.get("image") is not None:
                    print("SKIP Chicken Soup: an image appeared before upload; refusing replacement")
                    continue
                existing = fresh_soup
            _, uploaded = request(base, token, "/api/v1/images", "POST", image.read_bytes(), "image/jpeg")
            image_ref = json.loads(uploaded)["ref"]
            validation = {key: recipe[key] for key in ("title", "description", "servings", "tags", "source_url", "notes", "body")}
            validation["image_ref"] = image_ref
            try:
                _, validated_bytes = request(base, token, "/api/v1/recipes/validate", "POST", validation)
                validated = json.loads(validated_bytes)
            except urllib.error.HTTPError as exc:
                sys.exit(
                    f"Validation failed after uploading {image_ref}; no recipe was created or changed. "
                    f"Check the API response, then retry only after confirming title {recipe['title']!r} is still absent. "
                    f"({fail_http(exc)})"
                )
            if not validated.get("print_fit", {}).get("fits"):
                sys.exit(
                    f"Validation says {recipe['title']} does not fit one page. No recipe was created; "
                    f"uploaded image {image_ref} is unattached. Shorten the source and validate again."
                )
            if validated.get("warnings"):
                print(f"VALIDATION {recipe['title']}: {len(validated['warnings'])} parser warning(s)")
            if kind == "soup-cover":
                # Re-fetch before attachment; base_revision makes races conflict safely.
                _, latest_bytes = request(base, token, f"/api/v1/recipes/{urllib.parse.quote(existing['id'], safe='')}")
                latest = json.loads(latest_bytes)
                if latest.get("title") != SOUP_TITLE or latest.get("body") != recipe["body"] or latest.get("image") is not None:
                    sys.exit("Soup changed after preflight; uploaded image is unattached and no recipe write occurred")
                recipe_id = existing["id"]
                base_revision = latest["revision"]
            else:
                payload = {key: recipe[key] for key in ("title", "description", "servings", "tags", "source_url", "notes", "body")}
                _, saved = request(base, token, "/api/v1/recipes", "POST", payload)
                created = json.loads(saved)["recipe"]
                recipe_id = created["id"]
                base_revision = created["revision"]
                print(f"CREATED {recipe['title']}: id={recipe_id} revision={base_revision}")
            attach_path = f"/api/v1/recipes/{urllib.parse.quote(recipe_id, safe='')}/image"
            try:
                _, attached_bytes = request(
                    base,
                    token,
                    attach_path,
                    "POST",
                    {"base_revision": base_revision, "image_ref": image_ref},
                )
                attached = json.loads(attached_bytes)
                if not attached.get("image") or attached["image"].get("ref") != image_ref:
                    raise ValueError("attachment response did not confirm the uploaded image")
            except (urllib.error.HTTPError, ValueError, json.JSONDecodeError) as exc:
                detail = fail_http(exc) if isinstance(exc, urllib.error.HTTPError) else "unexpected attachment response"
                sys.exit(
                    f"ATTACH FAILED for recipe id={recipe_id}; image_ref={image_ref}; "
                    f"base_revision={base_revision}. The recipe remains and was not overwritten. "
                    f"Recovery: GET /api/v1/recipes/{recipe_id}, verify the current image is null, then POST "
                    f"/api/v1/recipes/{recipe_id}/image with its current revision as base_revision and "
                    f"image_ref={image_ref}. The seeder will skip this exact-title recipe on retry. ({detail})"
                )
            if kind == "create":
                print(f"ATTACHED cover to {recipe['title']}: id={recipe_id} revision={attached['revision']}")
            else:
                print(f"ATTACHED cover to unchanged Chicken Soup: id={recipe_id} revision={attached['revision']}")
        except urllib.error.HTTPError as exc:
            sys.exit(f"Write failed: {fail_http(exc)}")


if __name__ == "__main__":
    main()
