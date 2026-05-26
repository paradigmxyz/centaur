import os
import subprocess
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[3]
GIT_BRANCH = REPO_ROOT / "services" / "sandbox" / "git-branch.sh"


def _run(cmd: list[str], *, cwd: Path | None = None, env: dict[str, str] | None = None) -> str:
    result = subprocess.run(
        cmd,
        cwd=cwd,
        env=env,
        check=True,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    return result.stdout.strip()


def _create_source_repo(home: Path, repo: str) -> Path:
    src = home / "github" / repo
    src.mkdir(parents=True)
    _run(["git", "init", "-q"], cwd=src)
    (src / "README.md").write_text("# test\n")
    _run(["git", "add", "README.md"], cwd=src)
    _run(
        [
            "git",
            "-c",
            "user.name=Test User",
            "-c",
            "user.email=test@example.com",
            "commit",
            "-q",
            "-m",
            "initial commit",
        ],
        cwd=src,
    )
    return src


def test_git_branch_uses_slug_with_timestamp_suffix(tmp_path: Path) -> None:
    repo = "owner/project"
    _create_source_repo(tmp_path, repo)

    env = os.environ.copy()
    env["HOME"] = str(tmp_path)

    dest = Path(
        _run(
            ["bash", str(GIT_BRANCH), repo, "fix-flaky-slack-reply-delivery"],
            env=env,
        )
    )

    branch = _run(["git", "branch", "--show-current"], cwd=dest)
    assert branch.startswith("centaur/fix-flaky-slack-reply-delivery-")
    assert branch.rsplit("-", 1)[-1].isdigit()


def test_git_branch_falls_back_to_centaur_random_branch(tmp_path: Path) -> None:
    repo = "owner/project"
    _create_source_repo(tmp_path, repo)

    env = os.environ.copy()
    env["HOME"] = str(tmp_path)

    dest = Path(_run(["bash", str(GIT_BRANCH), repo], env=env))

    branch = _run(["git", "branch", "--show-current"], cwd=dest)
    assert branch.startswith("centaur/")
    assert not branch.startswith("agent-")
