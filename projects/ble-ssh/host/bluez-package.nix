{ bluez, lib }:
# Keep this local API extension tied to its reviewed source version. Rebase and
# revalidate explicitly before adopting a different BlueZ release.
assert lib.assertMsg (bluez.version == "5.86")
  "The Hoki bearer readiness patch currently requires BlueZ 5.86";
bluez.overrideAttrs (old: {
  patches = (old.patches or [ ]) ++ [
    ./patches/0001-per-bearer-service-freshness.patch
    ./patches/0002-service-changed-readiness.patch
    ./patches/0003-bound-discovery-cleanup-range.patch
  ];
})
