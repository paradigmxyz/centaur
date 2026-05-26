"""Credentials-only tool. The Codex CLI in the sandbox calls chatgpt.com
directly; iron-proxy injects the brokered OAuth bearer and account-id
header declared in pyproject.toml. No tool methods are exposed.
"""
