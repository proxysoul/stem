# Security

Report a vulnerability privately through GitHub: **Security → Report a vulnerability** on this repository. Please do not open a public issue for it.

You get an answer within a few days. Fixes ship in the next release, with credit if you want it.

## What Stem trusts

- Everything Stem keeps about a project (what it learned, saved tools, the fact store, logs) lives on your machine in `~/.stem/projects/`. Nothing a repository ships is loaded from there.
- A repository can ship `.stem/rules.json`, but those rules can only block calls. Rules that change a call live in `~/.stem/rules.json`.
- From a repository's `.env`, Stem reads only `*_API_KEY` variables, and your own in `~/.stem/.env` come first. Stem settings and other tokens in it are ignored.
- A repository's rules can block a call and say why, never change it; their message is marked as the project's.
- Providers and agents you plug in (grafts) are read only from `~/.stem/grafts`.
- Release binaries are built by GitHub Actions from a tagged commit, and each one ships with a SHA-256 checksum. Release tags cannot be moved or deleted.
