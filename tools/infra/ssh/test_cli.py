"""Remote argument quoting and disposable-identity handling."""
import importlib.util
import json
import os
from pathlib import Path
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("ssh_cli", Path(__file__).with_name("cli.py"))
cli = importlib.util.module_from_spec(spec)
spec.loader.exec_module(cli)


class ClientTests(unittest.TestCase):
    def test_destination_uses_disposable_key_and_proxy_host_pin(self):
        import base64
        config = {"host":"proxy","port":2222,"route":"ee","workspace":"/workspace","private_key":"disposable","host_key":"ssh-ed25519 fixture"}
        target="wss://one.dev.example.com/events/events"
        with patch.dict(os.environ,{"CENTAUR_SSH":json.dumps(config)}):
            with cli.connection(target) as (_, command):
                route=command[command.index("-l")+1]
                self.assertEqual(base64.urlsafe_b64decode(route[3:]+"="*(-len(route[3:])%4)).decode(),target)
                self.assertEqual(command[-1],"proxy")
                self.assertIn("StrictHostKeyChecking=yes",command)
            for invalid in ["tcp://host:22","wss://user:secret@host/path","wss://host/path?token=secret","wss://host:8443/path"]:
                with self.assertRaises(ValueError):
                    with cli.connection(invalid):
                        self.fail("invalid URL accepted")
    def test_disposable_key_is_private_and_removed(self):
        config = {"host": "proxy", "port": 2222, "route": "ee", "workspace": "/workspace", "private_key": "disposable", "host_key": "ssh-ed25519 fixture"}
        with patch.dict(os.environ, {"CENTAUR_SSH": json.dumps(config)}):
            with cli.connection("wss://dev.example.com/events/events") as (_, command):
                key = Path(command[command.index("-i") + 1])
                self.assertEqual(key.stat().st_mode & 0o777, 0o600)
                self.assertIn("StrictHostKeyChecking=yes", command)
                self.assertNotIn("disposable", command)
            self.assertFalse(key.exists())

    def test_command_arguments_remain_literal(self):
        config = {"workspace": "/path with spaces"}
        with patch.object(cli, "connection") as connection, patch.object(cli.subprocess, "run") as run:
            connection.return_value.__enter__.return_value = (config, ["ssh", "proxy"])
            run.return_value.returncode = 7
            with patch("sys.argv", ["remote-ssh", "exec", "--url", "wss://dev.example.com/events/events", "--", "printf", "%s", "$(touch bad)"]):
                self.assertEqual(cli.app(), 7)
            self.assertEqual(run.call_args.args[0][-1], "cd -- '/path with spaces' && printf %s '$(touch bad)'")


if __name__ == "__main__":
    unittest.main()
