#!/usr/bin/env python3
"""Build the documentation site from docs/*.md and docs/openapi.json.

Standard library only. The Markdown converter below covers the subset the
repository's documents use (ATX headings, paragraphs, fenced code, pipe
tables, nested lists with indented continuation blocks, block quotes, rules
and the usual inline forms); it is not a general CommonMark implementation.

    python site/build.py                       # build into site/_build
    python site/build.py --check-links         # build, then fail on a broken internal link
    python site/build.py --base-url URL --out DIR
"""

from __future__ import annotations

import argparse
import html
import json
import os
import posixpath
import re
import shutil
import sys
from dataclasses import dataclass
from datetime import date
from html.parser import HTMLParser
from pathlib import Path
from urllib.parse import urlsplit

ROOT = Path(__file__).resolve().parent.parent
SITE_DIR = ROOT / "site"
DEFAULT_BASE_URL = "https://sskutushev.github.io/crypto-gateway-project/"
REPOSITORY_URL = "https://github.com/Sskutushev/crypto-gateway-project"
SITE_NAME = "Crypto Gateway"


@dataclass(frozen=True)
class Page:
    source: str  # repository-relative path of the Markdown source
    output: str  # site-relative output file
    title: str
    description: str
    nav: str | None = None  # label in the top navigation, if listed there
    # Where relative links in the source resolve from. The landing page is
    # authored as if it sat at the repository root.
    link_base: str | None = None


PAGES: list[Page] = [
    Page(
        "site/content/index.md",
        "index.html",
        "Crypto Gateway — open-source, non-custodial USDT TRC20 payment gateway",
        "A self-hosted, non-custodial payment gateway in Rust: USDT TRC20 paid straight "
        "to the merchant's own address, exact integer money, independent chain evidence, "
        "signed webhooks, PostgreSQL as the source of truth.",
        nav="Overview",
        link_base="index.md",
    ),
    Page(
        "docs/merchant-integration.md",
        "merchant-integration.html",
        "Merchant integration",
        "Create a payment intent, quote it in USDT TRC20, read its status and verify the "
        "signed webhook: the whole merchant integration of the Crypto Gateway.",
        nav="Integrate",
    ),
    Page(
        "docs/scope-and-limits.md",
        "scope-and-limits.html",
        "Scope and limits",
        "What the gateway supports today and what happens on underpayment, overpayment, "
        "late payment, the wrong token or the wrong network.",
        nav="Scope",
    ),
    Page(
        "docs/architecture.md",
        "architecture.html",
        "Architecture",
        "Trust boundaries, services, database roles, the money model and delivery "
        "semantics of the non-custodial Crypto Gateway.",
        nav="Architecture",
    ),
    Page(
        "docs/threat-model.md",
        "threat-model.html",
        "Threat model",
        "The attacks the gateway is designed to refuse: lying providers, compromised "
        "observers, double claims, reorganisations, forged webhooks and stale evidence.",
        nav="Security",
    ),
    Page(
        "docs/money-invariants.md",
        "money-invariants.html",
        "Money invariants",
        "Each money invariant of the gateway and the tests that try to falsify it.",
    ),
    Page(
        "docs/deployment.md",
        "deployment.html",
        "Deployment",
        "Deploying the gateway with Docker Compose or Kubernetes: topology, database "
        "roles, the TLS boundary, order of operations and rotation.",
        nav="Deploy",
    ),
    Page(
        "docs/owner-setup.md",
        "owner-setup.html",
        "Owner setup",
        "Every account, key, secret and registration an owner provides before the "
        "first real payment, in order, with what is blocking.",
    ),
    Page(
        "docs/operator-runbook.md",
        "operator-runbook.html",
        "Operator runbook",
        "Feeding price evidence, reading queues, resolving parked money, onboarding "
        "merchants with the admin CLI, clearing a hard stop and alerts to set.",
        nav="Operate",
    ),
    Page(
        "docs/releasing.md",
        "releasing.html",
        "Releasing and upgrading",
        "Versioning, the schema and API compatibility matrix, the upgrade procedure "
        "with a migrate-only step, rollback rules and verifying signed images.",
    ),
    Page(
        "docs/backup-and-restore.md",
        "backup-and-restore.html",
        "Backup and restore",
        "Recovery objectives, PostgreSQL backups and point-in-time recovery, the "
        "monthly restore drill and what to verify after a restore.",
    ),
    Page(
        "docs/implementation-status.md",
        "implementation-status.html",
        "Implementation status",
        "What is implemented and verified, what is in progress, and what still "
        "stands between the gateway and production use.",
        nav="Status",
    ),
]

API_OUTPUT = "api.html"
API_SOURCE = "docs/openapi.json"


# --------------------------------------------------------------------------
# Markdown


def slugify(text: str) -> str:
    text = re.sub(r"<[^>]+>", "", text)
    text = html.unescape(text).strip().lower()
    text = re.sub(r"[^\w\- ]", "", text)
    return re.sub(r" ", "-", text)


class Inline:
    """Inline Markdown to HTML. `link` rewrites every link target."""

    def __init__(self, link):
        self.link = link

    def render(self, text: str) -> str:
        stash: list[str] = []

        def keep(fragment: str) -> str:
            stash.append(fragment)
            return f"\x00{len(stash) - 1}\x00"

        text = re.sub(
            r"(`+)(.+?)\1",
            lambda m: keep(f"<code>{html.escape(m.group(2).replace(chr(10), ' ').strip(), quote=False)}</code>"),
            text,
            flags=re.S,
        )

        def link(m: re.Match) -> str:
            label, target = m.group(1), m.group(2).strip()
            href = self.link(target)
            external = urlsplit(href).scheme in ("http", "https")
            rel = ' rel="noopener"' if external else ""
            return keep(f'<a href="{html.escape(href)}"{rel}>') + label + keep("</a>")

        text = re.sub(r"\[([^\]]+)\]\(([^)\s]+)\)", link, text)
        text = re.sub(
            r"<(https?://[^>\s]+)>",
            lambda m: keep(f'<a href="{html.escape(m.group(1))}" rel="noopener">{html.escape(m.group(1))}</a>'),
            text,
        )
        text = html.escape(text, quote=False)
        text = re.sub(r"\*\*(.+?)\*\*", r"<strong>\1</strong>", text, flags=re.S)
        text = re.sub(r"(?<![\w*])\*(?!\s)(.+?)(?<!\s)\*(?![\w*])", r"<em>\1</em>", text, flags=re.S)
        text = re.sub(r"(?<![\w_])_(?!\s)(.+?)(?<!\s)_(?![\w_])", r"<em>\1</em>", text)
        text = text.replace(" —", "&nbsp;—")
        for _ in range(3):
            text = re.sub(r"\x00(\d+)\x00", lambda m: stash[int(m.group(1))], text)
        return text


LIST_MARKER = re.compile(r"^( {0,3})([-*+]|\d{1,9}[.)])( +|$)")
FENCE = re.compile(r"^( {0,3})(```+|~~~+)\s*([\w+-]*)\s*$")
HEADING = re.compile(r"^ {0,3}(#{1,6})\s+(.*?)\s*#*\s*$")
RULE = re.compile(r"^ {0,3}([-*_])( *\1){2,} *$")
TABLE_SEPARATOR = re.compile(r"^\s*\|?\s*:?-{3,}:?\s*(\|\s*:?-{3,}:?\s*)*\|?\s*$")


class Markdown:
    def __init__(self, link):
        self.inline = Inline(link)
        self.headings: list[tuple[int, str, str]] = []
        self._ids: dict[str, int] = {}

    def convert(self, text: str) -> str:
        lines = text.replace("\r\n", "\n").replace("\t", "    ").split("\n")
        return self.blocks(lines)

    def heading_id(self, text: str) -> str:
        base = slugify(text) or "section"
        n = self._ids.get(base, 0)
        self._ids[base] = n + 1
        return base if n == 0 else f"{base}-{n}"

    def blocks(self, lines: list[str]) -> str:
        out: list[str] = []
        i = 0
        while i < len(lines):
            line = lines[i]
            if not line.strip():
                i += 1
                continue

            m = FENCE.match(line)
            if m:
                indent, fence, lang = len(m.group(1)), m.group(2), m.group(3)
                body: list[str] = []
                i += 1
                while i < len(lines) and not re.match(rf"^ {{0,3}}{re.escape(fence[0])}{{{len(fence)},}}\s*$", lines[i]):
                    body.append(lines[i][indent:] if lines[i][:indent].strip() == "" else lines[i])
                    i += 1
                i += 1
                cls = f' class="language-{html.escape(lang)}"' if lang else ""
                out.append(f"<pre><code{cls}>{html.escape(chr(10).join(body), quote=False)}</code></pre>")
                continue

            m = HEADING.match(line)
            if m:
                level, content = len(m.group(1)), m.group(2)
                rendered = self.inline.render(content)
                anchor = self.heading_id(rendered)
                self.headings.append((level, anchor, re.sub(r"<[^>]+>", "", rendered)))
                out.append(
                    f'<h{level} id="{anchor}">{rendered}'
                    f'<a class="anchor" href="#{anchor}" aria-label="Link to this section">#</a></h{level}>'
                )
                i += 1
                continue

            if RULE.match(line):
                out.append("<hr>")
                i += 1
                continue

            if line.lstrip().startswith(">"):
                quoted: list[str] = []
                while i < len(lines) and lines[i].lstrip().startswith(">"):
                    quoted.append(re.sub(r"^\s*> ?", "", lines[i]))
                    i += 1
                out.append(f"<blockquote>{self.blocks(quoted)}</blockquote>")
                continue

            if "|" in line and i + 1 < len(lines) and TABLE_SEPARATOR.match(lines[i + 1]):
                rows = [line]
                i += 2
                while i < len(lines) and "|" in lines[i] and lines[i].strip():
                    rows.append(lines[i])
                    i += 1
                out.append(self.table(rows))
                continue

            if LIST_MARKER.match(line):
                html_list, i = self.list_block(lines, i)
                out.append(html_list)
                continue

            para = [line.strip()]
            i += 1
            while i < len(lines) and lines[i].strip() and not self.interrupts(lines, i):
                para.append(lines[i].strip())
                i += 1
            out.append(f"<p>{self.inline.render(chr(10).join(para))}</p>")
        return "\n".join(out)

    @staticmethod
    def interrupts(lines: list[str], i: int) -> bool:
        line = lines[i]
        return bool(
            FENCE.match(line)
            or HEADING.match(line)
            or RULE.match(line)
            or line.lstrip().startswith(">")
            or LIST_MARKER.match(line)
            or ("|" in line and i + 1 < len(lines) and TABLE_SEPARATOR.match(lines[i + 1]))
        )

    @staticmethod
    def split_row(row: str) -> list[str]:
        row = row.strip()
        if row.startswith("|"):
            row = row[1:]
        if row.endswith("|") and not row.endswith("\\|"):
            row = row[:-1]
        cells, current, in_code = [], "", False
        for ch in row:
            if ch == "`":
                in_code = not in_code
            if ch == "|" and not in_code:
                cells.append(current.strip())
                current = ""
            else:
                current += ch
        cells.append(current.strip())
        return cells

    def table(self, rows: list[str]) -> str:
        head = self.split_row(rows[0])
        out = ['<div class="table"><table>', "<thead><tr>"]
        out += [f"<th>{self.inline.render(c)}</th>" for c in head]
        out.append("</tr></thead><tbody>")
        for row in rows[1:]:
            cells = self.split_row(row)
            cells += [""] * (len(head) - len(cells))
            out.append("<tr>" + "".join(f"<td>{self.inline.render(c)}</td>" for c in cells[: len(head)]) + "</tr>")
        out.append("</tbody></table></div>")
        return "".join(out)

    def list_block(self, lines: list[str], i: int) -> tuple[str, int]:
        first = LIST_MARKER.match(lines[i])
        ordered = first.group(2)[0].isdigit()
        base_indent = len(first.group(1))
        items: list[list[str]] = []
        loose = False
        start = int(first.group(2)[:-1]) if ordered else 1
        while i < len(lines):
            m = LIST_MARKER.match(lines[i])
            if not m or len(m.group(1)) != base_indent or m.group(2)[0].isdigit() != ordered:
                break
            content_indent = len(m.group(0)) if m.group(3) else len(m.group(0)) + 1
            item = [lines[i][len(m.group(0)):]]
            i += 1
            while i < len(lines):
                line = lines[i]
                if not line.strip():
                    j = i
                    while j < len(lines) and not lines[j].strip():
                        j += 1
                    if j < len(lines) and len(lines[j]) - len(lines[j].lstrip()) >= content_indent:
                        item.extend([""] * (j - i))
                        i = j
                        continue
                    if j < len(lines):
                        nxt = LIST_MARKER.match(lines[j])
                        if nxt and len(nxt.group(1)) == base_indent:
                            loose = True
                    i = j
                    break
                indent = len(line) - len(line.lstrip())
                if indent >= content_indent:
                    item.append(line[content_indent:])
                elif LIST_MARKER.match(line) or self.interrupts(lines, i):
                    break
                else:
                    item.append(line.strip())
                i += 1
            items.append(item)
            if i < len(lines) and not lines[i].strip():
                break
        tag = "ol" if ordered else "ul"
        attr = f' start="{start}"' if ordered and start != 1 else ""
        out = [f"<{tag}{attr}>"]
        for item in items:
            body = self.blocks(item)
            if not loose and "" not in item and body.startswith("<p>") and body.count("<p>") == 1:
                body = body[3:].replace("</p>", "", 1)
            out.append(f"<li>{body}</li>")
        out.append(f"</{tag}>")
        return "\n".join(out), i


# --------------------------------------------------------------------------
# Links


class LinkMap:
    def __init__(self, pages: list[Page]):
        self.by_source = {p.source: p.output for p in pages}
        self.by_source[API_SOURCE] = API_OUTPUT

    def rewrite(self, page: Page, target: str) -> str:
        parts = urlsplit(target)
        if parts.scheme or target.startswith("//") or target.startswith("mailto:"):
            return target
        if not parts.path:
            return target
        base_dir = posixpath.dirname(page.link_base or page.source)
        resolved = posixpath.normpath(posixpath.join(base_dir, parts.path))
        fragment = f"#{parts.fragment}" if parts.fragment else ""
        output = self.by_source.get(resolved)
        if output:
            return output + fragment
        # Anything not rendered on the site is linked to the repository.
        repo_path = ROOT / resolved
        kind = "tree" if target.endswith("/") or repo_path.is_dir() else "blob"
        return f"{REPOSITORY_URL}/{kind}/main/{resolved.rstrip('/')}{'/' if kind == 'tree' else ''}{fragment}"


# --------------------------------------------------------------------------
# OpenAPI


def ref_name(ref: str) -> str:
    return ref.rsplit("/", 1)[-1]


class OpenApiPage:
    def __init__(self, spec: dict):
        self.spec = spec
        self.components = spec.get("components", {})
        self.headings: list[tuple[int, str, str]] = []

    def resolve(self, obj: dict, kind: str) -> dict:
        if "$ref" in obj:
            return self.components[kind][ref_name(obj["$ref"])]
        return obj

    def type_label(self, schema: dict) -> str:
        if "$ref" in schema:
            # A ref may point inside another schema
            # (#/components/schemas/IssuedQuote/properties/asset); it links to
            # the schema that owns it and names the path.
            path = schema["$ref"].split("/")[3:]
            label = ".".join(p for p in path if p != "properties")
            return f'<a href="#schema-{path[0]}"><code>{html.escape(label)}</code></a>'
        for key in ("oneOf", "anyOf", "allOf"):
            if key in schema:
                joiner = " + " if key == "allOf" else " | "
                return joiner.join(self.type_label(s) for s in schema[key])
        kind = schema.get("type", "any")
        if isinstance(kind, list):
            kind = " | ".join(kind)
        if kind == "array":
            return f"array of {self.type_label(schema.get('items', {}))}"
        label = f"<code>{html.escape(str(kind))}</code>"
        if "format" in schema:
            label += f" ({html.escape(schema['format'])})"
        if kind == "object" and "additionalProperties" in schema and isinstance(schema["additionalProperties"], dict):
            label += f" of {self.type_label(schema['additionalProperties'])}"
        return label

    def constraints(self, schema: dict) -> str:
        notes = []
        if "enum" in schema:
            notes.append("one of " + ", ".join(f"<code>{html.escape(json.dumps(v))}</code>" for v in schema["enum"]))
        if "const" in schema:
            notes.append(f"always <code>{html.escape(json.dumps(schema['const']))}</code>")
        if "pattern" in schema:
            notes.append(f"pattern <code>{html.escape(schema['pattern'])}</code>")
        for key, word in (("minLength", "min length"), ("maxLength", "max length"), ("minimum", "min"), ("maximum", "max")):
            if key in schema:
                notes.append(f"{word} {schema[key]}")
        return "; ".join(notes)

    def text(self, value: str | None) -> str:
        return html.escape(value or "", quote=False)

    def properties_rows(self, schema: dict, prefix: str = "", depth: int = 0) -> list[str]:
        rows = []
        required = set(schema.get("required", []))
        for name, prop in schema.get("properties", {}).items():
            full = f"{prefix}{name}"
            desc = self.text(prop.get("description"))
            extra = self.constraints(prop)
            if extra:
                desc = f"{desc} <span class='muted'>{extra}</span>" if desc else f"<span class='muted'>{extra}</span>"
            req = '<span class="req">required</span>' if name in required else ""
            rows.append(f"<tr><td><code>{html.escape(full)}</code> {req}</td><td>{self.type_label(prop)}</td><td>{desc}</td></tr>")
            if prop.get("type") == "object" and "properties" in prop and depth < 3:
                rows += self.properties_rows(prop, f"{full}.", depth + 1)
        return rows

    def schema_block(self, name: str, schema: dict) -> str:
        out = [f'<section class="schema"><h3 id="schema-{name}"><code>{name}</code></h3>']
        if schema.get("description"):
            out.append(f"<p>{self.text(schema['description'])}</p>")
        rows = self.properties_rows(schema)
        if rows:
            out.append('<div class="table"><table><thead><tr><th>Field</th><th>Type</th><th>Description</th></tr></thead><tbody>')
            out += rows
            out.append("</tbody></table></div>")
        else:
            label = self.type_label({k: v for k, v in schema.items() if k != "description"})
            extra = self.constraints(schema)
            out.append(f"<p>Type: {label}{'; ' + extra if extra else ''}</p>")
        out.append("</section>")
        return "".join(out)

    def operation(self, method: str, path: str, op: dict, path_params: list) -> str:
        anchor = slugify(f"{method} {path}".replace("/", " ").replace("{", "").replace("}", ""))
        anchor = re.sub(r"-+", "-", anchor).strip("-")
        self.headings.append((3, anchor, f"{method.upper()} {path}"))
        out = [
            f'<section class="op"><h3 id="{anchor}"><span class="method {method}">{method.upper()}</span> '
            f"<code>{html.escape(path)}</code></h3>"
        ]
        if op.get("summary"):
            out.append(f"<p><strong>{self.text(op['summary'])}</strong></p>")
        if op.get("description"):
            out.append(f"<p>{self.text(op['description'])}</p>")
        security = op.get("security", self.spec.get("security"))
        if security:
            names = sorted({name for req in security for name in req}) or ["none"]
            out.append("<p class='muted'>Auth: " + ", ".join(f"<code>{n}</code>" for n in names) + "</p>")
        elif security == []:
            out.append("<p class='muted'>Auth: none</p>")
        params = [self.resolve(p, "parameters") for p in path_params + op.get("parameters", [])]
        if params:
            out.append('<div class="table"><table><thead><tr><th>Parameter</th><th>In</th><th>Type</th><th>Description</th></tr></thead><tbody>')
            for p in params:
                req = ' <span class="req">required</span>' if p.get("required") else ""
                schema = p.get("schema", {})
                extra = self.constraints(schema)
                desc = self.text(p.get("description")) + (f" <span class='muted'>{extra}</span>" if extra else "")
                out.append(
                    f"<tr><td><code>{html.escape(p['name'])}</code>{req}</td><td>{p.get('in', '')}</td>"
                    f"<td>{self.type_label(schema)}</td><td>{desc}</td></tr>"
                )
            out.append("</tbody></table></div>")
        body = op.get("requestBody")
        if body:
            body = self.resolve(body, "requestBodies")
            for media, content in body.get("content", {}).items():
                req = " (required)" if body.get("required") else ""
                out.append(f"<p>Request body{req}: <code>{html.escape(media)}</code> {self.type_label(content.get('schema', {}))}</p>")
        out.append('<div class="table"><table><thead><tr><th>Status</th><th>Description</th><th>Body</th></tr></thead><tbody>')
        for status, response in sorted(op.get("responses", {}).items()):
            response = self.resolve(response, "responses")
            bodies = ", ".join(self.type_label(c.get("schema", {})) for c in response.get("content", {}).values())
            out.append(f"<tr><td><code>{status}</code></td><td>{self.text(response.get('description'))}</td><td>{bodies}</td></tr>")
        out.append("</tbody></table></div></section>")
        return "".join(out)

    def render(self) -> str:
        info = self.spec.get("info", {})
        out = [
            f"<h1 id=\"api-reference\">API reference</h1>",
            f"<p>{self.text(info.get('title'))} {self.text(info.get('version'))}, OpenAPI {self.text(self.spec.get('openapi'))}. "
            f"{self.text(info.get('summary'))}</p>",
            f"<p>{self.text(info.get('description'))}</p>",
            '<p>Generated from <a href="openapi.json"><code>openapi.json</code></a>, the document a unit test keeps '
            "in step with the router. Import it into any OpenAPI tool for request examples.</p>",
        ]
        schemes = self.components.get("securitySchemes", {})
        if schemes:
            self.headings.append((2, "authentication", "Authentication"))
            out.append('<h2 id="authentication">Authentication</h2><ul>')
            for name, scheme in schemes.items():
                out.append(f"<li><code>{name}</code>: {self.text(scheme.get('scheme', scheme.get('type')))}. {self.text(scheme.get('description'))}</li>")
            out.append("</ul>")
        tags = [t["name"] for t in self.spec.get("tags", [])]
        tag_desc = {t["name"]: t.get("description", "") for t in self.spec.get("tags", [])}
        grouped: dict[str, list] = {t: [] for t in tags}
        for path, item in self.spec.get("paths", {}).items():
            for method in ("get", "post", "put", "patch", "delete"):
                if method in item:
                    tag = (item[method].get("tags") or ["other"])[0]
                    grouped.setdefault(tag, []).append((method, path, item[method], item.get("parameters", [])))
        for tag, ops in grouped.items():
            if not ops:
                continue
            anchor = f"tag-{slugify(tag)}"
            self.headings.append((2, anchor, tag.capitalize()))
            out.append(f'<h2 id="{anchor}">{html.escape(tag.capitalize())}</h2>')
            if tag_desc.get(tag):
                out.append(f"<p>{self.text(tag_desc[tag])}</p>")
            for method, path, op, path_params in ops:
                out.append(self.operation(method, path, op, path_params))
        self.headings.append((2, "schemas", "Schemas"))
        out.append('<h2 id="schemas">Schemas</h2>')
        for name, schema in self.components.get("schemas", {}).items():
            out.append(self.schema_block(name, schema))
        return "\n".join(out)


# --------------------------------------------------------------------------
# Layout


CSS = """
:root{--bg:#fbfbfa;--fg:#1c1d1f;--muted:#5d6269;--line:#e3e4e6;--code-bg:#f1f2f4;--accent:#0b6e4f;--link:#0a5dc2;--warn-bg:#fff6e0;--warn-line:#e7c56b}
@media (prefers-color-scheme:dark){:root{--bg:#131416;--fg:#e6e7e9;--muted:#9aa0a8;--line:#2b2e33;--code-bg:#1d1f23;--accent:#4cc79a;--link:#7ab4ff;--warn-bg:#2a2412;--warn-line:#7a6320}}
*{box-sizing:border-box}
html{-webkit-text-size-adjust:100%}
body{margin:0;background:var(--bg);color:var(--fg);font:16px/1.6 system-ui,-apple-system,"Segoe UI",Roboto,"Helvetica Neue",Arial,sans-serif}
a{color:var(--link)}
header.site{border-bottom:1px solid var(--line);position:sticky;top:0;background:var(--bg);z-index:2}
header.site .inner{max-width:1080px;margin:0 auto;padding:10px 16px;display:flex;flex-wrap:wrap;gap:6px 18px;align-items:center}
header.site .brand{font-weight:700;color:var(--fg);text-decoration:none;margin-right:auto}
header.site nav{display:flex;flex-wrap:wrap;gap:4px 14px;font-size:15px}
header.site nav a{color:var(--muted);text-decoration:none}
header.site nav a[aria-current]{color:var(--fg);font-weight:600}
.layout{max-width:1080px;margin:0 auto;padding:0 16px;display:grid;grid-template-columns:minmax(0,1fr);gap:32px}
@media (min-width:960px){.layout{grid-template-columns:220px minmax(0,1fr)}}
aside.toc{display:none;font-size:14px}
@media (min-width:960px){aside.toc{display:block;position:sticky;top:64px;align-self:start;max-height:calc(100vh - 80px);overflow:auto;padding-top:28px}}
aside.toc ul{list-style:none;margin:0;padding:0}
aside.toc li{margin:4px 0}
aside.toc li.l3{padding-left:12px}
aside.toc a{color:var(--muted);text-decoration:none}
main{min-width:0;padding:8px 0 48px}
h1,h2,h3,h4{line-height:1.25;margin:1.6em 0 .6em}
h1{font-size:2rem;margin-top:1em}
h2{font-size:1.45rem;border-bottom:1px solid var(--line);padding-bottom:.3em}
h3{font-size:1.15rem}
.anchor{margin-left:.4em;color:var(--muted);text-decoration:none;opacity:0;font-weight:400}
h1:hover .anchor,h2:hover .anchor,h3:hover .anchor,h4:hover .anchor{opacity:1}
code{font:.9em/1.4 ui-monospace,SFMono-Regular,Menlo,Consolas,monospace;background:var(--code-bg);padding:.1em .3em;border-radius:4px;overflow-wrap:anywhere}
pre{background:var(--code-bg);padding:12px 14px;border-radius:6px;overflow:auto;line-height:1.45}
pre code{background:none;padding:0;overflow-wrap:normal}
.table{overflow-x:auto;margin:1em 0}
table{border-collapse:collapse;width:100%;font-size:15px}
th,td{border:1px solid var(--line);padding:6px 10px;text-align:left;vertical-align:top}
th{background:var(--code-bg)}
blockquote{margin:1em 0;padding:.2em 1em;border-left:4px solid var(--warn-line);background:var(--warn-bg)}
hr{border:0;border-top:1px solid var(--line);margin:2em 0}
.muted{color:var(--muted)}
.req{font-size:12px;color:var(--accent);font-weight:600}
.method{display:inline-block;min-width:3.6em;text-align:center;font:600 13px/1.8 ui-monospace,monospace;border-radius:4px;color:#fff;background:#57606a;padding:0 6px}
.method.get{background:#1f6feb}.method.post{background:#1a7f37}.method.delete{background:#cf222e}.method.put,.method.patch{background:#9a6700}
section.op,section.schema{border-top:1px solid var(--line);padding-top:4px}
footer.site{border-top:1px solid var(--line);color:var(--muted);font-size:14px}
footer.site .inner{max-width:1080px;margin:0 auto;padding:16px}
"""


def page_html(*, title: str, description: str, canonical: str, body: str, nav_current: str,
              headings: list[tuple[int, str, str]], og_type: str, extra_head: str = "") -> str:
    nav_links = [(p.nav, p.output) for p in PAGES if p.nav] + [("API", API_OUTPUT)]
    current = ' aria-current="page"'
    nav = "".join(
        f'<a href="{href}"{current if href == nav_current else ""}>{html.escape(label)}</a>'
        for label, href in nav_links
    )
    toc_items = "".join(
        f'<li class="l{level}"><a href="#{anchor}">{html.escape(text)}</a></li>'
        for level, anchor, text in headings
        if level in (2, 3)
    )
    toc = f'<aside class="toc" aria-label="On this page"><ul>{toc_items}</ul></aside>' if toc_items else "<aside class=\"toc\"></aside>"
    full_title = title if title.startswith(SITE_NAME) else f"{title} — {SITE_NAME}"
    e = html.escape
    return f"""<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{e(full_title)}</title>
<meta name="description" content="{e(description)}">
<link rel="canonical" href="{e(canonical)}">
<meta name="color-scheme" content="light dark">
<meta property="og:site_name" content="{SITE_NAME}">
<meta property="og:type" content="{og_type}">
<meta property="og:title" content="{e(full_title)}">
<meta property="og:description" content="{e(description)}">
<meta property="og:url" content="{e(canonical)}">
<meta name="twitter:card" content="summary">
<link rel="stylesheet" href="style.css">
{extra_head}</head>
<body>
<header class="site"><div class="inner"><a class="brand" href="index.html">{SITE_NAME}</a><nav aria-label="Documentation">{nav}<a href="{REPOSITORY_URL}" rel="noopener">GitHub</a></nav></div></header>
<div class="layout">
{toc}
<main>
{body}
</main>
</div>
<footer class="site"><div class="inner">Apache-2.0 · <a href="{REPOSITORY_URL}" rel="noopener">Source on GitHub</a> · Generated from the repository's <code>docs/</code></div></footer>
</body>
</html>
"""


# --------------------------------------------------------------------------
# Link check


class _Collector(HTMLParser):
    def __init__(self):
        super().__init__()
        self.ids: set[str] = set()
        self.links: list[str] = []

    def handle_starttag(self, tag, attrs):
        a = dict(attrs)
        if "id" in a:
            self.ids.add(a["id"])
        for key in ("href", "src"):
            if key in a and a[key] is not None:
                self.links.append(a[key])


def check_links(out_dir: Path) -> list[str]:
    parsed: dict[Path, _Collector] = {}
    for path in out_dir.rglob("*.html"):
        c = _Collector()
        c.feed(path.read_text(encoding="utf-8"))
        parsed[path.resolve()] = c
    errors = []
    for path, c in sorted(parsed.items()):
        for link in c.links:
            parts = urlsplit(link)
            if parts.scheme or link.startswith("//"):
                continue
            target = (path.parent / parts.path).resolve() if parts.path else path
            rel = path.relative_to(out_dir.resolve())
            if not target.exists():
                errors.append(f"{rel}: {link} -> missing file")
                continue
            if target.suffix == ".md":
                errors.append(f"{rel}: {link} -> links a Markdown source")
            if parts.fragment and target.suffix == ".html" and parts.fragment not in parsed[target].ids:
                errors.append(f"{rel}: {link} -> missing anchor #{parts.fragment}")
    return errors


# --------------------------------------------------------------------------


def build(base_url: str, out_dir: Path) -> list[str]:
    if not base_url.endswith("/"):
        base_url += "/"
    if out_dir.exists():
        shutil.rmtree(out_dir)
    out_dir.mkdir(parents=True)
    links = LinkMap(PAGES)
    urls = []
    today = date.today().isoformat()

    for page in PAGES:
        source = (ROOT / page.source).read_text(encoding="utf-8")
        md = Markdown(lambda target, page=page: links.rewrite(page, target))
        body = md.convert(source)
        canonical = base_url if page.output == "index.html" else base_url + page.output
        extra = ""
        if page.output == "index.html":
            data = {
                "@context": "https://schema.org",
                "@type": "SoftwareSourceCode",
                "name": SITE_NAME,
                "description": page.description,
                "codeRepository": REPOSITORY_URL,
                "programmingLanguage": "Rust",
                "license": "https://www.apache.org/licenses/LICENSE-2.0",
                "url": base_url,
            }
            extra = f'<script type="application/ld+json">{json.dumps(data)}</script>\n'
        (out_dir / page.output).write_text(
            page_html(
                title=page.title,
                description=page.description,
                canonical=canonical,
                body=body,
                nav_current=page.output,
                headings=md.headings,
                og_type="website" if page.output == "index.html" else "article",
                extra_head=extra,
            ),
            encoding="utf-8",
            newline="\n",
        )
        urls.append(canonical)

    spec = json.loads((ROOT / API_SOURCE).read_text(encoding="utf-8"))
    api = OpenApiPage(spec)
    api_body = api.render()
    (out_dir / API_OUTPUT).write_text(
        page_html(
            title="API reference",
            description="Every route of the Crypto Gateway HTTP API: merchant payment intents, quotes "
            "and checkout, operator evidence and oversight, health, with parameters, responses and schemas.",
            canonical=base_url + API_OUTPUT,
            body=api_body,
            nav_current=API_OUTPUT,
            headings=api.headings,
            og_type="article",
        ),
        encoding="utf-8",
        newline="\n",
    )
    urls.append(base_url + API_OUTPUT)
    shutil.copyfile(ROOT / API_SOURCE, out_dir / "openapi.json")

    (out_dir / "style.css").write_text(CSS.lstrip(), encoding="utf-8", newline="\n")
    (out_dir / ".nojekyll").write_text("", encoding="utf-8")
    sitemap = ['<?xml version="1.0" encoding="UTF-8"?>', '<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">']
    sitemap += [f"  <url><loc>{html.escape(u)}</loc><lastmod>{today}</lastmod></url>" for u in urls]
    sitemap.append("</urlset>")
    (out_dir / "sitemap.xml").write_text("\n".join(sitemap) + "\n", encoding="utf-8", newline="\n")
    robots = (
        "User-agent: *\nAllow: /\n\n"
        "User-agent: Googlebot\nAllow: /\n\n"
        "User-agent: OAI-SearchBot\nAllow: /\n\n"
        f"Sitemap: {base_url}sitemap.xml\n"
    )
    (out_dir / "robots.txt").write_text(robots, encoding="utf-8", newline="\n")
    return urls


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--base-url", default=os.environ.get("SITE_BASE_URL") or DEFAULT_BASE_URL)
    parser.add_argument("--out", type=Path, default=SITE_DIR / "_build")
    parser.add_argument("--check-links", action="store_true")
    args = parser.parse_args()
    urls = build(args.base_url, args.out)
    print(f"built {len(urls)} pages into {args.out}")
    if args.check_links:
        errors = check_links(args.out)
        for error in errors:
            print(f"broken link: {error}", file=sys.stderr)
        if errors:
            print(f"{len(errors)} broken internal links", file=sys.stderr)
            return 1
        print("internal links: ok")
    return 0


if __name__ == "__main__":
    sys.exit(main())
