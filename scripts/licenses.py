"""Merges the cargo-about and go-licenses output of scripts/update-licenses.sh into assets/licenses.json: one entry per license, its packages grouped by the wording of their license file, most common first; the copyright lines stay with each package as its notices."""
import csv
import json
import os
import re
import sys

tmp = sys.argv[1]
# Syncative itself is GPL-3.0-or-later and shown in About as such.
OWN_CRATES = {"syncative", "syncative-go"}
OWN_GO = "syncative/go"
# The Go code ported from Proton-API-Bridge, which keeps its MIT notice.
PORTED = {"syncative/go/drive": "Proton-API-Bridge"}
# Crates whose other licenses cover only a part they bundle: the name to show for it and its notices, as its license file gives them.
PARTS = {
    ("iced_android", "Apache-2.0"): ("Twemoji font, bundled with iced_android", ["Copyright 2016-2018, Mozilla Foundation"]),
    ("iced_android", "CC-BY-4.0"): ("Twemoji emoji art, bundled with iced_android", ["Twemoji by Twitter (https://twitter.github.io/twemoji)"]),
}
# Icons that crates (or Syncative itself) bundle without them being in the crate's license file: the crate, the license, the name to show, the file with their license text inside the crate (None: the common wording of that license) and their copyright lines as upstream gives them (None: from that file).
BUNDLED = [
    ("iced_fabi", "MIT", "Material Icon Theme icons, bundled with iced_fabi", "assets/material-icon-theme/LICENSE", None),
    ("icondata_tb", "MIT", "Tabler Icons, bundled with icondata_tb", None, ["Copyright (c) 2020-2026 Paweł Kuna"]),
    ("icondata_mdi", "Apache-2.0", "Material Design Icons, bundled with icondata_mdi", None, ["Material Design Icons by Pictogrammers (https://pictogrammers.com), icons under Apache 2.0 by the Pictogrammers Free License"]),
    ("syncative", "MIT", "Ionicons (one icon in Syncative)", None, ["Copyright (c) 2015-present Ionic (http://ionic.io/)"]),
]
# A notice such as "Copyright (c) 2019 Someone", not license wording such as "copyright notice, this list of conditions" or the template line in Apache's appendix.
COPYRIGHT = re.compile(r"^\s*(copyright\b(?!\s+(notice|holders?|owners?|and|law|license|statement)\b)|\(c\)\s*\d|©)", re.I)
TEMPLATE = re.compile(r"\[yyyy\]|<year>|\{yyyy\}|\[year\]", re.I)
# A title line such as "MIT License" or "The MIT License (MIT)".
TITLE = re.compile(r"^\s*(the\s+)?[\w.\- ]{0,30}\blicen[cs]e\b[\w.\-() ]{0,20}$", re.I)


def split(text):
    """The license text without its copyright lines, and those lines. Short licenses also lose a title line and an unfilled copyright template, which only differ between copies; Apache's appendix keeps its template."""
    lines = text.replace("\r\n", "\n").splitlines()
    is_notice = lambda line: COPYRIGHT.match(line) and not TEMPLATE.search(line)
    notices = [line.strip() for line in lines if is_notice(line)]
    lines = [line for line in lines if not is_notice(line)]
    if len(text) < 3000:
        lines = [line for line in lines if not (COPYRIGHT.match(line) and TEMPLATE.search(line))]
        first = next((i for i, line in enumerate(lines) if line.strip()), None)
        if first is not None and TITLE.match(lines[first]):
            del lines[first]
    body = "\n".join(lines).strip("\n")
    return re.sub(r"\n{3,}", "\n\n", body), notices


# A line that starts a list item or a section, which keeps its own line.
ITEM = re.compile(r"^\s*(\d+(\.\d+)*\.?\s|\([a-z0-9]+\)\s|[-*•]\s|(Section|Article|Exhibit|APPENDIX)\b)")


def reflow(body):
    """One line per paragraph, so the text wraps to the dialog's width; list items and headings start lines of their own."""
    paragraphs = []
    for block in re.split(r"\n\s*\n", body):
        lines = []
        for line in block.splitlines():
            line = line.strip()
            if lines and not ITEM.match(line):
                lines[-1] += " " + line
            else:
                lines.append(line)
        paragraphs.append("\n".join(lines))
    return "\n\n".join(p for p in paragraphs if p)


def wording(body):
    """What tells two copies of a license apart: the words, without case, punctuation and line breaks."""
    return " ".join(re.sub(r"[^\w]+", " ", body.lower()).split())


groups = {}


def add(kind, text, name, version):
    body, notices = split(text)
    if (name, kind) in PARTS:
        name, notices = PARTS[(name, kind)]
    key = (kind, wording(body))
    group = groups.setdefault(key, {"license": kind, "text": reflow(body), "packages": {}})
    package = group["packages"].setdefault((name, version), {"name": name, "version": version, "notices": []})
    for notice in notices:
        if notice not in package["notices"]:
            package["notices"].append(notice)


rust = json.load(open(os.path.join(tmp, "rust.json")))
for license in rust["licenses"]:
    for user in license["used_by"]:
        crate = user["crate"]
        if crate["name"] not in OWN_CRATES:
            add(license["id"], license["text"], crate["name"], crate["version"])

# Bundled icons: with their own license file, or in the most common wording of their license.
crates = {c["package"]["name"]: c["package"] for c in rust["crates"]}
for crate, kind, name, path, notices in BUNDLED:
    package = crates.get(crate)
    if package is None:
        continue
    # The crate's version isn't the icons' version.
    version = ""
    if path:
        text = open(os.path.join(os.path.dirname(package["manifest_path"]), path), encoding="utf-8").read()
        add(kind, text, name, version)
    else:
        group = max((g for g in groups.values() if g["license"] == kind), key=lambda g: len(g["packages"]))
        group["packages"][(name, version)] = {"name": name, "version": version, "notices": notices}

versions = {}
for line in open(os.path.join(tmp, "go-modules.txt")):
    path, _, version = line.strip().partition(" ")
    versions[path] = version
for library, _url, kind in csv.reader(open(os.path.join(tmp, "go.csv"))):
    if library.startswith(OWN_GO) and library not in PORTED:
        continue
    folder = os.path.join(tmp, "go-save", library)
    files = sorted(f for f in os.listdir(folder) if re.match(r"(LICEN[CS]E|COPYING)", f, re.I)) if os.path.isdir(folder) else []
    if not files:
        print(f"no license text for {library}", file=sys.stderr)
        continue
    text = open(os.path.join(folder, files[0]), encoding="utf-8", errors="replace").read()
    module = max((path for path in versions if library == path or library.startswith(path + "/")), key=len, default="")
    add(kind, text, PORTED.get(library, library), versions.get(module, ""))

licenses = {}
for group in groups.values():
    packages = sorted(group["packages"].values(), key=lambda p: (p["name"].lower(), p["version"]))
    licenses.setdefault(group["license"], []).append({"text": group["text"], "packages": packages})
result = []
for kind, variants in licenses.items():
    variants.sort(key=lambda v: -len(v["packages"]))
    result.append({"license": kind, "variants": variants})
result.sort(key=lambda l: (-sum(len(v["packages"]) for v in l["variants"]), l["license"]))
json.dump(result, sys.stdout, ensure_ascii=False, separators=(",", ":"))
