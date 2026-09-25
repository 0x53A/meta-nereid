#!/usr/bin/env python3
"""Add local SSH/Wi-Fi access to an unmounted ext4 image, without sudo."""

import argparse
import configparser
import io
import json
import os
from pathlib import Path, PurePosixPath
import re
import shlex
import shutil
import stat
import subprocess
import tempfile



def run(argv):
    return subprocess.run(argv, capture_output=True, env={**os.environ, "LC_ALL": "C"})


def nm_value(uuid, field, secret=False):
    args = ["nmcli", "--escape", "no", "-g", field]
    if secret:
        args.append("--show-secrets")
    result = run(args + ["connection", "show", "uuid", uuid])
    if result.returncode:
        raise RuntimeError("NetworkManager could not read a selected profile (details suppressed).")
    return result.stdout.decode().removesuffix("\n")


def wifi_config(ssids):
    if not ssids:
        raise RuntimeError("Select at least one Wi-Fi SSID with --wifi-ssid.")
    result = run(["nmcli", "-t", "-f", "UUID,TYPE", "connection", "show"])
    if result.returncode:
        raise RuntimeError("Cannot list NetworkManager connections.")
    profiles = {ssid: [] for ssid in ssids}
    for line in result.stdout.decode().splitlines():
        uuid, kind = line.split(":", 1)
        if kind != "802-11-wireless":
            continue
        ssid = nm_value(uuid, "802-11-wireless.ssid")
        if ssid in profiles:
            profiles[ssid].append(uuid)
    sections = []
    for ssid, matches in profiles.items():
        if len(matches) != 1:
            raise RuntimeError(f"Expected one saved profile for SSID {ssid!r}; found {len(matches)}.")
        uuid = matches[0]
        if nm_value(uuid, "802-11-wireless.mode") != "infrastructure":
            raise RuntimeError(f"SSID {ssid!r} is not a client Wi-Fi profile.")
        security = nm_value(uuid, "802-11-wireless-security.key-mgmt")
        if security not in ("wpa-psk", "sae", "", "--"):
            raise RuntimeError(f"SSID {ssid!r}: only open and personal password networks are supported.")
        # Hex SSIDs avoid INI escaping and do not depend on the watch's MAC.
        ssid_hex = ssid.encode().hex()
        section = [f"[service_{ssid_hex}]", "Type=wifi", f"SSID={ssid_hex}"]
        # NM stores SAE passwords in the same PSK property. Export the
        # passphrase, leaving authentication negotiation to the target stack.
        # This does not add WPA3 support to the image or its Wi-Fi driver.
        if security in ("wpa-psk", "sae"):
            password = nm_value(uuid, "802-11-wireless-security.psk", secret=True)
            if not password or password == "--":
                raise RuntimeError(f"Password unavailable for SSID {ssid!r}; check NetworkManager secret access.")
            if any(c in password for c in "\r\n\0"):
                raise RuntimeError("A selected password contains unsupported control characters.")
            # ConnMan uses GLib key-file escaping.
            escaped = password.replace("\\", "\\\\").replace(" ", "\\s").replace("\t", "\\t")
            section.append("Passphrase=" + escaped)
        if nm_value(uuid, "802-11-wireless.hidden") == "yes":
            section.append("Hidden=true")
        sections.append("\n".join(section))
    return ("\n\n".join(sections) + "\n").encode()


def public_keys(paths):
    if not paths:
        paths = [p for name in ("id_ed25519.pub", "id_ecdsa.pub", "id_rsa.pub")
                 if (p := Path.home() / ".ssh" / name).is_file()][:1]
    if not paths:
        raise RuntimeError("No default SSH public key found; use --ssh-key PATH.")
    keys = []
    for path in paths:
        if run(["ssh-keygen", "-l", "-f", str(path)]).returncode:
            raise RuntimeError(f"Invalid SSH public key: {path}")
        lines = path.read_text().splitlines()
        if len(lines) != 1 or not re.match(r"^(ssh-|ecdsa-|sk-)\S+ \S+", lines[0]):
            raise RuntimeError(f"Expected a single public key, not a private key: {path}")
        keys.append(lines[0])
    return keys


def ssh_host_key(path):
    """Load and validate one OpenSSH host private/public key pair."""
    path = Path(path)
    public_path = Path(str(path) + ".pub")
    if path.is_symlink() or public_path.is_symlink() or not path.is_file() or not public_path.is_file():
        raise RuntimeError("SSH host key and matching .pub file must be regular files.")
    if stat.S_IMODE(path.stat().st_mode) & 0o077:
        raise RuntimeError("SSH host private key must not be accessible by group or other users.")
    result = run(["ssh-keygen", "-y", "-f", str(path)])
    if result.returncode:
        raise RuntimeError("Invalid SSH host private key (details suppressed).")
    derived = result.stdout.decode().split()
    public_lines = public_path.read_text().splitlines()
    if len(derived) < 2 or len(public_lines) != 1:
        raise RuntimeError("Invalid SSH host public key.")
    public = public_lines[0].split()
    if len(public) < 2 or public[:2] != derived[:2]:
        raise RuntimeError("SSH host private and public keys do not match.")
    algorithm = public[0]
    if algorithm.startswith("ecdsa-sha2-"):
        key_name = "ecdsa"
    elif algorithm == "ssh-ed25519":
        key_name = "ed25519"
    elif algorithm == "ssh-rsa":
        key_name = "rsa"
    else:
        raise RuntimeError("Unsupported SSH host key algorithm.")
    return key_name, path.read_bytes(), (public_lines[0] + "\n").encode()


def quote(path):
    path = str(path)
    if any(c in path for c in '\n\r\0"\\'):
        raise RuntimeError("Unsupported character in image path.")
    return '"' + path + '"'


class Image:
    def __init__(self, path, scratch):
        self.path, self.scratch = path, scratch

    def command(self, command, write=False):
        result = run(["debugfs"] + (["-w"] if write else []) + ["-R", command, str(self.path)])
        # debugfs often exits zero on failure. Treat any diagnostic after its
        # version banner as failure; never print its captured output/secrets.
        diagnostics = result.stderr.decode(errors="replace").splitlines()
        if diagnostics and diagnostics[0].startswith("debugfs "):
            diagnostics.pop(0)
        if result.returncode or diagnostics:
            raise RuntimeError("debugfs failed while accessing the image (details suppressed).")
        return result.stdout

    def stat(self, path):
        result = run(["debugfs", "-R", "stat " + quote(path), str(self.path)])
        if b"File not found by ext2_lookup" in result.stderr:
            return None
        output = self.command("stat " + quote(path)).decode()
        match = re.search(r"Type:\s+(\w+)", output)
        if not match:
            raise RuntimeError("Could not inspect image inode.")
        return match[1]

    def directory(self, path):
        current = PurePosixPath("/")
        for component in PurePosixPath(path).parts[1:]:
            current /= component
            kind = self.stat(current)
            if kind is None:
                self.command("mkdir " + quote(current), True)
                self.metadata(current, 0o40755)
            elif kind != "directory":
                raise RuntimeError(f"Expected a real directory in image: {current}")

    def read(self, path):
        kind = self.stat(path)
        if kind is None:
            return b""
        if kind != "regular":
            raise RuntimeError(f"Expected a regular file in image: {path}")
        return self.command("cat " + quote(path))

    def metadata(self, path, mode):
        for field, value in (("mode", oct(mode)), ("uid", "0"), ("gid", "0")):
            # debugfs accepts C-style octal, not Python's 0o prefix.
            if field == "mode":
                value = "0" + format(mode, "o")
            self.command(f"set_inode_field {quote(path)} {field} {value}", True)

    def write(self, path, data, mode=0o100600):
        self.directory(str(PurePosixPath(path).parent))
        old = self.read(path)
        if old != data or self.stat(path) is None:
            if self.stat(path) is not None:
                self.command("rm " + quote(path), True)
            payload = self.scratch / "payload"
            payload.write_bytes(data)
            payload.chmod(0o600)
            self.command(f"write {quote(payload)} {quote(path)}", True)
        self.metadata(path, mode)
        if self.read(path) != data:
            raise RuntimeError("Image content verification failed.")


def enable_ble_ssh(image):
    unit = "/usr/lib/systemd/system/ble-ssh-watch.service"
    if image.stat(unit) != "regular" or image.stat("/usr/bin/ble-ssh-watch") != "regular":
        raise RuntimeError("Image does not contain ble-ssh-watch; rebuild with HOKI_BLE_SSH=1.")
    # Refuse masked or overridden units rather than claiming an unusable enable.
    if image.stat("/etc/systemd/system/ble-ssh-watch.service") is not None:
        raise RuntimeError("Image overrides ble-ssh-watch.service; resolve the override before enabling.")
    wants = "/etc/systemd/system/multi-user.target.wants"
    image.directory(wants)
    link = wants + "/ble-ssh-watch.service"
    kind = image.stat(link)
    if kind is None:
        image.command(f"symlink {quote(link)} {quote(unit)}", True)
    elif kind != "symlink":
        raise RuntimeError("Unexpected non-symlink at ble-ssh-watch enable path.")
    # Preserve this explicit opt-in if first boot applies distribution presets.
    image.write("/etc/systemd/system-preset/00-hoki-ble-ssh.preset",
                b"enable ble-ssh-watch.service\n", 0o100644)
    details = image.command("stat " + quote(link)).decode()
    if f'Fast link dest: "{unit}"' not in details:
        raise RuntimeError("Unexpected ble-ssh-watch enable link target.")


def tailscale_state(path):
    """Read an explicit private snapshot, never log its keys or contents."""
    path = Path(path)
    if path.is_symlink() or not path.is_file():
        raise RuntimeError("Tailscale state must be a regular private file.")
    if stat.S_IMODE(path.stat().st_mode) & 0o077:
        raise RuntimeError("Tailscale state must not be accessible by group or other users.")
    data = path.read_bytes()
    try:
        state = json.loads(data)
    except (ValueError, UnicodeError):
        raise RuntimeError("Invalid Tailscale state JSON (details suppressed).") from None
    if not isinstance(state, dict) or not state.get("_machinekey"):
        raise RuntimeError("Expected unencrypted Tailscale state with a machine identity.")
    return data


def restore_tailscale(image, state):
    unit = "/usr/lib/systemd/system/tailscaled.service"
    if any(image.stat(path) != "regular" for path in
           (unit, "/usr/bin/tailscale", "/usr/sbin/tailscaled")):
        raise RuntimeError("Image does not contain Tailscale; rebuild with the Hoki Tailscale recipe.")
    if image.stat("/etc/systemd/system/tailscaled.service") is not None:
        raise RuntimeError("Image overrides tailscaled.service; resolve before restoring state.")
    image.directory("/var/lib/tailscale")
    image.metadata("/var/lib/tailscale", 0o40700)
    image.write("/var/lib/tailscale/tailscaled.state", state)
    wants = "/etc/systemd/system/multi-user.target.wants"
    image.directory(wants)
    link = wants + "/tailscaled.service"
    kind = image.stat(link)
    if kind is None:
        image.command(f"symlink {quote(link)} {quote(unit)}", True)
    elif kind != "symlink":
        raise RuntimeError("Unexpected non-symlink at tailscaled enable path.")
    if f'Fast link dest: "{unit}"' not in image.command("stat " + quote(link)).decode():
        raise RuntimeError("Unexpected tailscaled enable link target.")
    image.write("/etc/systemd/system-preset/00-hoki-tailscale.preset",
                b"enable tailscaled.service\n", 0o100644)


def local_timezone():
    """Use the workstation's IANA zone, including NixOS zoneinfo symlinks."""
    target = str(Path("/etc/localtime").resolve())
    if "/zoneinfo/" in target:
        return target.split("/zoneinfo/", 1)[1]
    try:
        zone = Path("/etc/timezone").read_text().strip()
        if zone:
            return zone
    except OSError:
        pass
    raise RuntimeError("Cannot determine workstation timezone; use --timezone IANA_ZONE.")


def apply_timezone(image, zone):
    if not re.fullmatch(r"[A-Za-z0-9_+-]+(?:/[A-Za-z0-9_+-]+)*", zone):
        raise RuntimeError("Invalid IANA timezone name.")
    target = "/usr/share/zoneinfo/" + zone
    # Most zones are regular files. Aliases need an explicit canonical zone.
    if image.stat(target) != "regular":
        raise RuntimeError("Timezone is missing from image or an alias; select its canonical IANA zone.")
    current = image.stat("/etc/localtime")
    if current not in (None, "regular", "symlink"):
        raise RuntimeError("Unexpected image /etc/localtime type.")
    if current is not None:
        image.command("rm /etc/localtime", True)
    image.command(f"symlink /etc/localtime {quote(target)}", True)
    image.write("/etc/timezone", (zone + "\n").encode(), 0o100644)


def apply_settings(image, keys, wifi, host_key, ble_ssh=False, tailscale=None, timezone=None):
    if timezone is not None:
        apply_timezone(image, timezone)
    if tailscale is not None:
        restore_tailscale(image, tailscale)
    if ble_ssh:
        enable_ble_ssh(image)
    root = next((line.split(":") for line in image.read("/etc/passwd").decode().splitlines()
                 if line.startswith("root:")), None)
    if not root or len(root) != 7 or root[2] != "0":
        raise RuntimeError("Cannot determine the image's root home directory.")
    home = PurePosixPath(root[5])
    if not home.is_absolute() or ".." in home.parts or home == PurePosixPath("/"):
        raise RuntimeError("Unsupported root home directory.")
    ssh_dir = str(home / ".ssh")
    image.directory(ssh_dir)
    image.metadata(ssh_dir, 0o40700)
    authorized = ssh_dir + "/authorized_keys"
    existing = image.read(authorized).decode()
    lines = existing.splitlines()
    for key in keys:
        identity = key.split()[:2]
        def already_present(line):
            if not line.strip() or line.lstrip().startswith("#"):
                return False
            try:
                fields = shlex.split(line)
            except ValueError:
                raise RuntimeError("Malformed existing authorized_keys entry.") from None
            return any(fields[i:i + 2] == identity for i in range(len(fields) - 1))
        if not any(already_present(line) for line in lines):
            lines.append(key)
    image.write(authorized, ("\n".join(lines) + "\n").encode())
    key_name, private_key, public_key = host_key
    host_key_path = f"/etc/ssh/ssh_host_{key_name}_key"
    image.write(host_key_path, private_key)
    image.write(host_key_path + ".pub", public_key, 0o100644)
    image.write("/var/lib/connman/hokipersonal.config", wifi)
    settings_path = "/var/lib/connman/settings"
    parser = configparser.ConfigParser(interpolation=None)
    parser.optionxform = str
    parser.read_string(image.read(settings_path).decode())
    changes = [("global", "OfflineMode", "false"), ("WiFi", "Enable", "true")]
    if ble_ssh:
        changes.append(("Bluetooth", "Enable", "true"))
    for section, key, value in changes:
        if not parser.has_section(section):
            parser.add_section(section)
        parser[section][key] = value
    output = io.StringIO()
    parser.write(output, space_around_delimiters=False)
    image.write(settings_path, output.getvalue().encode())


def check_fs(path):
    if run(["e2fsck", "-f", "-n", str(path)]).returncode:
        raise RuntimeError("Image failed the read-only filesystem check; output was not replaced.")


def personalize(source, destination, keys, wifi, host_key, ble_ssh=False, tailscale=None, timezone=None):
    if not source.is_file() or destination.is_symlink():
        raise RuntimeError("Source must be a regular image; output must not be a symlink.")
    destination.parent.mkdir(parents=True, exist_ok=True)
    # Same-filesystem temporary output allows atomic replacement, even when
    # source and destination are the same. Failure leaves the original intact.
    with tempfile.TemporaryDirectory(prefix=".hoki-personalize-", dir=destination.parent) as directory:
        scratch = Path(directory).resolve()
        working = scratch / "image.ext4"
        with working.open("xb"):
            os.chmod(working, 0o600)
        result = run(["cp", "--reflink=auto", "--sparse=always", "--", str(source), str(working)])
        if result.returncode:
            raise RuntimeError("Could not copy source image.")
        check_fs(working)
        apply_settings(Image(working, scratch), keys, wifi, host_key, ble_ssh, tailscale, timezone)
        check_fs(working)
        os.chmod(working, 0o600)
        os.replace(working, destination)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("image", type=Path, help="unmounted raw ext4 image")
    parser.add_argument("--output", type=Path, help="default: IMAGE.personalized.ext4; may equal input")
    parser.add_argument("--ssh-key", type=Path, action="append", help="public key file; repeat for multiple keys")
    parser.add_argument("--ssh-host-key", type=Path, required=True,
                        help="host private key; matching PATH.pub is required")
    parser.add_argument("--wifi-ssid", action="append", required=True,
                        help="exact NetworkManager SSID to include; repeat for multiple networks")
    parser.add_argument("--enable-ble-ssh", action="store_true",
                        help="enable the image's Bluetooth SSH daemon and ConnMan Bluetooth at boot")
    parser.add_argument("--tailscale-state", type=Path,
                        help="private tailscaled.state snapshot from this watch; restore identity and enable Tailscale")
    parser.add_argument("--timezone", help="IANA timezone; defaults to workstation timezone")
    args = parser.parse_args()
    for tool in ("nmcli", "debugfs", "e2fsck", "ssh-keygen", "cp"):
        if not shutil.which(tool):
            parser.exit(1, f"Missing required tool: {tool}\n")
    output = args.output or args.image.with_suffix(".personalized.ext4")
    try:
        timezone = args.timezone or local_timezone()
        keys = public_keys(args.ssh_key)
        host_key = ssh_host_key(args.ssh_host_key)
        wifi = wifi_config(args.wifi_ssid)
        ts_state = tailscale_state(args.tailscale_state) if args.tailscale_state else None
        personalize(args.image, output, keys, wifi, host_key, args.enable_ble_ssh, ts_state, timezone)
    except (RuntimeError, OSError, ValueError, configparser.Error) as error:
        # Never include subprocess output or config parsing errors containing credentials.
        message = str(error) if isinstance(error, RuntimeError) else type(error).__name__
        parser.exit(1, f"Personalization failed: {message}\n")
    print(f"Personalized image: {output} ({len(keys)} SSH key(s), stable SSH host identity, "
          f"{len(args.wifi_ssid)} Wi-Fi network(s), timezone {timezone})")


if __name__ == "__main__":
    main()
