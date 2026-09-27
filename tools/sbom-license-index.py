#!/usr/bin/env python3
"""Make a watch-readable package/license index from an image SPDX and Cargo reports."""
import argparse
import json
from pathlib import Path


def index(spdx, manifest, cargo_dir=None):
    graph = json.loads(Path(spdx).read_text())["@graph"]
    elements = {item["spdxId"]: item for item in graph if "spdxId" in item}
    declared = {}
    for item in graph:
        if item.get("relationshipType") == "hasDeclaredLicense":
            declared.setdefault(item["from"], set()).update(item["to"])
    installed = {line.split()[0] for line in Path(manifest).read_text().splitlines() if line.strip()}
    package_ids = {item["spdxId"] for item in graph
                   if item.get("type") == "software_Package"
                   and item.get("software_primaryPurpose") == "install"
                   and item.get("name") in installed}
    installed_file_ids = {identifier for item in graph
                          if item.get("relationshipType") == "contains"
                          and item.get("from") in package_ids
                          for identifier in item.get("to", [])}
    files = {Path(item.get("name", "")).name for item in graph
             if item.get("type") == "software_File" and item.get("spdxId") in installed_file_ids}
    rows = set()
    for item in graph:
        if item.get("type") != "software_Package" or item.get("software_primaryPurpose") != "install":
            continue
        name = item["name"]
        if item["spdxId"] not in package_ids:
            continue
        licenses = []
        for identifier in declared.get(item["spdxId"], ()):
            license_item = elements.get(identifier, {})
            licenses.append(license_item.get("simplelicensing_licenseExpression")
                            or license_item.get("name") or "NOASSERTION")
        rows.add(("package", name, item.get("software_packageVersion", ""),
                  " AND ".join(sorted(set(licenses))) if licenses else "NOASSERTION"))
    if cargo_dir and Path(cargo_dir).is_dir():
        for report in Path(cargo_dir).glob("*/*.licenses.json"):
            data = json.loads(report.read_text())
            if data["artifact"] not in files:
                continue
            for crate in data["crates"]:
                if not crate.get("name"):
                    continue
                rows.add(("crate", crate["name"], crate.get("version") or "",
                          crate.get("licenseDeclared") or "NOASSERTION"))
    return sorted(rows, key=lambda row: (row[1].casefold(), row[2], row[0], row[3]))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("spdx", type=Path)
    parser.add_argument("manifest", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("--cargo-dir", type=Path)
    args = parser.parse_args()
    rows = index(args.spdx, args.manifest, args.cargo_dir)
    args.output.write_text("".join("\t".join(row) + "\n" for row in rows))
    print(f"{len(rows)} distinct package/crate license rows in {args.output}")


if __name__ == "__main__":
    main()
