import importlib.util
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("personalize", Path(__file__).parents[1] / "personalize-image.py")
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class PersonalizeTests(unittest.TestCase):
    def test_timezone_replaces_symlink_and_is_repeatable(self):
        with tempfile.TemporaryDirectory() as temp:
            directory = Path(temp)
            tree = directory / "rootfs"
            (tree / "etc").mkdir(parents=True)
            (tree / "usr/share/zoneinfo/Europe").mkdir(parents=True)
            (tree / "usr/share/zoneinfo/Europe/Berlin").write_bytes(b"TZif-test")
            (tree / "etc/localtime").symlink_to("/usr/share/zoneinfo/Universal")
            source = directory / "image.ext4"
            with source.open("wb") as stream:
                stream.truncate(16 * 1024 * 1024)
            subprocess.run(["mkfs.ext4", "-q", "-F", "-d", str(tree), str(source)], check=True)
            image = module.Image(source, directory)
            for _ in range(2):
                module.apply_timezone(image, "Europe/Berlin")
                self.assertEqual(image.read("/etc/timezone"), b"Europe/Berlin\n")
                self.assertIn(b"/usr/share/zoneinfo/Europe/Berlin", image.command("stat /etc/localtime"))
            before = source.read_bytes()
            for invalid in ("../etc/passwd", "Missing/Zone"):
                with self.assertRaises(RuntimeError):
                    module.apply_timezone(image, invalid)
                self.assertEqual(source.read_bytes(), before)
            module.check_fs(source)

    def test_ext4_repeat_preserves_keys_settings_and_permissions(self):
        with tempfile.TemporaryDirectory() as temp:
            directory = Path(temp)
            tree = directory / "rootfs"
            (tree / "etc").mkdir(parents=True)
            (tree / "etc/passwd").write_text("root:x:0:0:root:/home/root:/bin/sh\n")
            ssh = tree / "home/root/.ssh"
            ssh.mkdir(parents=True)
            # A matching restricted key must not gain an unrestricted duplicate.
            (ssh / "authorized_keys").write_text('restrict ssh-ed25519 AAAA existing\nssh-rsa BBBB other\n')
            connman = tree / "var/lib/connman"
            connman.mkdir(parents=True)
            (connman / "settings").write_text("[global]\nOfflineMode=true\n[Bluetooth]\nEnable=true\n")
            unit_dir = tree / "usr/lib/systemd/system"
            unit_dir.mkdir(parents=True)
            (unit_dir / "ble-ssh-watch.service").write_text("[Service]\nExecStart=/usr/bin/ble-ssh-watch\n")
            (tree / "usr/bin").mkdir(parents=True)
            (tree / "usr/bin/ble-ssh-watch").write_text("synthetic binary")
            (unit_dir / "tailscaled.service").write_text("[Service]\nExecStart=/usr/sbin/tailscaled\n")
            (tree / "usr/sbin").mkdir()
            (tree / "usr/sbin/tailscaled").write_text("synthetic binary")
            (tree / "usr/bin/tailscale").write_text("synthetic binary")
            source = directory / "original.ext4"
            with source.open("wb") as stream:
                stream.truncate(32 * 1024 * 1024)
            subprocess.run(["mkfs.ext4", "-q", "-F", "-d", str(tree), str(source)], check=True)
            original = source.read_bytes()
            output = directory / "personal.ext4"
            keys = ["ssh-ed25519 AAAA changed-comment", "ssh-ed25519 CCCC new"]
            wifi = b"[service_test]\nType=wifi\nSSID=74657374\nPassphrase=testpassword\n"
            host_private = directory / "ssh_host_ecdsa_key"
            subprocess.run(["ssh-keygen", "-q", "-t", "ecdsa", "-N", "", "-f", str(host_private)], check=True)
            host_key = module.ssh_host_key(host_private)
            module.personalize(source, output, keys, wifi, host_key)
            image = module.Image(output, directory)
            paths = ["/home/root/.ssh/authorized_keys", "/var/lib/connman/settings",
                     "/var/lib/connman/hokipersonal.config", "/etc/ssh/ssh_host_ecdsa_key",
                     "/etc/ssh/ssh_host_ecdsa_key.pub"]
            first = [image.read(path) for path in paths]
            module.personalize(output, output, keys, wifi, host_key)
            self.assertEqual(first, [image.read(path) for path in paths])
            self.assertEqual(source.read_bytes(), original)
            self.assertEqual(first[0].count(b"AAAA"), 1)
            self.assertIn(b"restrict ssh-ed25519 AAAA existing", first[0])
            self.assertIn(b"ssh-rsa BBBB other", first[0])
            self.assertIn(b"[Bluetooth]\nEnable=true", first[1])
            self.assertIn(b"[WiFi]\nEnable=true", first[1])
            self.assertIn(b"OfflineMode=false", first[1])
            self.assertEqual(os.stat(output).st_mode & 0o777, 0o600)
            for path in paths[:-1]:
                stat = image.command("stat " + module.quote(path)).decode()
                self.assertRegex(stat, r"Mode:\s+0600")
                self.assertRegex(stat, r"User:\s+0\s+Group:\s+0")
            self.assertRegex(image.command("stat " + module.quote(paths[-1])).decode(), r"Mode:\s+0644")
            self.assertRegex(image.command('stat /home/root/.ssh').decode(), r"Mode:\s+0700")
            before = output.read_bytes()
            with patch.object(module, "apply_settings", side_effect=RuntimeError("synthetic failure")):
                with self.assertRaises(RuntimeError):
                    module.personalize(output, output, keys, wifi, host_key)
            self.assertEqual(output.read_bytes(), before)

            ts_path = "/var/lib/tailscale/tailscaled.state"
            self.assertIsNone(image.stat(ts_path))
            state = b'{"_machinekey":"synthetic-test-identity"}\n'
            for _ in range(2):
                module.personalize(output, output, keys, wifi, host_key, tailscale=state)
                self.assertEqual(image.read(ts_path), state)
                self.assertRegex(image.command("stat " + ts_path).decode(), r"Mode:\s+0600")
                self.assertRegex(image.command("stat /var/lib/tailscale").decode(), r"Mode:\s+0700")
                self.assertIn('Fast link dest: "/usr/lib/systemd/system/tailscaled.service"',
                              image.command("stat /etc/systemd/system/multi-user.target.wants/tailscaled.service").decode())
                self.assertEqual(image.read("/etc/systemd/system-preset/00-hoki-tailscale.preset"),
                                 b"enable tailscaled.service\n")
            # Omitting the option preserves the existing identity and enablement.
            module.personalize(output, output, keys, wifi, host_key)
            self.assertEqual(image.read(ts_path), state)
            image.command('rm /usr/sbin/tailscaled', True)
            before = output.read_bytes()
            with self.assertRaisesRegex(RuntimeError, "does not contain Tailscale"):
                module.personalize(output, output, keys, wifi, host_key, tailscale=state)
            self.assertEqual(output.read_bytes(), before)
            # Changing the allowlist replaces this tool's single managed file.
            module.personalize(output, output, keys,
                               b"[service_other]\nType=wifi\nSSID=6f74686572\n", host_key)
            self.assertNotIn(b"testpassword", image.read(paths[2]))
            link = "/etc/systemd/system/multi-user.target.wants/ble-ssh-watch.service"
            self.assertIsNone(image.stat(link))  # No implicit opt-in.
            for _ in range(2):
                module.personalize(output, output, keys, wifi, host_key, ble_ssh=True)
                self.assertEqual(image.stat(link), "symlink")
                self.assertEqual(image.read("/etc/systemd/system-preset/00-hoki-ble-ssh.preset"),
                                 b"enable ble-ssh-watch.service\n")
                self.assertIn('Fast link dest: "/usr/lib/systemd/system/ble-ssh-watch.service"',
                              image.command("stat " + module.quote(link)).decode())
                self.assertIn(b"[Bluetooth]\nEnable=true", image.read(paths[1]))
            image.command('rm /usr/bin/ble-ssh-watch', True)
            before = output.read_bytes()
            with self.assertRaisesRegex(RuntimeError, "does not contain"):
                module.personalize(output, output, keys, wifi, host_key, ble_ssh=True)
            self.assertEqual(output.read_bytes(), before)


    def test_host_key_validation_rejects_mismatch_and_weak_permissions(self):
        with tempfile.TemporaryDirectory() as temp:
            directory = Path(temp)
            first = directory / "first"
            second = directory / "second"
            for path in (first, second):
                subprocess.run(["ssh-keygen", "-q", "-t", "ecdsa", "-N", "", "-f", str(path)], check=True)
            first.chmod(0o644)
            with self.assertRaisesRegex(RuntimeError, "group or other"):
                module.ssh_host_key(first)
            first.chmod(0o600)
            Path(str(first) + ".pub").write_bytes(Path(str(second) + ".pub").read_bytes())
            with self.assertRaisesRegex(RuntimeError, "do not match"):
                module.ssh_host_key(first)

    def test_tailscale_state_validation(self):
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / "tailscaled.state"
            path.write_bytes(b'{"_machinekey":"synthetic-test-identity"}')
            path.chmod(0o600)
            self.assertEqual(module.tailscale_state(path), path.read_bytes())
            path.chmod(0o644)
            with self.assertRaisesRegex(RuntimeError, "group or other"):
                module.tailscale_state(path)
            path.chmod(0o600)
            for invalid in (b'{}', b'[]', b'secret malformed data'):
                path.write_bytes(invalid)
                with self.assertRaises(RuntimeError) as error:
                    module.tailscale_state(path)
                self.assertNotIn('secret malformed data', str(error.exception))
            link = Path(temp) / "link"
            link.symlink_to(path)
            with self.assertRaisesRegex(RuntimeError, "regular private file"):
                module.tailscale_state(link)

    def test_nm_allowlist_and_secret_escaping(self):
        queries = []
        def value(uuid, field, secret=False):
            queries.append((uuid, field, secret))
            if field.endswith(".ssid"):
                return {"one": "wanted", "two": "not selected"}[uuid]
            return {"802-11-wireless.mode": "infrastructure",
                    "802-11-wireless-security.key-mgmt": "wpa-psk",
                    "802-11-wireless-security.psk": " a\\b password ",
                    "802-11-wireless.hidden": "yes"}[field]
        listing = subprocess.CompletedProcess([], 0, b"one:802-11-wireless\ntwo:802-11-wireless\n", b"")
        with patch.object(module, "run", return_value=listing), patch.object(module, "nm_value", side_effect=value):
            content = module.wifi_config(["wanted"])
            self.assertIn(b"Passphrase=\\sa\\\\b\\spassword\\s", content)
            self.assertIn(b"Hidden=true", content)
            self.assertEqual([uuid for uuid, _, secret in queries if secret], ["one"])
            with self.assertRaisesRegex(RuntimeError, "found 0"):
                module.wifi_config(["missing"])

    def test_sae_password_export_and_missing_secret(self):
        listing = subprocess.CompletedProcess([], 0, b"one:802-11-wireless\n", b"")
        fields = {"802-11-wireless.ssid": "Example-WPA3",
                  "802-11-wireless.mode": "infrastructure",
                  "802-11-wireless-security.key-mgmt": "sae",
                  "802-11-wireless-security.psk": "synthetic-password",
                  "802-11-wireless.hidden": "no"}
        with patch.object(module, "run", return_value=listing), patch.object(
                module, "nm_value", side_effect=lambda uuid, field, secret=False: fields[field]):
            self.assertIn(b"Passphrase=synthetic-password", module.wifi_config(["Example-WPA3"]))
            fields["802-11-wireless-security.psk"] = ""
            with self.assertRaisesRegex(RuntimeError, "Password unavailable"):
                module.wifi_config(["Example-WPA3"])


if __name__ == "__main__":
    unittest.main()
