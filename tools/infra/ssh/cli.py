"""Run remote workspace operations through the sandbox's paired SSH proxy."""
import argparse
import base64
import contextlib
import json
import os
from pathlib import Path
import shlex
import subprocess
import sys
import tempfile
from urllib.parse import urlsplit


@contextlib.contextmanager
def connection(destination):
    """Materialize only the disposable client key and pinned proxy identity."""
    config = json.loads(os.environ.get("CENTAUR_SSH") or os.environ["CENTAUR_EE_SSH"])
    parsed = urlsplit(destination)
    if (len(destination.encode()) > 2048 or parsed.scheme != "wss" or not parsed.hostname
            or parsed.port not in (None, 443) or parsed.username is not None
            or parsed.password is not None or parsed.query or parsed.fragment):
        raise ValueError("invalid WSS SSH destination")
    # Preserve the route encoding understood by existing Iron deployments.
    route = "ee:" + base64.urlsafe_b64encode(destination.encode()).decode().rstrip("=")
    with tempfile.TemporaryDirectory(prefix="centaur-ssh-") as directory:
        root = Path(directory)
        key = root / "key"
        key.write_text(config["private_key"])
        key.chmod(0o600)
        hosts = root / "known_hosts"
        hosts.write_text(f'[{config["host"]}]:{config["port"]} {config["host_key"]}\n')
        yield config, [
            "ssh", "-F", "/dev/null", "-T", "-p", str(config["port"]),
            "-i", str(key), "-o", "IdentitiesOnly=yes", "-o", "BatchMode=yes",
            "-o", "StrictHostKeyChecking=yes", "-o", f"UserKnownHostsFile={hosts}",
            "-o", "GlobalKnownHostsFile=/dev/null", "-o", "ForwardAgent=no",
            "-o", "ClearAllForwardings=yes", "-o", "ConnectTimeout=10",
            "-o", "LogLevel=ERROR", "-l", route, config["host"],
        ]


def app():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="operation", required=True)
    run = sub.add_parser("exec", help="Run a command in the remote workspace")
    run.add_argument("--timeout", type=float, default=300)
    run.add_argument("command", nargs=argparse.REMAINDER)
    read = sub.add_parser("read", help="Read a remote file")
    read.add_argument("path")
    write = sub.add_parser("write", help="Write stdin to a remote file")
    write.add_argument("path")
    for operation in [run, read, write]:
        operation.add_argument("--url", required=True, help="Full WSS SSH URL; must match deployment policy")
    args = parser.parse_args()
    try:
        with connection(args.url) as (config, ssh):
            if args.operation == "exec":
                command = args.command
                if command[:1] == ["--"]:
                    command = command[1:]
                if not command:
                    parser.error("exec requires a command")
                command = shlex.join(command)
            elif args.operation == "read":
                command = "cat -- " + shlex.quote(args.path)
            else:
                command = "cat > " + shlex.quote(args.path)
            script = "cd -- " + shlex.quote(config["workspace"]) + " && " + command
            result = subprocess.run(ssh + [script], timeout=getattr(args, "timeout", 300), check=False)
            return result.returncode
    except subprocess.TimeoutExpired:
        print("Remote command timed out; its remote completion is unknown. Do not replay automatically.", file=sys.stderr)
        return 124
    except (KeyError, ValueError, OSError):
        print("SSH configuration unavailable or invalid", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(app())
