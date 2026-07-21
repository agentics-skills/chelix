# Third-Party Skills Security

Third-party skills and plugin repos are powerful and risky. Treat them like
untrusted code until reviewed.

## Trust Lifecycle

Installed marketplace skills/plugins now use a trust gate:

- `installed` - repo is on disk
- `trusted` - you explicitly marked the skill as reviewed
- `enabled` - skill is active for agent use

You cannot enable untrusted skills.

Portable bundle imports add one more step:

- `quarantined` - imported from a portable bundle and blocked from enable until
  explicitly cleared

Imported bundles keep provenance metadata (original source, commit SHA when
available, bundle path, export time) so you can review where they came from
before clearing quarantine.

The Skills page exposes these bundle flows directly:

- import a `.tar.gz` bundle from disk
- export an installed repo back to a portable bundle
- clear quarantine after reviewing provenance and contents

## Provenance Pinning

Chelix records a pinned `commit_sha` for installed repos:

- via `git rev-parse HEAD` after clone
- via GitHub commits API for tarball fallback installs

The Skills UI shows a short SHA to help review provenance.

## Re-Trust on Drift

If local repo HEAD changes from the pinned `commit_sha`:

- all skills in that repo are auto-marked `trusted=false`
- all skills in that repo are auto-disabled
- re-enable is blocked until explicit trust is granted again

The UI/API mark this state as `source changed`.

## Self-Contained Skill Folders

Chelix does not install system dependencies for skills. Each skill folder must
contain its own instructions and every script or binary it needs.

Legacy environment metadata is accepted for skill-format compatibility,
including top-level `requires` and `dockerfile` fields and namespaced `requires`
or `install` metadata. Chelix does not execute or convert this metadata into
dependency installation or sandbox provisioning.

Repository, folder, and portable-bundle installation remain available; these
flows install the skill folder itself and preserve the trust and provenance
lifecycle described above.

## Emergency Kill Switch

Use `skills.emergency_disable` to disable all installed third-party skills and
plugins immediately.

- Available in RPC and Skills UI action button
- Intended for incident response and containment

## Security Audit Log

Security-sensitive skill/plugin actions are appended to:

`~/.chelix/logs/security-audit.jsonl`

Logged events include installs, removals, trust changes, enable/disable, and
source drift detection.

## Recommended Production Policy

1. Keep sandbox enabled (`sandbox.mode = "On"`).
2. Keep approval mode at least `on-miss`.
3. Review SKILL.md and linked scripts before trust.
4. Prefer pinned, known repos over ad-hoc installs.
5. Monitor `security-audit.jsonl` for unusual events.
6. Keep imported bundles quarantined until you review their contents locally.
