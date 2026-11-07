"""Merges the cargo-about and go-licenses output of scripts/update-licenses.sh into assets/licenses.json: one entry per license, its packages grouped by the wording of their license file, most common first. ScanCode finds the copyright lines, which stay with each package as its notices; a license text without any sends it on to the package's other files."""
import collections
import csv
import glob
import hashlib
import json
import os
import re
import shlex
import subprocess
import sys
import tempfile

tmp = sys.argv[1]
# The command that runs ScanCode.
SCANCODE = shlex.split(sys.argv[2])
# Syncative itself is GPL-3.0-or-later and shown in About as such.
OWN_CRATES = {"syncative", "syncative-go"}
OWN_GO = "syncative/go"
# The Go code ported from Proton-API-Bridge, which keeps its MIT notice.
PORTED = {"syncative/go/proton/drive": "Proton-API-Bridge"}
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
# Licenses cargo-about finds in a crate for something not in it: webpki names BetterTLS, whose test suite it doesn't ship.
SKIP = {("webpki", "Pixar")}
# Crates for which cargo-about took a source file for the license: the file that really holds it.
LICENSE_TEXT = {("webpki", "ISC"): "LICENSE"}
# Packages for which ScanCode's finds are wrong, as they name other projects or miss a line without a year; their copyright as the package states it. None: go on to the authors.
NOTICES = {
    "cesu8": ["Copyright 2012-2014 The Rust Project Developers and Eric Kidd"],
    "self_cell": None,
    "go.opentelemetry.io/auto/sdk": ["Copyright The OpenTelemetry Authors"],
    "go.opentelemetry.io/contrib/instrumentation/net/http/otelhttp": ["Copyright The OpenTelemetry Authors"],
}
# ScanCode's finds that aren't a copyright statement, such as "holder is Tim Hudson" or "copyrighted by …".
STATEMENT = re.compile(r"(copyright(?!ed)\b|\(c\)|©)", re.I)
# An unfilled copyright template.
TEMPLATE = re.compile(r"\[yyyy\]|<year>|\{yyyy\}|\[year\]", re.I)
TEMPLATE_LINE = re.compile(r"^\s*(copyright\b|\(c\)|©).*(\[yyyy\]|<year>|\{yyyy\}|\[year\])", re.I)
# A title line such as "MIT License" or "The MIT License (MIT)".
TITLE = re.compile(r"^\s*(the\s+)?[\w.\- ]{0,30}\blicen[cs]e\b[\w.\-() ]{0,20}$", re.I)
# The marks of a source comment around a line.
COMMENT_START = re.compile(r"^\s*(//[/!]?|/\*+|\*+/?|#+)[ \t]?")
COMMENT_END = re.compile(r"\s*(\*+/?|//)\s*$")
NOTICE_START = re.compile(r"^(copyright\b|\(c\)|©)", re.I)
LICENSE_FILE = re.compile(r"(LICEN[CS]E|COPYING|COPYRIGHT|NOTICE|MIT|APACHE)", re.I)


def normalized(text):
    return text.replace("\r\n", "\n").replace("\r", "\n")


def uncommented(line):
    return COMMENT_END.sub("", COMMENT_START.sub("", line)).strip()


def scan(texts):
    """ScanCode's copyright finds for each of `texts` (key → text): key → the finds with their lines."""
    # In the project's temp/: steam-run doesn't show ScanCode this /tmp.
    with tempfile.TemporaryDirectory(dir=os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "temp")) as folder:
        keys = {}
        for number, (key, text) in enumerate(texts.items()):
            name = f"{number}.txt"
            keys[name] = key
            with open(os.path.join(folder, name), "w", encoding="utf-8", newline="\n") as file:
                file.write(text)
        out = folder + "-scan.json"
        try:
            # Exits with an error as well when a single file failed; those simply have no finds.
            run = subprocess.run([*SCANCODE, "--copyright", "--json", out, "-n", "4", "--quiet", folder], stderr=subprocess.PIPE, text=True)
            if not os.path.exists(out):
                sys.exit(f"ScanCode failed ({run.returncode}): {run.stderr[-2000:]}")
            result = json.load(open(out))
        finally:
            if os.path.exists(out):
                os.remove(out)
    finds = {key: [] for key in texts}
    for file in result["files"]:
        key = keys.get(os.path.basename(file["path"]))
        if key is not None:
            finds[key] = file.get("copyrights", [])
    return finds


def notice_lines(text, finds):
    """The copyright statements found in `text`, and the line numbers they fill. Their own wording where a line holds nothing else, because ScanCode's version loses accents and "All rights reserved"; ScanCode's version for a statement inside a sentence, whose lines stay."""
    lines = text.split("\n")
    notices, used = [], set()
    for find in finds:
        if not STATEMENT.search(find["copyright"]) or TEMPLATE.search(find["copyright"]):
            continue
        span = range(find["start_line"], find["end_line"] + 1)
        original = " ".join(" ".join(uncommented(lines[number - 1]) for number in span if number <= len(lines)).split())
        if NOTICE_START.match(original):
            notice = re.sub(r"^copyright:\s*(?=copyright)", "", original, flags=re.I)
            used.update(span)
        else:
            notice = " ".join(find["copyright"].split())
        if notice not in notices:
            notices.append(notice)
    return notices, used


def uncomment(lines):
    """A license written as a source comment, without the comment marks."""
    filled = [line for line in lines if line.strip()]
    if filled and all(COMMENT_START.match(line) and not line.lstrip().startswith("#") for line in filled):
        return [uncommented(line) for line in lines]
    return lines


def split(text, finds):
    """The license text without its copyright lines, and those lines. Short licenses also lose a title line and an unfilled copyright template, which only differ between copies; Apache's appendix keeps its template."""
    notices, used = notice_lines(text, finds)
    lines = [line for number, line in enumerate(text.split("\n"), 1) if number not in used]
    lines = uncomment(lines)
    if len(text) < 3000:
        lines = [line for line in lines if not TEMPLATE_LINE.match(line)]
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


def read(path):
    try:
        return normalized(open(path, encoding="utf-8", errors="replace").read())
    except OSError:
        return ""


def license_files(folder):
    try:
        return sorted(os.path.join(folder, f) for f in os.listdir(folder) if LICENSE_FILE.match(f) and os.path.isfile(os.path.join(folder, f)))
    except OSError:
        return []


def repository_root(folder):
    """The checkout a git dependency's crate lies in; a crate from crates.io stands alone."""
    if "/.cargo/git/checkouts/" not in folder:
        return None
    while folder != "/" and not os.path.exists(os.path.join(folder, ".git")):
        folder = os.path.dirname(folder)
    return folder if folder != "/" else None


def crate_files(crate):
    """Where a crate may state its copyright, in order: its license files, the head of its sources, the license files of the repository it is part of, its README."""
    folder = os.path.dirname(crate["manifest_path"])
    sources = [os.path.join(folder, "src", f) for f in ("lib.rs", "main.rs")] + sorted(glob.glob(os.path.join(folder, "src", "*.rs")))[:10]
    root = repository_root(folder)
    return license_files(folder) + sources + (license_files(root) if root else []) + [os.path.join(folder, "README.md")]


def go_files(folder):
    return license_files(folder) + sorted(glob.glob(os.path.join(folder, "*.go")))[:10] + [os.path.join(folder, "README.md")]


def head(path):
    """A license file in full, of other files the head, where a copyright statement belongs."""
    text = read(path)
    return text if LICENSE_FILE.match(os.path.basename(path)) else "\n".join(text.split("\n")[:40])


# What each package's license says, before ScanCode looked at it.
entries = []


def add(kind, text, name, version, files=(), authors=(), repository=None, go=False):
    entries.append({"kind": kind, "text": normalized(text), "name": name, "version": version, "files": list(files), "authors": list(authors), "repository": repository, "go": go})


rust = json.load(open(os.path.join(tmp, "rust.json")))
crate_packages = {(c["package"]["name"], c["package"]["version"]): c["package"] for c in rust["crates"]}
for license in rust["licenses"]:
    for user in license["used_by"]:
        crate = user["crate"]
        if crate["name"] not in OWN_CRATES and (crate["name"], license["id"]) not in SKIP:
            package = crate_packages.get((crate["name"], crate["version"]), crate)
            text = license["text"]
            if (crate["name"], license["id"]) in LICENSE_TEXT:
                text = read(os.path.join(os.path.dirname(package["manifest_path"]), LICENSE_TEXT[(crate["name"], license["id"])]))
            add(license["id"], text, crate["name"], crate["version"], crate_files(package), package.get("authors") or (), package.get("repository"))

# Bundled icons with their own license file; those in the common wording of their license follow once the groups exist.
crates = {c["package"]["name"]: c["package"] for c in rust["crates"]}
common = []
for crate, kind, name, path, notices in BUNDLED:
    package = crates.get(crate)
    if package is None:
        continue
    # The crate's version isn't the icons' version.
    if path:
        add(kind, read(os.path.join(os.path.dirname(package["manifest_path"]), path)), name, "")
    else:
        common.append((kind, name, notices))

versions = {}
for line in open(os.path.join(tmp, "go-modules.txt")):
    path, _, version = line.strip().partition(" ")
    versions[path] = version
go_dirs = dict(line.split(" ", 1) for line in open(os.path.join(tmp, "go-dirs.txt")).read().splitlines() if " " in line)
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
    source = go_dirs.get(library) or next((d for path, d in sorted(go_dirs.items()) if path.startswith(library + "/")), "")
    add(kind, text, PORTED.get(library, library), versions.get(module, ""), go_files(source) if source else [], repository="https://" + "/".join(library.split("/")[:3]) if library.startswith("github.com/") else None, go=True)

# The license texts first; the other files only of the packages whose text names nobody.
key_of = lambda text: hashlib.sha256(text.encode()).hexdigest()
text_finds = scan({key_of(e["text"]): e["text"] for e in entries})
pieces = {}
for entry in entries:
    entry["body"], entry["notices"] = split(entry["text"], text_finds[key_of(entry["text"])])
    if not entry["notices"] and (entry["name"], entry["kind"]) not in PARTS and entry["name"] not in NOTICES:
        for path in entry["files"]:
            pieces.setdefault(path, head(path))
file_finds = scan({path: text for path, text in pieces.items() if text.strip()})

# Go paths whose start says little: the short form that replaces it.
GO_PREFIXES = {
    "golang.org/x/": "x/",
    "cloud.google.com/go/": "cloud/",
    "google.golang.org/": "google/",
    "go.opentelemetry.io/otel": "otel",
    "go.opentelemetry.io/": "otel/",
}
# Repository names that need their owner to mean anything.
GO_GENERIC = {"common", "errors"}


def go_name(library):
    """A Go package's name without its host, owner and major version: github.com/spf13/pflag → pflag, golang.org/x/crypto → x/crypto."""
    parts = library.split("/")
    for start, short in GO_PREFIXES.items():
        if library.startswith(start):
            parts = (short + library[len(start):]).split("/")
            break
    else:
        keep_owner = len(parts) > 2 and parts[2] in GO_GENERIC
        parts = parts[1:] if keep_owner else parts[2:] if parts[0] in ("github.com", "gitlab.com", "bitbucket.org", "codeberg.org") else parts[1:]
    parts = [re.sub(r"\.v\d+$", "", part) for part in parts if not re.fullmatch(r"v\d+", part)]
    return "/".join(parts) or library


# A short name that several packages would share keeps the full path.
go_libraries = {e["name"] for e in entries if e["go"] and e["name"] not in PORTED.values()}
crate_names = {e["name"] for e in entries if not e["go"]}
short_counts = collections.Counter(go_name(library) for library in go_libraries)
for entry in entries:
    if entry["name"] in go_libraries:
        short = go_name(entry["name"])
        if short_counts[short] == 1 and short not in crate_names:
            entry["display"] = short

groups = {}


def notices_of(entry):
    if (entry["name"], entry["kind"]) in PARTS:
        return PARTS[(entry["name"], entry["kind"])][1]
    if entry["notices"]:
        return entry["notices"]
    if entry["name"] in NOTICES and NOTICES[entry["name"]] is not None:
        return NOTICES[entry["name"]]
    if entry["name"] not in NOTICES:
        for path in entry["files"]:
            found, _ = notice_lines(pieces.get(path, ""), file_finds.get(path, []))
            if found:
                return found
    names = [re.sub(r"\s*<.*?>", "", author).strip() for author in entry["authors"]]
    if names:
        return ["Authors: " + ", ".join(names)]
    return ["Source: " + entry["repository"]] if entry["repository"] else []


for entry in entries:
    notices = notices_of(entry)
    name = PARTS[(entry["name"], entry["kind"])][0] if (entry["name"], entry["kind"]) in PARTS else entry.get("display", entry["name"])
    kind = entry["kind"]
    group = groups.setdefault((kind, wording(entry["body"])), {"license": kind, "text": reflow(entry["body"]), "packages": {}})
    package = group["packages"].setdefault((name, entry["version"]), {"name": name, "version": entry["version"], "notices": []})
    for notice in notices:
        if notice not in package["notices"]:
            package["notices"].append(notice)

for kind, name, notices in common:
    group = max((g for g in groups.values() if g["license"] == kind), key=lambda g: len(g["packages"]))
    group["packages"][(name, "")] = {"name": name, "version": "", "notices": notices}

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
