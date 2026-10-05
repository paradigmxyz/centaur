# Centaur QA Report

| Field | Value |
|-------|-------|
| **Date** | {DATE} |
| **Tool scope** | {sample / full} |
| **Layer scope** | {all / layers 1-2 / layer 1 only} |

## Layer Summary

| Layer | Status | Notes |
|-------|--------|-------|
| 1. Internal API | ⬜ | |
| 2. HTTP edge | ⬜ | |
| 3a. Slackbot | ⬜ | |

---

## Layer 1: Internal API

### Services

| Service | Status | Notes |
|---------|--------|-------|
| postgres | ⬜ | |
| secrets | ⬜ | |
| firewall | ⬜ | |
| api | ⬜ | |
| ingress/http-route | ⬜ | |
| auth | ⬜ | |
| slackbot | ⬜ | |
| web | ⬜ | |

- **Tools loaded:** {N}
- **Warm pool:** {N} pods

### Tool Testing

| Tool | Method | Status | Notes |
|------|--------|--------|-------|

**Summary:** {N} pass, {N} fail, {N} skip, {N} warn

### Agent Execute

| Test | Status | Notes |
|------|--------|-------|
| SSE stream | ⬜ | |
| turn.done result | ⬜ | |
| Container cleanup | ⬜ | |

### Personas

| Persona | Status | AGENT_PERSONA | Prompt distinct | Notes |
|---------|--------|---------------|-----------------|-------|

### Log Pipeline

| Test | Status | Notes |
|------|--------|-------|
| Services in VictoriaLogs | ⬜ | |
| _msg populated | ⬜ | |
| Structured fields | ⬜ | |
| Sandbox logs | ⬜ | |

---

## Layer 2: Nginx

| Test | Status | Notes |
|------|--------|-------|
| Health endpoint | ⬜ | |
| Tools via Bearer auth | ⬜ | |
| Sample tool call | ⬜ | |
| Agent execute | ⬜ | |
| Auth gate (302) | ⬜ | |

---

## Layer 3a: Slackbot

| Test | Status | Notes |
|------|--------|-------|
| url_verification (signed) | ⬜ | |
| Bad signature → 401 | ⬜ | |
| Via nginx webhook route | ⬜ | |

---

## Issues Found

### {Issue title}

- **Layer:** {1 / 2 / 3a}
- **Request:** {what was called}
- **Error:** {error message}
- **Root cause:** {analysis}
- **Suggested fix:** {what to change}
- **Fixed:** ✅ / ❌

### Missing Credentials

| Tool | Missing Secret | Where to Add |
|------|---------------|--------------|
