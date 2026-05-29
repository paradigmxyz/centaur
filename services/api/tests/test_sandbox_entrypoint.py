from __future__ import annotations

import json
import os
import subprocess
from pathlib import Path


ENTRYPOINT_SH = Path(__file__).resolve().parents[2] / "sandbox" / "entrypoint.sh"
GIT_CACHE_WRAPPER_SH = (
    Path(__file__).resolve().parents[2] / "sandbox" / "git-cache-wrapper.sh"
)


def _write_codex_harness_config(home: Path) -> Path:
    harness_dir = home / "harness"
    codex_dir = harness_dir / "codex"
    codex_dir.mkdir(parents=True)
    (codex_dir / "config.toml").write_text(
        "\n".join(
            [
                'model = "gpt-5.5"',
                'model_reasoning_effort = "low"',
                'plan_mode_reasoning_effort = "high"',
                'approval_policy = "on-request"',
                'approvals_reviewer = "user"',
                'web_search = "live"',
                'personality = "pragmatic"',
                'sandbox_mode = "workspace-write"',
                "check_for_update_on_startup = true",
                "suppress_unstable_features_warning = true",
                'service_tier = "fast"',
                "",
                "[tools]",
                "view_image = true",
                "",
                "[features]",
                "goals = true",
                "memories = true",
                "code_mode = true",
                "hooks = true",
                "browser_use = true",
                "computer_use = true",
                "enable_fanout = true",
                "runtime_metrics = true",
                "",
                "[features.multi_agent_v2]",
                "enabled = true",
                "max_concurrent_threads_per_session = 6",
                "",
                "[agents]",
                "max_depth = 2",
                "job_max_runtime_seconds = 1800",
                "",
            ]
        )
    )
    return harness_dir


def test_sandbox_entrypoint_bootstraps_mock_google_adc(tmp_path: Path) -> None:
    home = tmp_path / "home"
    (home / ".config" / "amp").mkdir(parents=True)
    harness_dir = _write_codex_harness_config(home)

    result = subprocess.run(
        [
            "bash",
            str(ENTRYPOINT_SH),
            "sh",
            "-lc",
            'printf \'%s\n\' "$GOOGLE_APPLICATION_CREDENTIALS" && cat "$GOOGLE_APPLICATION_CREDENTIALS"',
        ],
        check=False,
        capture_output=True,
        text=True,
        env={
            "HOME": str(home),
            "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
            "CENTAUR_HARNESS_CONFIG_DIR": str(harness_dir),
        },
    )

    assert result.returncode == 0, result.stderr or result.stdout
    adc_path, adc_json = result.stdout.split("\n", 1)
    assert adc_path == str(
        home / ".config" / "gcloud" / "application_default_credentials.json"
    )
    assert Path(adc_path).is_file()
    adc = json.loads(adc_json)
    assert adc == {
        "type": "service_account",
        "project_id": "centaur-sandbox",
        "private_key_id": "0000000000000000000000000000000000000000",
        "private_key": adc["private_key"],
        "client_email": "mock@creds.com",
        "client_id": "100000000000000000000",
        "auth_uri": "https://accounts.google.com/o/oauth2/auth",
        "token_uri": "https://oauth2.googleapis.com/token",
        "auth_provider_x509_cert_url": "https://www.googleapis.com/oauth2/v1/certs",
        "client_x509_cert_url": "https://www.googleapis.com/robot/v1/metadata/x509/mock%40creds.com",
        "universe_domain": "googleapis.com",
    }
    assert adc["private_key"].startswith("-----BEGIN PRIVATE KEY-----\n")
    assert adc["private_key"].endswith("-----END PRIVATE KEY-----\n")

    codex_config = (home / ".codex" / "config.toml").read_text()
    assert 'model = "gpt-5.5"' in codex_config
    assert 'model_reasoning_effort = "low"' in codex_config
    assert 'plan_mode_reasoning_effort = "high"' in codex_config
    assert 'approval_policy = "on-request"' in codex_config
    assert 'sandbox_mode = "workspace-write"' in codex_config
    assert 'service_tier = "fast"' in codex_config
    assert "max_concurrent_threads_per_session = 6" in codex_config


def test_sandbox_entrypoint_installs_codex_harness_config(tmp_path: Path) -> None:
    home = tmp_path / "home"
    harness_dir = _write_codex_harness_config(home)

    result = subprocess.run(
        [
            "bash",
            str(ENTRYPOINT_SH),
            "sh",
            "-lc",
            'cat "$HOME/.codex/config.toml"',
        ],
        check=False,
        capture_output=True,
        text=True,
        env={
            "HOME": str(home),
            "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
            "CENTAUR_HARNESS_CONFIG_DIR": str(harness_dir),
        },
    )

    assert result.returncode == 0, result.stderr or result.stdout
    assert result.stdout == (harness_dir / "codex" / "config.toml").read_text()


def test_sandbox_entrypoint_does_not_install_global_git_cache_rewrites(
    tmp_path: Path,
) -> None:
    home = tmp_path / "home"
    harness_dir = _write_codex_harness_config(home)

    result = subprocess.run(
        [
            "bash",
            str(ENTRYPOINT_SH),
            "sh",
            "-lc",
            "git config --global --get-all url.http://repo-cache:8080/repos/github.com/.insteadOf",
        ],
        check=False,
        capture_output=True,
        text=True,
        env={
            "HOME": str(home),
            "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
            "CENTAUR_GIT_CACHE_URL": "http://repo-cache:8080/repos/",
            "CENTAUR_HARNESS_CONFIG_DIR": str(harness_dir),
        },
    )

    assert result.returncode == 1
    assert result.stdout == ""


def test_git_cache_wrapper_does_not_rewrite_push(tmp_path: Path) -> None:
    real_git = tmp_path / "git"
    calls = tmp_path / "calls"
    real_git.write_text(
        "#!/bin/sh\n"
        "printf '%s\\n' \"$*\" >> \"$GIT_WRAPPER_CALLS\"\n"
    )
    real_git.chmod(0o755)

    env = {
        **os.environ,
        "CENTAUR_REAL_GIT": str(real_git),
        "CENTAUR_GIT_CACHE_URL": "http://repo-cache:8080/repos/",
        "GIT_WRAPPER_CALLS": str(calls),
    }

    clone = subprocess.run(
        [
            "bash",
            str(GIT_CACHE_WRAPPER_SH),
            "clone",
            "https://github.com/tarrencev/centaur.git",
        ],
        check=False,
        capture_output=True,
        text=True,
        env=env,
    )
    push = subprocess.run(
        [
            "bash",
            str(GIT_CACHE_WRAPPER_SH),
            "push",
            "origin",
            "test-branch",
        ],
        check=False,
        capture_output=True,
        text=True,
        env=env,
    )

    assert clone.returncode == 0, clone.stderr or clone.stdout
    assert push.returncode == 0, push.stderr or push.stdout
    clone_call, push_call = calls.read_text().splitlines()
    assert "url.http://repo-cache:8080/repos/github.com/.insteadOf=https://github.com/" in clone_call
    assert "url.http://repo-cache:8080/repos/github.com/.insteadOf" not in push_call
    assert push_call == "push origin test-branch"
