"""MkDocs hooks that publish the repo's Markdown as it is.

README.md and CHANGELOG.md live at the repo root, outside docs_dir, so they are
added as generated pages. Links are written for GitHub (relative to the repo),
so each one is mapped to its page on the site, or to the file on GitHub when
the target is not a page (LICENSE, profiles, the manifest).
"""

import posixpath
import re
from pathlib import Path

from mkdocs.structure.files import File

ROOT = Path(__file__).resolve().parents[2]

# Repo path -> site page, for the Markdown files outside docs/.
ROOT_PAGES = {"README.md": "index.md", "CHANGELOG.md": "changelog.md"}

LINK = re.compile(r"(\]\()([^)\s]+)(\))")


_serving = False


def on_startup(command, dirty):
    global _serving
    _serving = command == "serve"


def on_files(files, config):
    for source, dest in ROOT_PAGES.items():
        content = (ROOT / source).read_text(encoding="utf-8")
        files.append(File.generated(config, dest, content=content))
    if _serving:
        # Port forwarders and preview tools poll /health; answer them locally
        # only, so the published site carries no such file.
        files.append(File.generated(config, "health", content="ok\n"))
    return files


def _repo_path(page):
    """The page's path in the repo, so its links resolve as they do on GitHub."""
    for source, dest in ROOT_PAGES.items():
        if page.file.src_uri == dest:
            return source
    return "docs/" + page.file.src_uri


def _site_path(resolved):
    """The site page for a repo path, or None when it is not a page."""
    if resolved in ROOT_PAGES:
        return ROOT_PAGES[resolved]
    if resolved.startswith("docs/") and resolved.endswith(".md"):
        return resolved[len("docs/"):]
    return None


def _rewrite(target, page, config):
    if re.match(r"^[a-z][a-z0-9+.-]*:", target) or target.startswith("#"):
        return target
    path, _, anchor = target.partition("#")
    resolved = posixpath.normpath(posixpath.join(posixpath.dirname(_repo_path(page)), path))
    fragment = "#" + anchor if anchor else ""
    site = _site_path(resolved)
    if site is not None:
        return posixpath.relpath(site, posixpath.dirname(page.file.src_uri) or ".") + fragment
    branch = "blob" if (ROOT / resolved).is_file() else "tree"
    return f"{config['repo_url'].rstrip('/')}/{branch}/main/{resolved}{fragment}"


def on_page_markdown(markdown, page, config, files):
    return LINK.sub(lambda m: m.group(1) + _rewrite(m.group(2), page, config) + m.group(3), markdown)
